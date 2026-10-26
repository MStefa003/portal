//! Content-defined chunking.
//!
//! Files are cut where the *content* says so (FastCDC's rolling gear hash),
//! not at fixed offsets. Inserting a byte near the start of a file only
//! changes the chunk around the insertion; every later chunk keeps its
//! boundaries and therefore its BLAKE3 hash. That is what lets a re-send of a
//! modified folder transfer only the chunks that actually changed.

use std::io::{self, Read};
use std::ops::Range;
use std::sync::{Arc, mpsc};
use std::thread;

use fastcdc::v2020::{self, FastCDC};
use serde::{Deserialize, Serialize};

use crate::hash::Hash;

/// Smallest chunk FastCDC will cut (except the last chunk of a file).
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024;
/// Target average chunk size.
pub const AVG_CHUNK_SIZE: u32 = 256 * 1024;
/// Largest chunk FastCDC will cut. Also the cap on chunks received from the network.
pub const MAX_CHUNK_SIZE: u32 = 1024 * 1024;

// `FastCDC::new` asserts that its sizes are within these bounds. Checking at
// compile time means that assertion can never fire at runtime.
const _: () = {
    assert!(MIN_CHUNK_SIZE as usize >= v2020::MINIMUM_MIN);
    assert!(MIN_CHUNK_SIZE as usize <= v2020::MINIMUM_MAX);
    assert!(AVG_CHUNK_SIZE as usize >= v2020::AVERAGE_MIN);
    assert!(AVG_CHUNK_SIZE as usize <= v2020::AVERAGE_MAX);
    assert!(MAX_CHUNK_SIZE as usize >= v2020::MAXIMUM_MIN);
    assert!(MAX_CHUNK_SIZE as usize <= v2020::MAXIMUM_MAX);
    // Every block then holds a whole maximum chunk past the carried-over
    // tail, so each block but the last yields at least one cut.
    assert!(FIRST_BLOCK >= 2 * MAX_CHUNK_SIZE as usize);
    assert!(BLOCK >= FIRST_BLOCK);
};

/// Bytes read from a file at a time. Blocks are reused, so reading a large
/// file allocates (and page-faults in) only a few of them.
const BLOCK: usize = 4 * 1024 * 1024;
/// The first block is smaller, so that small files need only a small buffer.
const FIRST_BLOCK: usize = 2 * MAX_CHUNK_SIZE as usize;
/// Blocks waiting for each hashing thread.
const HASH_QUEUE: usize = 2;

/// One chunk of a file: its content hash and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    /// BLAKE3 hash of the chunk's bytes.
    pub hash: Hash,
    /// Length in bytes, between 1 and [`MAX_CHUNK_SIZE`].
    pub len: u32,
}

/// The result of chunking a whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedFile {
    /// Total number of bytes read.
    pub size: u64,
    /// BLAKE3 hash of the whole content.
    pub hash: Hash,
    /// The chunks, in order. Empty for an empty file.
    pub chunks: Vec<ChunkRef>,
}

/// Reads `reader` to the end, splitting it into content-defined chunks.
///
/// The cuts are exactly those of FastCDC's `StreamCDC`, found without its
/// per-chunk copying. For files of more than one chunk, the chunk hashes and
/// the whole-content hash are computed on two more threads while this one
/// reads and cuts.
pub fn chunk_reader<R: Read>(reader: R) -> io::Result<ChunkedFile> {
    chunk_reader_with(reader, |_| {})
}

