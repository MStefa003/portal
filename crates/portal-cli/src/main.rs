//! The `portal` command-line tool: a thin layer over the Portal libraries.
#![forbid(unsafe_code)]

mod ui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{ArgGroup, Parser, Subcommand};
use console::style;
use portal_core::{OverwritePolicy, ScanOptions, SinkError};
use portal_net::{NetError, NetMode, ReceiveOptions, Ticket};
use portal_proto::DeclineReason;
use tracing_subscriber::EnvFilter;

use crate::ui::{Progress, Role, describe};

/// Placeholder until the public rendezvous instance exists (M3).
const DEFAULT_RENDEZVOUS: &str = "wss://rendezvous.portal.invalid/v1";

/// At most this many skipped entries are listed individually.
const MAX_SKIPPED_SHOWN: usize = 10;

/// Send files and folders directly between two machines:
/// peer-to-peer, end-to-end encrypted and resumable.
#[derive(Debug, Parser)]
#[command(name = "portal", version, about, long_about = None)]
struct Cli {
    /// Increase log verbosity (-v, -vv, -vvv). `RUST_LOG` overrides this.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Only use 127.0.0.1, no relays (for testing on one machine).
    #[arg(long, global = true, hide = true)]
    loopback: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send a file or folder. Prints what the receiver needs to type.
    Send {
        /// File or folder to send.
        path: PathBuf,

        /// Skip files ignored by .gitignore / .ignore rules.
        #[arg(long)]
        respect_gitignore: bool,

        /// Rendezvous server URL.
        #[arg(long, env = "PORTAL_RENDEZVOUS", default_value = DEFAULT_RENDEZVOUS)]
        rendezvous: String,
    },
    /// Receive a file or folder from a sender.
    #[command(group(ArgGroup::new("from").required(true).args(["code", "addr"])))]
    Receive {
        /// The code shown by the sender, e.g. 7-sunset-river (coming soon).
        code: Option<String>,

        /// The address printed by `portal send`.
        #[arg(long, value_name = "ADDRESS")]
        addr: Option<String>,

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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let mode = if cli.loopback {
        NetMode::Loopback
    } else {
        NetMode::Internet
    };

    let result = match cli.command {
        Command::Send {
            path,
            respect_gitignore,
            rendezvous: _,
        } => send(&path, respect_gitignore, mode).await,
        Command::Receive {
            code,
            addr,
            output,
            overwrite,
            rendezvous: _,
        } => receive(code, addr, output, overwrite, mode).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e:#}", style("Error:").red().bold());
            ExitCode::FAILURE
        }
    }
}

async fn send(path: &Path, respect_gitignore: bool, mode: NetMode) -> anyhow::Result<()> {
    let options = ScanOptions { respect_gitignore };
    let to_scan = path.to_path_buf();
    let scan = tokio::task::spawn_blocking(move || portal_core::scan(&to_scan, &options))
        .await
        .context("scanning failed")??;
    for skipped in scan.skipped.iter().take(MAX_SKIPPED_SHOWN) {
        let what = skipped
            .path
            .as_deref()
            .map_or_else(|| "an entry".to_owned(), |p| p.display().to_string());
        ui::warn(format!("skipped {what}: {}", skipped.reason));
    }
    if scan.skipped.len() > MAX_SKIPPED_SHOWN {
        ui::warn(format!(
            "…and {} more skipped",
            scan.skipped.len() - MAX_SKIPPED_SHOWN
        ));
    }

    let manifest = &scan.manifest;
    let summary = describe(
        manifest.file_count(),
        manifest.total_bytes(),
        manifest.is_single_file(),
    );
    println!(
        "Sending {} ({summary})",
        style(format!("\"{}\"", manifest.name())).bold()
    );

    let endpoint = portal_net::bind(mode, true).await?;
    let ticket = portal_net::local_ticket(&endpoint, mode).await;
    let loopback = if mode == NetMode::Loopback {
        " --loopback"
    } else {
        ""
    };
    println!("\nOn the other machine, run:\n\n  portal receive{loopback} --addr {ticket}\n");
    println!("Waiting for receiver...");

    let progress = Progress::new();
    let result = portal_net::send(&endpoint, path, manifest, progress.reporter(Role::Send)).await;
    progress.finish();
    endpoint.close().await;
    result.map_err(explain)?;
    ui::success(format!("Sent {summary}"));
    Ok(())
}

async fn receive(
    code: Option<String>,
    addr: Option<String>,
    output: Option<PathBuf>,
    overwrite: bool,
    mode: NetMode,
) -> anyhow::Result<()> {
    let Some(addr) = addr else {
        // Deliberately not echoing the code back.
        let _ = code;
        bail!(
            "short codes are not supported yet. Ask the sender for the --addr value that `portal send` printed."
        );
    };
    let ticket: Ticket = addr.parse()?;
    let dest = match output {
        Some(dir) => dir,
        None => std::env::current_dir().context("could not determine the current folder")?,
    };

    let endpoint = portal_net::bind(mode, false).await?;
    println!("Connecting...");
    let progress = Progress::new();
    let options = ReceiveOptions {
        dest: dest.clone(),
        overwrite: if overwrite {
            OverwritePolicy::Always
        } else {
            OverwritePolicy::Never
        },
    };
    let result = portal_net::receive(
        &endpoint,
        &ticket,
        options,
        progress.reporter(Role::Receive),
    )
    .await;
    progress.finish();
    endpoint.close().await;
    let summary = result.map_err(explain)?;
    ui::success(format!(
        "Received {} into {}",
        describe(summary.files, summary.bytes, false),
        dest.display()
    ));
    Ok(())
}

/// Turns library errors into advice a user can act on.
fn explain(e: NetError) -> anyhow::Error {
    match e {
        NetError::Sink(SinkError::Conflicts(_)) => anyhow!(
            "{e}. Use --overwrite to replace existing files, or --output to receive into another folder."
        ),
        NetError::Declined(DeclineReason::Conflicts) => anyhow!(
            "the receiver already has files with these names. They can re-run with --overwrite or --output."
        ),
        NetError::Connect(_) => anyhow::Error::new(e).context(
            "Could not reach the sender. Check that `portal send` is still running and that the whole address was copied",
        ),
        other => other.into(),
    }
}

/// Logs go to stderr so they never mix with output meant for the user's pipes.
fn init_logging(verbose: u8) {
    // Without -v, only our own warnings: iroh's internal ones are noise to users.
    let default = match verbose {
        0 => "error,portal_core=warn,portal_net=warn,portal=warn",
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
