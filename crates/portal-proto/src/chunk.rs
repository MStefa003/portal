//! Chunk frames on data streams, with optional zstd compression.
//!
//! Layout of one chunk on a data stream:
//!
//! ```text
//! [u32 LE header length][postcard ChunkHeader][payload: header.wire_len bytes]
//! ```
//!
//! The payload is carried outside postcard so a 1 MiB chunk is copied once,
//! not serialized byte by byte.

use portal_core::Hash;
use portal_core::chunk::MAX_CHUNK_SIZE;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::frame::{self, FrameError, read_exact_or_truncated};

/// zstd level: fast, with most of the gain on compressible data.
const ZSTD_LEVEL: i32 = 3;
/// Upper bound on an encoded header (hash + small integers).
const MAX_HEADER: usize = 64;

/// How a chunk's payload is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// The raw bytes.
    Raw,
    /// zstd-compressed bytes.
    Zstd,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChunkHeader {
    hash: Hash,
    encoding: Encoding,
    raw_len: u32,
    wire_len: u32,
}

/// Why a received chunk could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    /// Reading the frame failed.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The announced sizes exceed the chunk limit.
    #[error("the other side sent an oversized chunk")]
    TooLarge,
    /// Decompression failed or produced the wrong length.
    #[error("the other side sent a chunk that could not be decompressed")]
    BadCompression,
}

/// A chunk as it travels on a data stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedChunk {
    /// BLAKE3 hash of the *raw* bytes; verified by the receiver after decoding.
    pub hash: Hash,
    /// Encoding of `payload`.
    pub encoding: Encoding,
    /// Length of the raw bytes.
    pub raw_len: u32,
    /// The bytes on the wire.
    pub payload: Vec<u8>,
}

impl EncodedChunk {
    /// Encodes `raw`, using zstd only if it actually makes the chunk smaller
    /// (already-compressed data such as JPEG or ZIP is sent as is).
    pub fn compress(hash: Hash, raw: Vec<u8>) -> Self {
        // A chunk never exceeds MAX_CHUNK_SIZE (u32), so this cannot saturate in practice.
        let raw_len = u32::try_from(raw.len()).unwrap_or(u32::MAX);
        match zstd::bulk::compress(&raw, ZSTD_LEVEL) {
            Ok(compressed) if compressed.len() < raw.len() => Self {
                hash,
                encoding: Encoding::Zstd,
                raw_len,
                payload: compressed,
            },
            _ => Self::raw(hash, raw),
        }
    }

    /// Sends `raw` as is, without trying to compress it.
    pub fn raw(hash: Hash, raw: Vec<u8>) -> Self {
        Self {
            hash,
            encoding: Encoding::Raw,
            raw_len: u32::try_from(raw.len()).unwrap_or(u32::MAX),
            payload: raw,
        }
    }

    /// Decodes the payload back into raw bytes, never producing more than
    /// [`MAX_CHUNK_SIZE`] bytes (zip-bomb safe). The caller still has to
    /// verify the hash.
    pub fn into_raw(self) -> Result<(Hash, Vec<u8>), ChunkError> {
        let raw_len = self.raw_len as usize;
        if raw_len > MAX_CHUNK_SIZE as usize {
            return Err(ChunkError::TooLarge);
        }
        let raw = match self.encoding {
            Encoding::Raw => self.payload,
            // `decompress` fails if the output would exceed `raw_len`.
            Encoding::Zstd => zstd::bulk::decompress(&self.payload, raw_len)
                .map_err(|_| ChunkError::BadCompression)?,
        };
        if raw.len() != raw_len {
            return Err(ChunkError::BadCompression);
        }
        Ok((self.hash, raw))
    }

