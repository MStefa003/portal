//! Trusted devices: pair two devices once with a code, then send between
//! them without one.
//!
//! Each device keeps a long-term iroh identity (see [`crate::bind_as`]).
//! Pairing runs the usual code handshake from [`crate::direct`] over a
//! [`TRUST_ALPN`] connection, which proves both node IDs, and then each side
//! tells the other a name for itself. The caller stores the result.
//!
//! Later, the sender binds with its identity, [`announce`]s itself and waits
//! for the device with [`crate::accept_from`]; the receiver [`dial`]s the
//! sender's node ID. TLS authenticates both node IDs, so no code is needed:
//! each side talks only to the one node it expects.

use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use portal_proto::code::Code;
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{ALPN, Device, MAX_DEVICE_NAME, TRUST_ALPN, sanitize};

use crate::discovery::{self, Announcement, Discovery};
use crate::error::NetError;

/// Largest pairing frame: a [`Device`], with room.
const MAX_DEVICE_FRAME: usize = 1024;
/// How long the device exchange after the code handshake may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long [`dial`] looks on the local network before trying the internet.
const LOCAL_WAIT: Duration = Duration::from_secs(3);
/// Wait between attempts while the sender is not reachable yet.
const DIAL_RETRY: Duration = Duration::from_secs(3);

/// The other side of a successful pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    /// Its node ID, proven by the handshake.
    pub id: EndpointId,
    /// The name it gave itself, made safe to display and store.
    pub name: String,
}

/// The side that showed the code: waits for the device that knows `code`,
/// then exchanges names. `endpoint` must have been bound with this
/// device's identity.
pub async fn pair_accept(
    endpoint: &Endpoint,
    code: &Code,
    my_name: &str,
) -> Result<Paired, NetError> {
    let conn = crate::direct::accept_alpn(endpoint, code, TRUST_ALPN).await?;
    let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
        let (mut tx, mut rx) = conn.accept_bi().await.map_err(NetError::connection)?;
        let theirs = read_device(&mut rx).await?;
        write_device(&mut tx, my_name).await?;
        tx.finish().map_err(NetError::connection)?;
        Ok(theirs)
    })
    .await
    .map_err(|_| NetError::Timeout("exchanging device names"))
    .and_then(|r| r);
    // The other side hangs up once it has our name.
    let _ = tokio::time::timeout(crate::LINGER, conn.closed()).await;
    conn.close(0u32.into(), b"paired");
    result.map(|name| Paired {
        id: conn.remote_id(),
        name,
    })
}

/// The side that typed the code: connects to `peer` (found through
/// [`crate::discovery::find`]), proves it knows `code`, then exchanges names.
pub async fn pair_join(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    code: &Code,
    my_name: &str,
) -> Result<Paired, NetError> {
    let conn = crate::direct::connect_alpn(endpoint, peer, code, TRUST_ALPN).await?;
    let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
        let (mut tx, mut rx) = conn.open_bi().await.map_err(NetError::connection)?;
        write_device(&mut tx, my_name).await?;
        tx.finish().map_err(NetError::connection)?;
        read_device(&mut rx).await
    })
    .await
    .map_err(|_| NetError::Timeout("exchanging device names"))
    .and_then(|r| r);
    conn.close(0u32.into(), b"paired");
    result.map(|name| Paired {
        id: conn.remote_id(),
        name,
    })
}

async fn write_device(tx: &mut iroh::endpoint::SendStream, name: &str) -> Result<(), NetError> {
    let device = Device {
        name: clean_name(name),
    };
    write_msg(tx, &device).await.map_err(NetError::from)
}

async fn read_device(rx: &mut iroh::endpoint::RecvStream) -> Result<String, NetError> {
    match read_msg::<_, Device>(rx, MAX_DEVICE_FRAME).await? {
        Some(device) => Ok(clean_name(&device.name)),
        None => Err(NetError::PeerClosed),
    }
}

/// Makes a device name safe to display and store on one line: control
/// characters escaped, surrounding space trimmed, length capped.
pub fn clean_name(name: &str) -> String {
    let mut clean = sanitize(name.trim());
    if clean.len() > MAX_DEVICE_NAME {
        let mut end = MAX_DEVICE_NAME;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    let clean = clean.trim().to_owned();
    if clean.is_empty() {
        "device".to_owned()
    } else {
        clean
    }
}

/// Sender to a trusted device: makes `endpoint` (bound with this device's
/// identity) findable on the local network while the result is kept.
/// Across the internet, iroh's address lookup publishes it already.
pub fn announce(endpoint: &Endpoint, discovery: &Discovery) -> Announcement {
    discovery::announce_device(endpoint, discovery)
}

/// Receiver: connects to the trusted sender `sender` with [`ALPN`],
/// retrying until it is reachable or `timeout` passes. `sender` may carry
/// known addresses (or just the node ID); addresses found on the local
/// network are added.
pub async fn dial(
    endpoint: &Endpoint,
    sender: EndpointAddr,
    discovery: &Discovery,
    timeout: Duration,
) -> Result<Connection, NetError> {
    let expected = sender.id;
    let attempts = async {
        loop {
            let local = if discovery.mdns {
                tokio::time::timeout(LOCAL_WAIT, discovery::find_device(expected, discovery))
                    .await
                    .ok()
            } else {
                None
            };
            let addr = match local {
                Some(found) => EndpointAddr::from_parts(
                    expected,
                    sender.addrs.iter().cloned().chain(found.addrs),
                ),
                None => sender.clone(),
            };
            let only_id = !addr.addrs.iter().any(|a| matches!(a, TransportAddr::Ip(_)));
            match endpoint.connect(addr, ALPN).await {
                Ok(conn) if conn.remote_id() == expected => return Ok(conn),
                Ok(conn) => {
                    conn.close(2u32.into(), b"unexpected peer");
                    return Err(NetError::WrongPeer);
                }
                Err(e) => tracing::info!(%e, only_id, "the device is not reachable yet"),
            }
            tokio::time::sleep(DIAL_RETRY).await;
        }
    };
    tokio::time::timeout(timeout, attempts)
        .await
        .map_err(|_| NetError::DeviceNotFound)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  laptop \n"), "laptop");
        assert_eq!(clean_name("a\tb"), "a\\tb");
        assert_eq!(clean_name("\x1b[2J"), "\\u{1b}[2J");
        assert_eq!(clean_name("   "), "device");
        let long = clean_name(&"é".repeat(100));
        assert!(long.len() <= MAX_DEVICE_NAME);
        assert!(long.starts_with('é'));
    }
}
