use std::net::SocketAddr;
use std::sync::Arc;

use portal_proto::Offer;

/// How the two peers are connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// A direct UDP path to this address (LAN or hole-punched).
    Direct(SocketAddr),
    /// Through an encrypted relay server.
    Relay,
    /// Not known yet.
    Unknown,
}

/// Something worth showing to the user.
#[derive(Debug, Clone)]
pub enum Event {
    /// A peer connected.
    Connected {
        /// Short form of the peer's node ID.
        peer: String,
        /// How the connection is routed.
        route: Route,
    },
    /// The receiver got the sender's offer (receiver side only).
    Offer(Offer),
    /// Data is about to flow.
    Started {
        /// Bytes that will cross the network.
        total_bytes: u64,
        /// Bytes that do not need sending (duplicate chunks; resumed data in M4).
        skipped_bytes: u64,
    },
    /// More chunk data was sent or received.
    Progress {
        /// Uncompressed bytes in this step.
        bytes: u64,
    },
}

/// Callback receiving [`Event`]s. Called from several tasks; must be cheap.
pub type Reporter = Arc<dyn Fn(Event) + Send + Sync>;
