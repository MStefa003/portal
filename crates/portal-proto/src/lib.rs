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
//!
//! Before that, the peers find each other with a short [`Code`], in one of
//! two ways:
//! - **direct** (the default): the receiver finds the sender by the code's
//!   number on the local network or the DHT, connects with [`PAIR_ALPN`], and
//!   the [`pake`] handshake runs on the connection's first stream;
//! - **rendezvous**: a [`rendezvous`] server pairs them by the code's number
//!   and relays the [`pake`] handshake, which exchanges their addresses; the
//!   receiver then connects with [`ALPN`].
//!
//! See `docs/PROTOCOL.md` for the full description.
#![forbid(unsafe_code)]

pub mod chunk;
pub mod code;
pub mod frame;
pub mod message;
pub mod pake;
pub mod rendezvous;
pub mod version;

pub use chunk::{ChunkError, EncodedChunk, Encoding};
pub use code::{Code, CodeError};
pub use frame::FrameError;
pub use message::{Control, DeclineReason, Device, ErrorCode, Offer, sanitize};
pub use pake::{PakeError, PeerMsg, Role};
pub use rendezvous::{ClientMsg, RendezvousError, ServerMsg};
pub use version::{VersionError, negotiate};

/// Version of the peer-to-peer wire protocol spoken by this build.
///
/// - 1: one file or folder per transfer.
/// - 2: adds [`Control::Text`] and manifests with several top-level items.
pub const PROTOCOL_VERSION: u16 = 2;

/// Oldest protocol version this build can still talk to.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// QUIC application protocol identifier (ALPN) for Portal transfers whose
/// peers were already paired by a rendezvous server.
pub const ALPN: &[u8] = b"portal/1";

/// ALPN for direct connections: the first bidirectional stream carries the
/// [`pake`] handshake, then the transfer proceeds as with [`ALPN`].
pub const PAIR_ALPN: &[u8] = b"portal-pair/1";

/// ALPN for pairing two devices so they can later send to each other
/// without a code: the [`pake`] handshake as with [`PAIR_ALPN`], then each
/// side sends its [`Device`] description.
pub const TRUST_ALPN: &[u8] = b"portal-trust/1";

/// Maximum size of one control-stream frame.
pub const MAX_CONTROL_FRAME: usize = 1024 * 1024;

/// Size of the manifest pieces sent in [`Control::ManifestPart`].
pub const MANIFEST_PART_SIZE: usize = 512 * 1024;

/// Maximum number of hashes in one [`Control::Want`] (keeps it under the frame cap).
pub const WANT_BATCH: usize = 8 * 1024;

/// Maximum size of a text message ([`Control::Text`]) in bytes.
pub const MAX_TEXT: usize = 256 * 1024;

/// Maximum length of a device name ([`Device`]) in bytes.
pub const MAX_DEVICE_NAME: usize = 64;

/// Maximum length of a peer-supplied error message that we keep or display.
pub const MAX_ERROR_MESSAGE: usize = 1024;