/// Like [`chunk_reader`], calling `progress` with the number of bytes cut
/// each time a block of the file has been read.
pub fn chunk_reader_with<R: Read>(
    reader: R,
    mut progress: impl FnMut(u64),
) -> io::Result<ChunkedFile> {
    let mut cutter = Cutter::new(reader);
    let Some(first) = cutter.next_block()? else {
        return Ok(ChunkedFile {
            size: 0,
            hash: Hash::of(b""),
            chunks: Vec::new(),
        });
    };
    progress(first.len());
    if let [cut] = &first.cuts[..]
        && cutter.is_done()
    {
        // One chunk: its hash is the hash of the whole content.
        let chunk = chunk_ref(&first.data[cut.clone()])?;
        return Ok(ChunkedFile {
            size: u64::from(chunk.len),
            hash: chunk.hash,
            chunks: vec![chunk],
        });
    }

    let (chunk_tx, chunk_rx) = mpsc::sync_channel::<Block>(HASH_QUEUE);
    let (whole_tx, whole_rx) = mpsc::sync_channel::<Block>(HASH_QUEUE);
    let recycle = cutter.recycler();
    let recycle_too = recycle.clone();
    thread::scope(|scope| {
        let chunk_hasher = scope.spawn(move || {
            let mut chunks = Vec::new();
            for block in chunk_rx {
                for cut in &block.cuts {
                    chunks.push(chunk_ref(&block.data[cut.clone()])?);
                }
                block.recycle(&recycle);
            }
            Ok::<_, io::Error>(chunks)
        });
        let whole_hasher = scope.spawn(move || {
            let mut hasher = blake3::Hasher::new();
            for block in whole_rx {
                // The cuts of a block are contiguous.
                if let (Some(first), Some(last)) = (block.cuts.first(), block.cuts.last()) {
                    hasher.update(&block.data[first.start..last.end]);
                }
                block.recycle(&recycle_too);
            }
            Hash::from(hasher.finalize())
        });
        let mut size = 0;
        let mut next = Some(first);
        while let Some(block) = next {
            size += block.len();
            // A send fails only if a hashing thread is gone; `join` reports that.
            let _ = chunk_tx.send(block.clone());
            let _ = whole_tx.send(block);
            // On error, dropping the senders lets both threads finish.
            next = cutter.next_block()?;
            if let Some(block) = &next {
                progress(block.len());
            }
        }
        drop((chunk_tx, whole_tx));
        let failed = || io::Error::other("a hashing thread failed");
        let chunks = chunk_hasher.join().map_err(|_| failed())??;
        let hash = whole_hasher.join().map_err(|_| failed())?;
        Ok(ChunkedFile { size, hash, chunks })
    })
}

/// Part of a file read into memory, with the chunks cut from it.
#[derive(Clone)]
struct Block {
    data: Arc<Vec<u8>>,
    /// Byte ranges of `data`, contiguous and in order.
    cuts: Vec<Range<usize>>,
}

impl Block {
    fn len(&self) -> u64 {
        self.cuts.iter().map(|c| c.len() as u64).sum()
    }

    /// Hands the buffer back to the cutter once nothing else uses it.
    fn recycle(self, to: &mpsc::Sender<Vec<u8>>) {
        if let Some(data) = Arc::into_inner(self.data) {
            let _ = to.send(data);
        }
    }
}

/// Reads a file block by block and finds FastCDC's cut points in it.
struct Cutter<R> {
    reader: R,
    /// The last block and its uncut tail, which starts the next block.
    last: Option<(Arc<Vec<u8>>, Range<usize>)>,
    eof: bool,
    free_tx: mpsc::Sender<Vec<u8>>,
    free_rx: mpsc::Receiver<Vec<u8>>,
}

impl<R: Read> Cutter<R> {
    fn new(reader: R) -> Self {
        let (free_tx, free_rx) = mpsc::channel();
        Self {
            reader,
            last: None,
            eof: false,
            free_tx,
            free_rx,
        }
    }

    /// Where blocks are handed back for reuse.
    fn recycler(&self) -> mpsc::Sender<Vec<u8>> {
        self.free_tx.clone()
    }

    /// Whether every byte has been read and cut.
    fn is_done(&self) -> bool {
        self.eof && self.last.as_ref().is_none_or(|(_, tail)| tail.is_empty())
    }

