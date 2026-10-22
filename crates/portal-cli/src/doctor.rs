//! `portal doctor`: checks what works on this network and explains what it
//! means for transfers.

use crate::ui::{self, Level};
use crate::{config, update};

/// Runs every check and prints the results.
pub async fn run(rendezvous: Option<&str>) -> anyhow::Result<()> {
    println!("Checking this network (this takes up to 20 seconds)...\n");
    let (report, latest) = tokio::join!(portal_net::doctor::check(), update::latest());
    let report = report?;

    match &report.relay {
        Some((url, latency)) => {
            let latency = latency.map_or(String::new(), |l| format!(" ({} ms)", l.as_millis()));
            // Relay URLs end in a fully qualified host name: "…iroh.link./".
            let url = url.to_string();
            let url = url.trim_end_matches('/').trim_end_matches('.');
            ui::check(
                Level::Ok,
                format!(
                    "Relay: connected to {url}{latency}. Transfers work even when a direct path cannot be found."
                ),
            );
        }
        None => ui::check(
            Level::Fail,
            "Relay: unreachable. Without it, only machines on the same local network can connect. Check the internet connection, or whether a firewall blocks outgoing connections.",
        ),
    }

    if report.relay.is_some() {
        match (report.udp_v4, report.udp_v6) {
            (false, false) => ui::check(
                Level::Warn,
                "UDP: blocked. Transfers across the internet go through the relay, which is slower. Some company and school networks do this; a rendezvous server does not help here.",
            ),
            (v4, v6) => {
                let mut address = String::new();
                if let Some(a) = report.public_v4 {
                    address = format!(", public address {a}");
                } else if let Some(a) = report.public_v6 {
                    address = format!(", public address {a}");
                }
                let versions = match (v4, v6) {
                    (true, true) => "IPv4 and IPv6",
                    (true, false) => "IPv4",
                    _ => "IPv6",
                };
                ui::check(Level::Ok, format!("UDP: works over {versions}{address}"));
            }
        }
        match report.hard_nat {
            Some(true) => ui::check(
                Level::Warn,
                "NAT: strict (the router changes ports per destination). Direct connections to machines on other networks often fail, so those transfers use the relay and are slower. Same-network transfers are not affected.",
            ),
            Some(false) => ui::check(
                Level::Ok,
                "NAT: friendly. Direct connections to machines on other networks should work.",
            ),
            None => {}
        }
        if report.captive_portal == Some(true) {
            ui::check(
                Level::Warn,
                "This network seems to show a login page (hotel, airport...). Log in in a browser first.",
            );
        }
    }

    match &report.dht_error {
        None => ui::check(
            Level::Ok,
            "DHT: reachable. Codes work across the internet without a server.",
        ),
        Some(e) => ui::check(
            Level::Fail,
            format!(
                "DHT: unreachable ({e}). Codes only work on the local network; use a rendezvous server (--rendezvous) to pair across the internet."
            ),
        ),
    }
    match &report.mdns_error {
        None => ui::check(
            Level::Ok,
            "Local network discovery: works. Machines on the same network find each other instantly.",
        ),
        Some(e) => ui::check(
            Level::Warn,
            format!(
                "Local network discovery: not available ({e}). Codes still work through the DHT."
            ),
        ),
    }

    if let Some(url) = rendezvous {
        match portal_net::rendezvous::allocate(url).await {
            // Dropping it frees the number again.
            Ok(_) => ui::check(Level::Ok, format!("Rendezvous server {url}: reachable")),
            Err(e) => ui::check(
                Level::Fail,
                format!("Rendezvous server {url}: {:#}", anyhow::Error::new(e)),
            ),
        }
    }

    match config::download_dir() {
        Ok(Some(dir)) if dir.is_dir() => {
            ui::check(Level::Ok, format!("Download folder: {}", dir.display()))
        }
        Ok(Some(dir)) => ui::check(
            Level::Warn,
            format!(
                "Download folder: {} does not exist; it is created on the next transfer",
                dir.display()
            ),
        ),
        Ok(None) => ui::check(
            Level::Ok,
            "Download folder: not set, files are saved into the current folder (`portal dir` sets one)",
        ),
        Err(e) => ui::check(Level::Warn, format!("Download folder: {e:#}")),
    }

    let current = update::Version::current();
    match latest {
        Ok(latest) if latest > current => ui::check(
            Level::Warn,
            format!("Portal {latest} is available (this is {current}). Run `portal update`."),
        ),
        Ok(_) => ui::check(Level::Ok, format!("Portal {current} is the newest version")),
        Err(e) => ui::check(Level::Warn, format!("Could not check for updates: {e:#}")),
    }
    Ok(())
}
