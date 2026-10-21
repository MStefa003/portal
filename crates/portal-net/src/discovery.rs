//! Finding a sender by the number in its code, without a server of our own.
//!
//! The sender [`announce`]s "the sender for nameplate N is at this address"
//! in two places, and the receiver [`find`]s it by looking in both at once:
//! - **mDNS** on the local network: instant, and works with no internet.
//! - **the BitTorrent Mainline DHT** (a BEP 44 mutable item): works across
//!   the internet. The item's signing key is derived from the nameplate, so
//!   anyone who knows the number can find (or overwrite) it.
//!
//! Neither source is trusted. What they return is only a place to dial; the
//! code handshake in [`crate::direct`] decides whether it is the right peer.
//! To keep IP addresses out of a public database, the DHT item names only
//! the sender's node ID and relay when it has one; the direct path is then
//! found by hole punching through that relay.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use mainline::{Dht, MutableItem, SigningKey, async_dht::AsyncDht};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use portal_proto::code::Code;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::NetMode;
use crate::endpoint::local_addr;
use crate::error::NetError;

/// Where to announce and search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// Use mDNS on the local network.
    pub mdns: bool,
    /// Use the Mainline DHT.
    pub dht: Option<DhtNetwork>,
}

impl Discovery {
    /// Everything on: the local network and the public DHT.
    pub fn public() -> Self {
        Self {
            mdns: true,
            dht: Some(DhtNetwork::Public),
        }
    }
}

/// Which Mainline DHT to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DhtNetwork {
    /// The public BitTorrent DHT, joined through its usual bootstrap nodes.
    Public,
    /// A private DHT on 127.0.0.1 reached through these nodes (for tests).
    Local(Vec<String>),
}

/// blake3 context for the DHT signing key of a nameplate.
const DHT_KEY_CONTEXT: &str = "portal 2026-10-21 dht key for nameplate";
/// Version of [`DhtRecord`].
const DHT_RECORD_VERSION: u16 = 1;
/// BEP 44 caps values at 1000 bytes.
const MAX_DHT_VALUE: usize = 1000;
/// DHT nodes drop items after about two hours; refresh well before.
const REPUBLISH_EVERY: Duration = Duration::from_secs(30 * 60);
/// Wait between failed publishes (no network yet, DHT still bootstrapping).
const PUBLISH_RETRY: Duration = Duration::from_secs(15);
/// Wait between DHT lookups while the sender may still be publishing.
const LOOKUP_RETRY: Duration = Duration::from_secs(2);
/// Items older than this belong to an earlier transfer with the same number.
const MAX_RECORD_AGE: Duration = Duration::from_secs(2 * 60 * 60);

/// mDNS service type for Portal senders.
const MDNS_SERVICE: &str = "_portal._udp.local.";
/// TXT keys: the sender's node ID, and its IPv6 port if it has one.
const MDNS_ID: &str = "id";
const MDNS_PORT_V6: &str = "port6";

/// What a sender stores in the DHT.
#[derive(Debug, Serialize, Deserialize)]
struct DhtRecord {
    version: u16,
    addr: EndpointAddr,
}

/// A running announcement; dropping it stops announcing.
pub struct Announcement {
    _tasks: JoinSet<()>,
    mdns: Option<(ServiceDaemon, String)>,
}

