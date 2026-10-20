//! Creating endpoints and describing how to reach them.

use std::time::Duration;

use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, TransportAddr};
use portal_proto::ALPN;

use crate::error::NetError;
use crate::event::Route;

/// How long a sender waits to reach a relay before sharing direct addresses only.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);

/// Which networks an endpoint uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetMode {
    /// Normal operation: all interfaces, n0's public relays for hole punching
    /// and fallback, and address lookup.
    Internet,
    /// 127.0.0.1 only, no relays, no lookups. For tests on one machine:
    /// needs no network and triggers no firewall prompts.
    Loopback,
}

/// Creates an endpoint. A sender (`listen = true`) accepts Portal connections.
pub async fn bind(mode: NetMode, listen: bool) -> Result<Endpoint, NetError> {
    let builder = match mode {
        NetMode::Internet => Endpoint::builder(presets::N0),
        NetMode::Loopback => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .map_err(NetError::bind)?,
    };
    let alpns = if listen { vec![ALPN.to_vec()] } else { vec![] };
    builder.alpns(alpns).bind().await.map_err(NetError::bind)
}

/// The address a receiver needs to reach this endpoint: its node ID plus
/// relay and IP addresses.
pub async fn local_addr(endpoint: &Endpoint, mode: NetMode) -> EndpointAddr {
    let addr = match mode {
        NetMode::Internet => {
            if tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online())
                .await
                .is_err()
            {
                tracing::warn!("no relay reachable; sharing direct addresses only");
            }
            endpoint.addr()
        }
        NetMode::Loopback => EndpointAddr::from_parts(
            endpoint.id(),
            endpoint.bound_sockets().into_iter().map(TransportAddr::Ip),
        ),
    };
    let addrs = addr
        .addrs
        .into_iter()
        .filter(|a| matches!(a, TransportAddr::Ip(_) | TransportAddr::Relay(_)));
    EndpointAddr::from_parts(addr.id, addrs)
}

/// How `conn` is currently routed.
pub fn route(conn: &Connection) -> Route {
    conn.paths()
        .iter()
        .find(|p| p.is_selected())
        .map_or(Route::Unknown, |p| match p.remote_addr() {
            TransportAddr::Ip(addr) => Route::Direct(*addr),
            _ => Route::Relay,
        })
}
