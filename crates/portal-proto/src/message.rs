//! Control-stream messages.

use std::fmt;

use portal_core::Hash;
use serde::{Deserialize, Serialize};

/// A message on the control stream.
///
/// **Compatibility rule:** `Hello` must stay variant 0 forever, so that any
/// two versions of Portal can at least read each other's `Hello` and fail
/// with a clear version error. New variants are only ever appended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// First message from each side: the protocol versions it supports.
    Hello {
        /// Newest supported protocol version.
        version: u16,
        /// Oldest supported protocol version.
        min_version: u16,
    },
    /// Sender → receiver: what is about to be sent. Followed by the manifest.
    Offer(Offer),
    /// Sender → receiver: the next piece of the encoded manifest.
    ManifestPart(Vec<u8>),
    /// Receiver → sender: go ahead.
    Accept,
    /// Receiver → sender: the transfer is refused.
    Decline(DeclineReason),
    /// Receiver → sender: please send these chunks.
    Want(Vec<Hash>),
    /// Receiver → sender: no more `Want`s will follow.
    WantDone,
    /// Receiver → sender: everything was received and verified.
    Done,
    /// Either side: the transfer failed.
    Error {
        /// Machine-readable category.
        code: ErrorCode,
        /// Human-readable details (untrusted; sanitize before display).
        message: String,
    },
}

/// Summary of a transfer, sent before the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// Top-level file or folder name.
    pub name: String,
    /// Number of files.
    pub files: u64,
    /// Total size of all files in bytes.
    pub bytes: u64,
    /// Length of the encoded manifest that follows.
    pub manifest_len: u64,
    /// BLAKE3 hash of the encoded manifest.
    pub transfer_id: Hash,
}

/// Why a receiver declined a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclineReason {
    /// The user said no.
    Rejected,
    /// Files already exist at the destination.
    Conflicts,
    /// The destination cannot be written to.
    Destination,
}

impl fmt::Display for DeclineReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rejected => "the receiver said no",
            Self::Conflicts => "files with the same names already exist on the receiver",
            Self::Destination => "the receiver cannot write to its destination folder",
        })
    }
}

/// Category of a [`Control::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Unexpected internal failure.
    Internal,
    /// The peer violated the protocol.
    Protocol,
    /// Received data failed verification.
    Corrupt,
    /// A source file changed while being sent.
    SourceChanged,
    /// A local file could not be read or written.
    Io,
}

/// Makes a peer-supplied string safe to print: control characters (which
/// could drive the terminal) are escaped and the length is capped.
pub fn sanitize(untrusted: &str) -> String {
    untrusted
        .chars()
        .take(crate::MAX_ERROR_MESSAGE)
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding is part of the protocol: changing it breaks compatibility.
    #[test]
    fn hello_encoding_is_stable() {
        let hello = Control::Hello {
            version: 1,
            min_version: 1,
        };
        assert_eq!(postcard::to_allocvec(&hello).unwrap(), [0, 1, 1]);
    }

    #[test]
    fn messages_roundtrip() {
        let msgs = [
            Control::Offer(Offer {
                name: "project".into(),
                files: 1284,
                bytes: 2_300_000_000,
                manifest_len: 42,
                transfer_id: Hash::of(b"m"),
            }),
            Control::ManifestPart(vec![1, 2, 3]),
            Control::Accept,
            Control::Decline(DeclineReason::Conflicts),
            Control::Want(vec![Hash::of(b"a"), Hash::of(b"b")]),
            Control::WantDone,
            Control::Done,
            Control::Error {
                code: ErrorCode::Corrupt,
                message: "bad".into(),
            },
        ];
        for msg in msgs {
            let bytes = postcard::to_allocvec(&msg).unwrap();
            assert_eq!(postcard::from_bytes::<Control>(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn full_want_batch_fits_in_a_frame() {
        let want = Control::Want(vec![Hash::of(b"x"); crate::WANT_BATCH]);
        assert!(postcard::to_allocvec(&want).unwrap().len() <= crate::MAX_CONTROL_FRAME);
    }

    proptest::proptest! {
        /// Arbitrary bytes from a peer either decode or fail; they never panic.
        #[test]
        fn decoding_arbitrary_bytes_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
            let _ = postcard::from_bytes::<Control>(&bytes);
        }
    }

    #[test]
    fn sanitize_escapes_terminal_control() {
        assert_eq!(sanitize("ok\x1b[2Jboom\n"), "ok\\u{1b}[2Jboom\\n");
        assert_eq!(sanitize(&"x".repeat(5000)).len(), crate::MAX_ERROR_MESSAGE);
    }
}
