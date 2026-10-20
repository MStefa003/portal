//! `portal-rendezvous`: a small mailbox server that lets two Portal peers find
//! each other using the number from a short code. It never sees file
//! contents, keys or the code words.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use portal_rendezvous::Config;
use tracing_subscriber::EnvFilter;

/// Rendezvous server for Portal peers.
///
/// Serves plain WebSocket (ws://) on `--listen`. For wss://, put it behind a
/// TLS-terminating reverse proxy; see docs/SELF_HOSTING.md.
#[derive(Debug, Parser)]
#[command(name = "portal-rendezvous", version, about, long_about = None)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "PORTAL_RENDEZVOUS_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,

    /// Minutes a sender may wait for a receiver.
    #[arg(long, default_value_t = 60)]
    code_ttl_minutes: u64,

    /// Maximum number of senders waiting at once.
    #[arg(long, default_value_t = 10_000)]
    max_waiting: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let config = Config {
        nameplate_ttl: Duration::from_secs(args.code_ttl_minutes.saturating_mul(60)),
        max_waiting: args.max_waiting,
        max_connections: args.max_waiting.saturating_mul(2),
        ..Config::default()
    };
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("could not listen on {}", args.listen))?;
    tracing::info!(addr = %listener.local_addr()?, "portal-rendezvous listening");
    portal_rendezvous::serve(listener, config)
        .await
        .context("server failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn args_definition_is_valid() {
        Args::command().debug_assert();
    }
}
