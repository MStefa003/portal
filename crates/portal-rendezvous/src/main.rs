//! `portal-rendezvous`: a small mailbox server that lets two Portal peers find
//! each other using the channel number from a short code. It never sees file
//! contents, keys or the code words.
#![forbid(unsafe_code)]

use std::net::SocketAddr;

use anyhow::bail;
use clap::Parser;

/// Rendezvous server for Portal peers.
#[derive(Debug, Parser)]
#[command(name = "portal-rendezvous", version, about, long_about = None)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "PORTAL_RENDEZVOUS_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    let _args = Args::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    bail!("the rendezvous server is not implemented yet.")
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
