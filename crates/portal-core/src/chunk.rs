//! Content-defined chunking.
//!
//! Files are cut where the *content* says so (FastCDC's rolling gear hash),
//! not at fixed offsets. Inserting a byte near the start of a file only
//! changes the chunk around the insertion; every later chunk keeps its
//! boundaries and therefore its BLAKE3 hash. That is what lets a re-send of a
//! modified folder transfer only the chunks that actually changed.

use std::io::{self, Read};

use fastcdc::v2020::{self, StreamCDC};
use serde::{Deserialize, Serialize};

use crate::hash::Hash;

/// Smallest chunk FastCDC will cut (except the last chunk of a file).
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024;
/// Target average chunk size.
pub const AVG_CHUNK_SIZE: u32 = 256 * 1024;
/// Largest chunk FastCDC will cut. Also the cap on chunks received from the network.
pub const MAX_CHUNK_SIZE: u32 = 1024 * 1024;

// `StreamCDC::new` asserts that its sizes are within these bounds. Checking at
// compile time means that assertion can never fire at runtime.
const _: () = {
    assert!(MIN_CHUNK_SIZE as usize >= v2020::MINIMUM_MIN);
    assert!(MIN_CHUNK_SIZE as usize <= v2020::MINIMUM_MAX);
    assert!(AVG_CHUNK_SIZE as usize >= v2020::AVERAGE_MIN);
    assert!(AVG_CHUNK_SIZE as usize <= v2020::AVERAGE_MAX);
    assert!(MAX_CHUNK_SIZE as usize >= v2020::MAXIMUM_MIN);
    assert!(MAX_CHUNK_SIZE as usize <= v2020::MAXIMUM_MAX);
};

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
/// Computes each chunk's hash and the whole-content hash in one pass.
pub fn chunk_reader<R: Read>(reader: R) -> io::Result<ChunkedFile> {
    let cdc = StreamCDC::new(
        reader,
        MIN_CHUNK_SIZE as usize,
        AVG_CHUNK_SIZE as usize,
        MAX_CHUNK_SIZE as usize,
    );
    let mut whole = blake3::Hasher::new();
    let mut chunks = Vec::new();
    let mut size = 0u64;
    for chunk in cdc {
        let chunk = chunk.map_err(io::Error::from)?;
        let len = u32::try_from(chunk.data.len())
            .map_err(|_| io::Error::other("chunker produced an oversized chunk"))?;
        whole.update(&chunk.data);
        chunks.push(ChunkRef {
            hash: Hash::of(&chunk.data),
            len,
        });
        size += u64::from(len);
    }
    Ok(ChunkedFile {
        size,
        hash: whole.finalize().into(),
        chunks,
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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn invariants_hold_for_any_length(seed in any::<u64>(), len in 0usize..3_000_000) {
            let data = random_bytes(seed, len);
            check_invariants(&data, &chunk_reader(&data[..]).unwrap());
        }
    }
}
