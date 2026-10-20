//! BLAKE3 content hashes.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A 32-byte BLAKE3 hash identifying a chunk, a whole file or a manifest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Hashes `data` with BLAKE3.
    pub fn of(data: &[u8]) -> Self {
        blake3::hash(data).into()
    }

    /// Returns the raw hash bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex of the first `n` bytes (at most 32), for short names.
    pub fn to_hex_prefix(&self, n: usize) -> String {
        self.0.iter().take(n).map(|b| format!("{b:02x}")).collect()
    }
}

impl From<blake3::Hash> for Hash {
    fn from(h: blake3::Hash) -> Self {
        Self(*h.as_bytes())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex_prefix(32))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({}…)", self.to_hex_prefix(6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_blake3_reference() {
        // BLAKE3 of the empty input, from the official test vectors.
        assert_eq!(
            Hash::of(b"").to_string(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn hex_prefix_is_truncated() {
        assert_eq!(Hash::of(b"").to_hex_prefix(2), "af13");
    }
}
