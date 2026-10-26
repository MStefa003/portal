//! Creating endpoints and describing how to reach them.

use std::time::Duration;

use iroh::endpoint::{Connection, QuicTransportConfig, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use portal_proto::{ALPN, PAIR_ALPN, TRUST_ALPN};

use crate::error::NetError;
use crate::event::Route;

/// How long a sender waits to reach a relay before sharing direct addresses only.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes one stream may have in flight. Kept small enough that lost packets
/// cannot leave more holes in one stream than the QUIC library will buffer
/// (1024; past that it drops the connection): 2 MiB is under 1,800 packets
/// even at the smallest packet size, so at most ~900 holes.
const STREAM_WINDOW: u32 = 2 * 1024 * 1024;
/// Bytes all streams of a connection may have in flight together; also
/// bounds the memory a peer can make us buffer. With every data stream of a
/// transfer busy this fills about 2.5 Gbit/s at 100 ms.
const CONNECTION_WINDOW: u32 = crate::send::DATA_STREAMS as u32 * STREAM_WINDOW;

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

/// Creates an endpoint with a new random identity. A sender (`listen =
/// true`) accepts Portal connections: direct ([`PAIR_ALPN`]), paired by a
/// rendezvous server or trusted ([`ALPN`]), and device pairing ([`TRUST_ALPN`]).
pub async fn bind(mode: NetMode, listen: bool) -> Result<Endpoint, NetError> {
    bind_as(mode, listen, None).await
}

/// Like [`bind`], with the identity `key` if given: a device's long-term
/// key, so that devices it was paired with recognize it.
pub async fn bind_as(
    mode: NetMode,
    listen: bool,
    key: Option<SecretKey>,
) -> Result<Endpoint, NetError> {
    let builder = match mode {
        NetMode::Internet => Endpoint::builder(presets::N0),
        NetMode::Loopback => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .map_err(NetError::bind)?,
    };
    let alpns = if listen {
        vec![PAIR_ALPN.to_vec(), ALPN.to_vec(), TRUST_ALPN.to_vec()]
    } else {
        vec![]
    };
    let builder = match key {
        Some(key) => builder.secret_key(key),
        None => builder,
    };
    let transport = QuicTransportConfig::builder()
        .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
        .receive_window(VarInt::from_u32(CONNECTION_WINDOW))
        .send_window(u64::from(CONNECTION_WINDOW))
        .build();
    let builder = builder.transport_config(transport);
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