    /// Writes this chunk to a data stream.
    pub async fn write<W: AsyncWrite + Unpin>(&self, w: &mut W) -> Result<(), FrameError> {
        let header = ChunkHeader {
            hash: self.hash,
            encoding: self.encoding,
            raw_len: self.raw_len,
            wire_len: u32::try_from(self.payload.len()).map_err(|_| FrameError::TooLarge {
                len: self.payload.len(),
                max: MAX_CHUNK_SIZE as usize,
            })?,
        };
        frame::write_msg(w, &header).await?;
        w.write_all(&self.payload).await?;
        Ok(())
    }

    /// Reads the next chunk from a data stream. `Ok(None)` at a clean end of stream.
    pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Self>, ChunkError> {
        let Some(header) = frame::read_msg::<_, ChunkHeader>(r, MAX_HEADER).await? else {
            return Ok(None);
        };
        if header.wire_len > MAX_CHUNK_SIZE || header.raw_len > MAX_CHUNK_SIZE {
            return Err(ChunkError::TooLarge);
        }
        let mut payload = vec![0u8; header.wire_len as usize];
        read_exact_or_truncated(r, &mut payload).await?;
        Ok(Some(Self {
            hash: header.hash,
            encoding: header.encoding,
            raw_len: header.raw_len,
            payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressible_data_is_compressed() {
        let raw = b"portal ".repeat(10_000);
        let c = EncodedChunk::compress(Hash::of(&raw), raw.clone());
        assert_eq!(c.encoding, Encoding::Zstd);
        assert!(c.payload.len() < raw.len() / 10);
        assert_eq!(c.into_raw().unwrap().1, raw);
    }

    #[test]
    fn incompressible_data_is_sent_raw() {
        // Bytes from a hash chain look random to zstd.
        let mut raw = Vec::new();
        let mut h = Hash::of(b"seed");
        while raw.len() < 100_000 {
            h = Hash::of(h.as_bytes());
            raw.extend_from_slice(h.as_bytes());
        }
        let c = EncodedChunk::compress(Hash::of(&raw), raw.clone());
        assert_eq!(c.encoding, Encoding::Raw);
        assert_eq!(c.into_raw().unwrap().1, raw);
    }

    #[test]
    fn rejects_decompression_bombs_and_lies() {
        // 1 MiB of zeros compresses to a few bytes; claim it is only 10 bytes.
        let zeros = vec![0u8; MAX_CHUNK_SIZE as usize];
        let mut c = EncodedChunk::compress(Hash::of(&zeros), zeros);
        c.raw_len = 10;
        assert!(matches!(c.into_raw(), Err(ChunkError::BadCompression)));

        let c = EncodedChunk {
            hash: Hash::of(b""),
            encoding: Encoding::Raw,
            raw_len: MAX_CHUNK_SIZE + 1,
            payload: vec![],
        };
        assert!(matches!(c.into_raw(), Err(ChunkError::TooLarge)));

        let c = EncodedChunk {
            hash: Hash::of(b""),
            encoding: Encoding::Zstd,
            raw_len: 5,
            payload: b"not zstd".to_vec(),
        };
        assert!(matches!(c.into_raw(), Err(ChunkError::BadCompression)));
    }

    #[tokio::test]
    async fn stream_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4 * 1024 * 1024);
        let one = EncodedChunk::compress(Hash::of(b"1"), b"first chunk".to_vec());
        let two = EncodedChunk::compress(Hash::of(b"2"), vec![7u8; 300_000]);
        one.write(&mut a).await.unwrap();
        two.write(&mut a).await.unwrap();
        drop(a);
        assert_eq!(EncodedChunk::read(&mut b).await.unwrap(), Some(one));
        assert_eq!(EncodedChunk::read(&mut b).await.unwrap(), Some(two));
        assert_eq!(EncodedChunk::read(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn stream_rejects_oversized_header_claims() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let header = ChunkHeader {
            hash: Hash::of(b""),
            encoding: Encoding::Raw,
            raw_len: 1,
            wire_len: u32::MAX,
        };
        frame::write_msg(&mut a, &header).await.unwrap();
        assert!(matches!(
            EncodedChunk::read(&mut b).await,
            Err(ChunkError::TooLarge)
        ));
    }
}