impl std::fmt::Debug for Announcement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Announcement")
            .field("mdns", &self.mdns.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for Announcement {
    fn drop(&mut self) {
        if let Some((daemon, name)) = self.mdns.take() {
            let _ = daemon.unregister(&name);
            let _ = daemon.shutdown();
        }
    }
}

/// Sender: starts announcing `endpoint` under `code`'s nameplate.
///
/// Returns at once; publishing to the DHT goes on in the background (it
/// first waits for a relay, then takes a few seconds). Failures are logged,
/// not returned: one working source is enough.
pub fn announce(
    endpoint: &Endpoint,
    mode: NetMode,
    code: &Code,
    discovery: &Discovery,
) -> Announcement {
    let nameplate = code.nameplate();
    let mut tasks = JoinSet::new();
    let mdns = if discovery.mdns {
        match announce_mdns(endpoint, nameplate) {
            Ok(registered) => Some(registered),
            Err(e) => {
                tracing::warn!(%e, "could not announce on the local network");
                None
            }
        }
    } else {
        None
    };
    if let Some(network) = &discovery.dht {
        tasks.spawn(publish_dht(
            endpoint.clone(),
            mode,
            nameplate,
            network.clone(),
        ));
    }
    Announcement {
        _tasks: tasks,
        mdns,
    }
}

/// Receiver: the address announced for `code`'s nameplate, from whichever
/// source answers first. Gives up after `timeout`.
pub async fn find(
    code: &Code,
    discovery: &Discovery,
    timeout: Duration,
) -> Result<EndpointAddr, NetError> {
    let nameplate = code.nameplate();
    let mdns = async {
        if !discovery.mdns {
            return std::future::pending().await;
        }
        match find_mdns(nameplate).await {
            Ok(addr) => addr,
            Err(e) => {
                tracing::warn!(%e, "could not search the local network");
                std::future::pending().await
            }
        }
    };
    let dht = async {
        let Some(network) = &discovery.dht else {
            return std::future::pending().await;
        };
        match find_dht(nameplate, network).await {
            Ok(addr) => addr,
            Err(e) => {
                tracing::warn!(%e, "could not search the DHT");
                std::future::pending().await
            }
        }
    };
    let found = async {
        tokio::select! {
            addr = mdns => { tracing::info!("found the sender on the local network"); addr }
            addr = dht => { tracing::info!("found the sender in the DHT"); addr }
        }
    };
    tokio::time::timeout(timeout, found)
        .await
        .map_err(|_| NetError::SenderNotFound)
}

// --- DHT ---

/// The DHT signing key for `nameplate`. Deliberately public: it lets the
/// receiver find the item, and the code words are what keep the transfer safe.
fn dht_key(nameplate: u32) -> SigningKey {
    SigningKey::from_bytes(&blake3::derive_key(
        DHT_KEY_CONTEXT,
        &nameplate.to_be_bytes(),
    ))
}

fn dht_client(network: &DhtNetwork) -> Result<AsyncDht, NetError> {
    let mut builder = Dht::builder();
    if let DhtNetwork::Local(bootstrap) = network {
        builder
            .bootstrap(bootstrap)
            .bind_address(Ipv4Addr::LOCALHOST);
    }
    let dht = builder
        .build()
        .map_err(|e| NetError::Discovery(Box::new(e)))?;
    Ok(dht.as_async())
}

/// What to publish: the node ID and relays only when a relay is known,
/// otherwise the direct addresses (no relay means no hole punching).
fn dht_record(addr: EndpointAddr) -> Result<Vec<u8>, NetError> {
    let relays: Vec<TransportAddr> = addr
        .addrs
        .iter()
        .filter(|a| matches!(a, TransportAddr::Relay(_)))
        .cloned()
        .collect();
    let mut addrs: Vec<TransportAddr> = if relays.is_empty() {
        addr.addrs.into_iter().collect()
    } else {
        relays
    };
    loop {
        let record = DhtRecord {
            version: DHT_RECORD_VERSION,
            addr: EndpointAddr::from_parts(addr.id, addrs.iter().cloned()),
        };
        let bytes =
            postcard::to_allocvec(&record).map_err(|e| NetError::Internal(e.to_string()))?;
        if bytes.len() <= MAX_DHT_VALUE {
            return Ok(bytes);
        }
        if addrs.pop().is_none() {
            return Err(NetError::Internal("DHT record too large".into()));
        }
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Publishes the sender's address until the task is dropped.
async fn publish_dht(endpoint: Endpoint, mode: NetMode, nameplate: u32, network: DhtNetwork) {
    let dht = match dht_client(&network) {
        Ok(dht) => dht,
        Err(e) => {
            tracing::warn!(%e, "could not join the DHT");
            return;
        }
    };
    let key = dht_key(nameplate);
    loop {
        let wait = match publish_once(&dht, &key, &endpoint, mode).await {
            Ok(()) => {
                tracing::info!("announced the code in the DHT");
                REPUBLISH_EVERY
            }
            Err(e) => {
                tracing::warn!(%e, "could not announce the code in the DHT; retrying");
                PUBLISH_RETRY
            }
        };
        tokio::time::sleep(wait).await;
    }
}

async fn publish_once(
    dht: &AsyncDht,
    key: &SigningKey,
    endpoint: &Endpoint,
    mode: NetMode,
) -> Result<(), NetError> {
    let value = dht_record(local_addr(endpoint, mode).await)?;
    // The time as sequence number: newer announcements win over leftovers
    // from an earlier transfer that used the same number.
    let item = MutableItem::new(key.clone(), &value, now_secs(), None);
    dht.put_mutable(item, None)
        .await
        .map(|_| ())
        .map_err(|e| NetError::Discovery(Box::new(e)))
}

/// Looks the nameplate up until a recent record appears.
async fn find_dht(nameplate: u32, network: &DhtNetwork) -> Result<EndpointAddr, NetError> {
    let dht = dht_client(network)?;
    let public_key = dht_key(nameplate).verifying_key().to_bytes();
    let max_age = i64::try_from(MAX_RECORD_AGE.as_secs()).unwrap_or(i64::MAX);
    loop {
        if let Some(item) = dht.get_mutable_most_recent(&public_key, None).await
            && now_secs().saturating_sub(item.seq()) <= max_age
        {
            match postcard::from_bytes::<DhtRecord>(item.value()) {
                Ok(record) if record.version == DHT_RECORD_VERSION => return Ok(record.addr),
                _ => tracing::warn!("ignored an unreadable DHT record"),
            }
        }
        tokio::time::sleep(LOOKUP_RETRY).await;
    }
}

// --- mDNS ---

fn mdns_instance(nameplate: u32) -> String {
    format!("portal-{nameplate}")
}

fn announce_mdns(endpoint: &Endpoint, nameplate: u32) -> Result<(ServiceDaemon, String), NetError> {
    let sockets = endpoint.bound_sockets();
    let port_v4 = sockets
        .iter()
        .find(|s| s.is_ipv4())
        .map(SocketAddr::port)
        .ok_or_else(|| NetError::Internal("no IPv4 socket to announce".into()))?;
    let mut txt = vec![(MDNS_ID, endpoint.id().to_string())];
    if let Some(v6) = sockets.iter().find(|s| s.is_ipv6()) {
        txt.push((MDNS_PORT_V6, v6.port().to_string()));
    }
    let host = format!("portal-{}.local.", endpoint.id().fmt_short());
    let info = ServiceInfo::new(
        MDNS_SERVICE,
        &mdns_instance(nameplate),
        &host,
        (),
        port_v4,
        &txt[..],
    )
    .map_err(|e| NetError::Discovery(Box::new(e)))?
    .enable_addr_auto();
    let fullname = info.get_fullname().to_owned();
    let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(Box::new(e)))?;
    daemon
        .register(info)
        .map_err(|e| NetError::Discovery(Box::new(e)))?;
    Ok((daemon, fullname))
}

/// Browses until a sender with this nameplate resolves.
async fn find_mdns(nameplate: u32) -> Result<EndpointAddr, NetError> {
    let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(Box::new(e)))?;
    let events = daemon
        .browse(MDNS_SERVICE)
        .map_err(|e| NetError::Discovery(Box::new(e)))?;
    let prefix = format!("{}.", mdns_instance(nameplate));
    let found = loop {
        let Ok(event) = events.recv_async().await else {
            break Err(NetError::Internal("mDNS browsing stopped".into()));
        };
        let ServiceEvent::ServiceResolved(service) = event else {
            continue;
        };
        if !service.fullname.starts_with(&prefix) {
            continue;
        }
        let Some(id) = service
            .get_property_val_str(MDNS_ID)
            .and_then(|s| s.parse::<EndpointId>().ok())
        else {
            continue;
        };
        let port_v6 = service
            .get_property_val_str(MDNS_PORT_V6)
            .and_then(|p| p.parse::<u16>().ok());
        let ips: BTreeSet<SocketAddr> = service
            .addresses
            .iter()
            .filter_map(|ip| match ip.to_ip_addr() {
                IpAddr::V4(v4) => Some(SocketAddr::new(v4.into(), service.port)),
                // Link-local addresses need a scope ID that we cannot carry.
                IpAddr::V6(v6) if !v6.is_unicast_link_local() => {
                    port_v6.map(|p| SocketAddr::new(v6.into(), p))
                }
                IpAddr::V6(_) => None,
            })
            .collect();
        if ips.is_empty() {
            continue;
        }
        break Ok(EndpointAddr::from_parts(
            id,
            ips.into_iter().map(TransportAddr::Ip),
        ));
    };
    let _ = daemon.shutdown();
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn dht_keys_differ_per_nameplate_and_are_stable() {
        assert_eq!(dht_key(7).to_bytes(), dht_key(7).to_bytes());
        assert_ne!(dht_key(7).to_bytes(), dht_key(8).to_bytes());
    }

    fn sample_addr(ips: usize, relay: bool) -> EndpointAddr {
        let id = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let mut addrs: Vec<TransportAddr> = (0..ips)
            .map(|i| {
                TransportAddr::Ip(SocketAddr::new(
                    Ipv4Addr::new(10, 0, (i / 256) as u8, (i % 256) as u8).into(),
                    4000,
                ))
            })
            .collect();
        if relay {
            addrs.push(TransportAddr::Relay(
                "https://relay.example.com".parse().unwrap(),
            ));
        }
        EndpointAddr::from_parts(id, addrs)
    }

    fn decode(bytes: &[u8]) -> EndpointAddr {
        postcard::from_bytes::<DhtRecord>(bytes).unwrap().addr
    }

    #[test]
    fn dht_record_hides_ips_when_a_relay_is_known() {
        let addr = decode(&dht_record(sample_addr(3, true)).unwrap());
        assert_eq!(addr.relay_urls().count(), 1);
        assert_eq!(addr.ip_addrs().count(), 0);
    }

    #[test]
    fn dht_record_falls_back_to_ips_and_fits() {
        let addr = decode(&dht_record(sample_addr(3, false)).unwrap());
        assert_eq!(addr.ip_addrs().count(), 3);
        let many = dht_record(sample_addr(500, false)).unwrap();
        assert!(many.len() <= MAX_DHT_VALUE);
        assert!(decode(&many).ip_addrs().count() > 0);
    }
}
