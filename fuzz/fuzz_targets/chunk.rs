//! Chunk frames on data streams: sizes, compression, zip bombs.
#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use portal_core::Hash;
use portal_core::chunk::MAX_CHUNK_SIZE;
use portal_proto::EncodedChunk;

fuzz_target!(|data: &[u8]| {
    // Our own encoding always decodes to the original.
    let encoded = EncodedChunk::compress(Hash::of(data), data.to_vec());
    let (_, raw) = encoded.into_raw().expect("our own chunk decodes");
    assert_eq!(raw, data);

    // A peer's frames never decode to more than the chunk limit.
    let mut input = data;
    while let Ok(Some(chunk)) = common::now(EncodedChunk::read(&mut input)) {
        let raw_len = chunk.raw_len as usize;
        if let Ok((_, raw)) = chunk.into_raw() {
            assert_eq!(raw.len(), raw_len);
            assert!(raw.len() <= MAX_CHUNK_SIZE as usize);
        }
    }
});
