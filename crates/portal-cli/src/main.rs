//! The `portal` command-line tool: a thin layer over the Portal libraries.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::bail;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Placeholder until the public rendezvous instance exists (M3).
const DEFAULT_RENDEZVOUS: &str = "wss://rendezvous.portal.invalid/v1";

/// Send files and folders directly between two machines:
/// peer-to-peer, end-to-end encrypted and resumable.
#[derive(Debug, Parser)]
#[command(name = "portal", version, about, long_about = None)]
struct Cli {
    /// Increase log verbosity (-v, -vv, -vvv). `RUST_LOG` overrides this.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send a file or folder. Prints a code to give to the receiver.
    Send {
        /// File or folder to send.
        path: PathBuf,

        /// Rendezvous server URL.
        #[arg(long, env = "PORTAL_RENDEZVOUS", default_value = DEFAULT_RENDEZVOUS)]
        rendezvous: String,
    },
    /// Receive a file or folder using the code shown by the sender.
    Receive {
        /// The code shown by the sender, e.g. 7-sunset-river.
        code: String,

        /// Folder to save into (defaults to the current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Overwrite existing files without asking.
        #[arg(long)]
        overwrite: bool,

        /// Rendezvous server URL.
        #[arg(long, env = "PORTAL_RENDEZVOUS", default_value = DEFAULT_RENDEZVOUS)]
        rendezvous: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match cli.command {
        Command::Send { .. } => bail!("`portal send` is not implemented yet."),
        Command::Receive { .. } => bail!("`portal receive` is not implemented yet."),
    }
}

/// Logs go to stderr so they never mix with output meant for the user's pipes.
fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
