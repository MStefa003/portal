//! Messages between a Portal client and the rendezvous server.
//!
//! Each WebSocket binary message carries one postcard-encoded message. The
//! server pairs the two clients that use the same nameplate, then forwards a
//! few small opaque [`ClientMsg::Relay`] payloads between them. Those payloads
//! are the [`crate::pake`] handshake: the server cannot read or change them
//! without the code words.
//!
//! As in [`crate::Control`], `Hello`/`Welcome` are variant 0 forever and new
//! variants are only appended.

use serde::{Deserialize, Serialize};

/// Version of the client/server rendezvous protocol.
pub const RENDEZVOUS_VERSION: u16 = 1;

/// Oldest rendezvous protocol version this build still speaks.
pub const MIN_RENDEZVOUS_VERSION: u16 = 1;

/// URL path of the rendezvous WebSocket endpoint, part of the server URL.
pub const RENDEZVOUS_PATH: &str = "/v1";

/// Maximum size of one WebSocket message in either direction.
pub const MAX_RENDEZVOUS_MESSAGE: usize = 8 * 1024;

/// Maximum size of one relayed payload.
pub const MAX_RELAY_PAYLOAD: usize = 4 * 1024;

/// Maximum number of payloads one client may relay per pairing.
pub const MAX_RELAYED_MESSAGES: usize = 8;

/// Client to server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message: the versions this client speaks.
    Hello {
        /// Newest supported version.
        version: u16,
        /// Oldest supported version.
        min_version: u16,
    },
    /// Sender: reserve a nameplate and wait for a receiver.
    Allocate,
    /// Receiver: join the sender waiting on `nameplate`.
    Claim {
        /// From the code the receiver typed.
        nameplate: u32,
    },
    /// Forward this payload to the paired peer.
    Relay(Vec<u8>),
}

/// Server to client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Reply to `Hello` when the versions are compatible.
    Welcome {
        /// Newest version the server speaks.
        version: u16,
        /// Oldest version the server speaks.
        min_version: u16,
    },
    /// Reply to `Allocate`.
    Allocated {
        /// Put this number at the front of the code.
        nameplate: u32,
    },
    /// The other peer is connected; relaying is open.
    Paired,
    /// A payload from the paired peer.
    Relay(Vec<u8>),
    /// The paired peer disconnected.
    PeerLeft,
    /// The request failed; the server closes the connection after this.
    Error(RendezvousError),
}

/// Why the server refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RendezvousError {
    /// The client's protocol version is not supported.
    #[error("the rendezvous server does not support this version of Portal")]
    Version,
    /// Nobody is waiting on that nameplate (wrong number, expired, or taken).
    #[error("no sender is waiting for this code; check the number at the start")]
    UnknownNameplate,
    /// The server is at capacity.
    #[error("the rendezvous server is busy; try again in a minute")]
    Busy,
    /// The nameplate expired before a receiver arrived.
    #[error("the code expired before anyone used it")]
    Expired,
    /// The client broke the protocol or exceeded a limit.
    #[error("the rendezvous server rejected a malformed request")]
    Protocol,
}

/// Whether a peer that speaks `version` down to `min_version` can talk to us
/// (`ours` down to `our_min`).
pub fn compatible(version: u16, min_version: u16, ours: u16, our_min: u16) -> bool {
    version >= our_min && min_version <= ours
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_welcome_are_variant_zero() {
        let hello = postcard::to_allocvec(&ClientMsg::Hello {
            version: 1,
            min_version: 1,
        })
        .unwrap();
        let welcome = postcard::to_allocvec(&ServerMsg::Welcome {
            version: 1,
            min_version: 1,
        })
        .unwrap();
        assert_eq!(hello[0], 0);
        assert_eq!(welcome[0], 0);
    }

    #[test]
    fn largest_relay_fits_a_message() {
        let msg = ClientMsg::Relay(vec![0xff; MAX_RELAY_PAYLOAD]);
        assert!(postcard::to_allocvec(&msg).unwrap().len() <= MAX_RENDEZVOUS_MESSAGE);
        let msg = ServerMsg::Relay(vec![0xff; MAX_RELAY_PAYLOAD]);
        assert!(postcard::to_allocvec(&msg).unwrap().len() <= MAX_RENDEZVOUS_MESSAGE);
    }

    #[test]
    fn compatibility() {
        assert!(compatible(1, 1, 1, 1));
        assert!(compatible(3, 1, 1, 1));
        assert!(!compatible(3, 2, 1, 1));
        assert!(!compatible(1, 1, 3, 2));
    }
}