    /// Reads the next block and cuts it; `None` at the end. The bytes after
    /// the last cut are carried over to the next block.
    fn next_block(&mut self) -> io::Result<Option<Block>> {
        if self.is_done() {
            return Ok(None);
        }
        // Never wait for a block to come back: the bounded queues to the
        // hashing threads already limit how many are in use.
        let mut buf = self.free_rx.try_recv().unwrap_or_default();
        let size = if self.last.is_none() {
            FIRST_BLOCK
        } else {
            BLOCK
        };
        buf.resize(size, 0);
        let mut end = 0;
        if let Some((data, tail)) = self.last.take() {
            buf[..tail.len()].copy_from_slice(&data[tail.clone()]);
            end = tail.len();
            Block { data, cuts: vec![] }.recycle(&self.free_tx);
        }
        while !self.eof && end < buf.len() {
            match self.reader.read(&mut buf[end..]) {
                Ok(0) => self.eof = true,
                Ok(n) => end += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }

        let cdc = FastCDC::new(
            &buf[..end],
            MIN_CHUNK_SIZE as usize,
            AVG_CHUNK_SIZE as usize,
            MAX_CHUNK_SIZE as usize,
        );
        let mut cuts = Vec::new();
        let mut start = 0;
        // FastCDC looks at most one maximum chunk ahead; it must see that
        // much (or everything that is left) for its cut to be final.
        while start < end && (self.eof || end - start >= MAX_CHUNK_SIZE as usize) {
            let (_, cut) = cdc.cut(start, end - start);
            cuts.push(start..cut);
            start = cut;
        }
        let data = Arc::new(buf);
        self.last = Some((data.clone(), start..end));
        Ok(Some(Block { data, cuts }))
    }
}

fn chunk_ref(data: &[u8]) -> io::Result<ChunkRef> {
    let len = u32::try_from(data.len())
        .map_err(|_| io::Error::other("chunker produced an oversized chunk"))?;
    Ok(ChunkRef {
        hash: Hash::of(data),
        len,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Deterministic pseudo-random bytes (xorshift64*), so tests are reproducible.
    pub(crate) fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Checks the structural invariants of a chunking of `data`.
    fn check_invariants(data: &[u8], chunked: &ChunkedFile) {
        assert_eq!(chunked.size, data.len() as u64);
        assert_eq!(chunked.hash, Hash::of(data));
        let mut offset = 0usize;
        for (i, c) in chunked.chunks.iter().enumerate() {
            let len = c.len as usize;
            assert!(len >= 1 && len <= MAX_CHUNK_SIZE as usize);
            if i + 1 < chunked.chunks.len() {
                assert!(
                    len >= MIN_CHUNK_SIZE as usize,
                    "only the last chunk may be short"
                );
            }
            assert_eq!(c.hash, Hash::of(&data[offset..offset + len]));
            offset += len;
        }
        assert_eq!(offset, data.len());
    }

    #[test]
    fn empty_input_has_no_chunks() {
        let c = chunk_reader(&b""[..]).unwrap();
        assert!(c.chunks.is_empty());
        assert_eq!(c.size, 0);
        assert_eq!(c.hash, Hash::of(b""));
    }

    #[test]
    fn small_input_is_one_chunk() {
        let data = b"hello portal";
        let c = chunk_reader(&data[..]).unwrap();
        assert_eq!(c.chunks.len(), 1);
        check_invariants(data, &c);
    }

    #[test]
    fn large_input_satisfies_invariants() {
        let data = random_bytes(1, 5 * 1024 * 1024 + 123);
        let c = chunk_reader(&data[..]).unwrap();
        assert!(
            c.chunks.len() > 5,
            "expected many chunks, got {}",
            c.chunks.len()
        );
        check_invariants(&data, &c);
    }

    #[test]
    fn chunking_is_deterministic() {
        let data = random_bytes(2, 3 * 1024 * 1024);
        assert_eq!(
            chunk_reader(&data[..]).unwrap(),
            chunk_reader(&data[..]).unwrap()
        );
    }

    #[test]
    fn insertion_only_changes_nearby_chunks() {
        let original = random_bytes(3, 8 * 1024 * 1024);
        let mut edited = original.clone();
        edited.splice(100_000..100_000, *b"a few inserted bytes");

        let a = chunk_reader(&original[..]).unwrap();
        let b = chunk_reader(&edited[..]).unwrap();
        let shared = b.chunks.iter().filter(|c| a.chunks.contains(c)).count();
        assert!(
            shared + 2 >= a.chunks.len(),
            "only the edited chunk(s) should differ: {shared} of {} shared",
            a.chunks.len()
        );
    }

    /// Cuts the way earlier versions did, with FastCDC's own stream chunker.
    fn stream_cdc_cuts(data: &[u8]) -> Vec<ChunkRef> {
        fastcdc::v2020::StreamCDC::new(
            data,
            MIN_CHUNK_SIZE as usize,
            AVG_CHUNK_SIZE as usize,
            MAX_CHUNK_SIZE as usize,
        )
        .map(|c| chunk_ref(&c.unwrap().data).unwrap())
        .collect()
    }

    /// A reader that returns at most `step` bytes per call, like a pipe.
    struct Trickle<'a> {
        data: &'a [u8],
        step: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.step).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    #[test]
    fn cuts_match_stream_cdc_across_buffer_refills() {
        // Several refills of the read buffer, with a partial last one.
        let data = random_bytes(4, 5 * BLOCK + 777_777);
        let expected = stream_cdc_cuts(&data);
        assert_eq!(chunk_reader(&data[..]).unwrap().chunks, expected);
        let trickle = Trickle {
            data: &data,
            step: 100_003,
        };
        assert_eq!(chunk_reader(trickle).unwrap().chunks, expected);
    }

    #[test]
    fn progress_adds_up_to_the_size() {
        let data = random_bytes(5, 2_500_000);
        let mut seen = 0;
        let c = chunk_reader_with(&data[..], |n| seen += n).unwrap();
        assert_eq!(seen, c.size);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn invariants_hold_for_any_length(seed in any::<u64>(), len in 0usize..3_000_000) {
            let data = random_bytes(seed, len);
            let chunked = chunk_reader(&data[..]).unwrap();
            check_invariants(&data, &chunked);
            prop_assert_eq!(chunked.chunks, stream_cdc_cuts(&data));
        }
    }
}
