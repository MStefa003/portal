//! Creating endpoints and describing how to reach them.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, TransportAddr};
use portal_proto::ALPN;

use crate::error::NetError;
use crate::event::Route;

/// How long a sender waits to reach a relay before sharing direct addresses only.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on the length of a ticket string we try to parse.
const MAX_TICKET_LEN: usize = 8 * 1024;

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

/// Builds the ticket a receiver needs to reach this endpoint.
pub async fn local_ticket(endpoint: &Endpoint, mode: NetMode) -> Ticket {
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
    Ticket::new(addr)
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

/// Everything needed to dial a peer: its node ID plus relay and IP addresses.
///
/// Shown as lowercase hex of its postcard encoding. Temporary for M2; M3
/// replaces it with short codes exchanged through the rendezvous server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket(EndpointAddr);

impl Ticket {
    /// Wraps an address, keeping only relay and IP transports.
    pub fn new(addr: EndpointAddr) -> Self {
        let addrs = addr
            .addrs
            .into_iter()
            .filter(|a| matches!(a, TransportAddr::Ip(_) | TransportAddr::Relay(_)));
        Self(EndpointAddr::from_parts(addr.id, addrs))
    }

    /// The address to dial.
    pub fn addr(&self) -> &EndpointAddr {
        &self.0
    }
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = postcard::to_allocvec(&self.0).map_err(|_| fmt::Error)?;
        for b in bytes {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Ticket {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() > MAX_TICKET_LEN || !s.len().is_multiple_of(2) || !s.is_ascii() {
            return Err(NetError::InvalidTicket);
        }
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| NetError::InvalidTicket)?;
        let addr: EndpointAddr =
            postcard::from_bytes(&bytes).map_err(|_| NetError::InvalidTicket)?;
        Ok(Self::new(addr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_roundtrip() {
        let id = iroh::SecretKey::generate().public();
        let addr = EndpointAddr::from_parts(
            id,
            [
                TransportAddr::Ip("192.168.1.20:4433".parse().unwrap()),
                TransportAddr::Ip("[fe80::1]:4433".parse().unwrap()),
            ],
        );
        let ticket = Ticket::new(addr);
        let text = ticket.to_string();
        assert!(text.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(text.parse::<Ticket>().unwrap(), ticket);
        assert_eq!(format!("  {text}\n").parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn bad_tickets_are_rejected() {
        for bad in ["", "abc", "zz", "00", &"ab".repeat(MAX_TICKET_LEN)] {
            assert!(
                matches!(bad.parse::<Ticket>(), Err(NetError::InvalidTicket)),
                "{bad}"
            );
        }
    }
}
