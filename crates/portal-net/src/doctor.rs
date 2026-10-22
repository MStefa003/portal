//! Network diagnostics: what works on this network, and what that means
//! for transfers.

use std::net::{SocketAddrV4, SocketAddrV6};
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::{Endpoint, RelayUrl, Watcher};
use mdns_sd::ServiceDaemon;

use crate::discovery::{DhtNetwork, dht_client};
use crate::error::NetError;

/// How long to wait for a relay connection.
const RELAY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the first network report after reaching a relay.
const REPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for joining the DHT.
const DHT_TIMEOUT: Duration = Duration::from_secs(20);

/// What [`check`] found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The relay server this machine connected to, and its round-trip time.
    pub relay: Option<(RelayUrl, Option<Duration>)>,
    /// UDP works over IPv4 / IPv6 (QUIC round trips to the relays completed).
    pub udp_v4: bool,
    /// See [`Self::udp_v4`].
    pub udp_v6: bool,
    /// This machine's public addresses as the relays see them.
    pub public_v4: Option<SocketAddrV4>,
    /// See [`Self::public_v4`].
    pub public_v6: Option<SocketAddrV6>,
    /// The NAT maps one local port to different public ports per destination
    /// ("hard" or symmetric NAT), which defeats most hole punching.
    pub hard_nat: Option<bool>,
    /// Something intercepts web traffic, e.g. a hotel or airport login page.
    pub captive_portal: Option<bool>,
    /// Why the public DHT could not be joined, if it could not.
    pub dht_error: Option<String>,
    /// Why local network discovery (mDNS) could not start, if it could not.
    pub mdns_error: Option<String>,
}

/// Runs every check; takes up to about 20 seconds.
pub async fn check() -> Result<Report, NetError> {
    let endpoint = Endpoint::builder(presets::N0)
        .bind()
        .await
        .map_err(NetError::bind)?;
    let (network, dht) = tokio::join!(network(&endpoint), dht());
    endpoint.close().await;
    let mdns_error = match ServiceDaemon::new() {
        Ok(daemon) => {
            let _ = daemon.shutdown();
            None
        }
        Err(e) => Some(e.to_string()),
    };
    Ok(Report {
        dht_error: dht.err(),
        mdns_error,
        ..network
    })
}

/// Relay, UDP and NAT, from iroh's network report.
async fn network(endpoint: &Endpoint) -> Report {
    let mut report = Report::default();
    if tokio::time::timeout(RELAY_TIMEOUT, endpoint.online())
        .await
        .is_err()
    {
        return report;
    }
    let mut watcher = endpoint.net_report();
    let Ok(net) = tokio::time::timeout(REPORT_TIMEOUT, watcher.initialized()).await else {
        return report;
    };
    report.udp_v4 = net.udp_v4;
    report.udp_v6 = net.udp_v6;
    report.public_v4 = net.global_v4;
    report.public_v6 = net.global_v6;
    report.hard_nat = net.mapping_varies_by_dest();
    report.captive_portal = net.captive_portal;
    report.relay = net.preferred_relay.clone().map(|url| {
        let latency = net
            .relay_latency
            .iter()
            .filter(|(_, u, _)| **u == url)
            .map(|(_, _, latency)| latency)
            .min();
        (url, latency)
    });
    report
}

async fn dht() -> Result<(), String> {
    let dht = dht_client(&DhtNetwork::Public).map_err(|e| e.to_string())?;
    match tokio::time::timeout(DHT_TIMEOUT, dht.bootstrapped()).await {
        Ok(true) => Ok(()),
        Ok(false) => Err("no DHT node answered".into()),
        Err(_) => Err("timed out".into()),
    }
}
