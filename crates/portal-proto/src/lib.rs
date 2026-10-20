//! Wire protocol for Portal: message types, versioning and (de)serialization.
//!
//! A transfer uses one QUIC connection with:
//! - one bidirectional **control stream**, opened by the receiver, carrying
//!   [`Control`] messages in both directions, and
//! - several unidirectional **data streams**, opened by the sender, each
//!   carrying a sequence of [`EncodedChunk`]s.
//!
//! Every message is a length-prefixed frame (see [`frame`]), and every length
//! read from the network is checked against a hard cap before allocating.
//! See `docs/PROTOCOL.md` for the full description.
#![forbid(unsafe_code)]

pub mod chunk;
pub mod frame;
pub mod message;
pub mod version;

pub use chunk::{ChunkError, EncodedChunk, Encoding};
pub use frame::FrameError;
pub use message::{Control, DeclineReason, ErrorCode, Offer, sanitize};
pub use version::{VersionError, negotiate};

/// Version of the peer-to-peer wire protocol spoken by this build.
pub const PROTOCOL_VERSION: u16 = 1;

/// Oldest protocol version this build can still talk to.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// QUIC application protocol identifier (ALPN) for Portal transfers.
pub const ALPN: &[u8] = b"portal/1";

/// Maximum size of one control-stream frame.
pub const MAX_CONTROL_FRAME: usize = 1024 * 1024;

/// Size of the manifest pieces sent in [`Control::ManifestPart`].
pub const MANIFEST_PART_SIZE: usize = 512 * 1024;

/// Maximum number of hashes in one [`Control::Want`] (keeps it under the frame cap).
pub const WANT_BATCH: usize = 8 * 1024;

/// Maximum length of a peer-supplied error message that we keep or display.
pub const MAX_ERROR_MESSAGE: usize = 1024;
