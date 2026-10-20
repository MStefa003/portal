//! Wire protocol for Portal: message types, versioning and (de)serialization.
//!
//! See `docs/PROTOCOL.md` for the human-readable description of the format.
#![forbid(unsafe_code)]

/// Version of the peer-to-peer wire protocol spoken by this build.
///
/// Peers exchange this value first; incompatible versions fail with a clear
/// error instead of misinterpreting each other's messages.
pub const PROTOCOL_VERSION: u16 = 1;
