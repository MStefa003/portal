//! The `portal` command-line tool: a thin layer over the Portal libraries.
#![forbid(unsafe_code)]

mod ui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use clap::{Parser, Subcommand};
use console::style;
use portal_core::{OverwritePolicy, ScanOptions, SinkError};
use portal_net::{DhtNetwork, Discovery, NetError, NetMode, ReceiveOptions};
use portal_proto::{Code, DeclineReason, PakeError, RendezvousError};
use tracing_subscriber::EnvFilter;

use crate::ui::{Progress, Role, describe};

/// How long a sender waits for its receiver: as long as a rendezvous
/// server keeps a nameplate.
const SEND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// How long a receiver looks for the sender. The sender needs some seconds
/// to reach a relay and publish to the DHT after showing the code.
const FIND_TIMEOUT: Duration = Duration::from_secs(60);

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

    /// Pair through this rendezvous server (ws:// or wss:// URL) instead of
    /// finding the other side on the local network or the DHT.
    #[arg(long, global = true, env = "PORTAL_RENDEZVOUS")]
    rendezvous: Option<String>,

    /// Only use 127.0.0.1, no relays (for testing on one machine).
    #[arg(long, global = true, hide = true)]
    loopback: bool,

    /// Use a private DHT reached through this node instead of the public
    /// one (for tests). Repeatable.
    #[arg(long, global = true, hide = true)]
    dht_bootstrap: Vec<String>,

    /// Do not announce or search on the local network.
    #[arg(long, global = true, hide = true)]
    no_mdns: bool,

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
    },
    /// Receive a file or folder from a sender.
    Receive {
        /// The code shown by the sender, e.g. 7-cedar-river. Asked for if omitted.
        code: Option<String>,

        /// Folder to save into (defaults to the current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Overwrite existing files without asking.
        #[arg(long)]
        overwrite: bool,
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
    // Loopback means "touch no real network": no mDNS, and the DHT only if
    // a private one was given.
    let discovery = Discovery {
        mdns: !cli.loopback && !cli.no_mdns,
        dht: if !cli.dht_bootstrap.is_empty() {
            Some(DhtNetwork::Local(cli.dht_bootstrap.clone()))
        } else if cli.loopback {
            None
        } else {
            Some(DhtNetwork::Public)
        },
    };
    let net = Net {
        mode,
        discovery,
        dht_bootstrap: cli.dht_bootstrap,
    };

    let result = match cli.command {
        Command::Send {
            path,
            respect_gitignore,
        } => send(&path, respect_gitignore, cli.rendezvous.as_deref(), &net).await,
        Command::Receive {
            code,
            output,
            overwrite,
        } => receive(code, output, overwrite, cli.rendezvous.as_deref(), &net).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e:#}", style("Error:").red().bold());
            ExitCode::FAILURE
        }
    }
}

/// Network settings shared by both commands.
struct Net {
    mode: NetMode,
    discovery: Discovery,
    /// Passed through to the command printed for the receiver.
    dht_bootstrap: Vec<String>,
}

async fn send(
    path: &Path,
    respect_gitignore: bool,
    rendezvous: Option<&str>,
    net: &Net,
) -> anyhow::Result<()> {
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

    let mode = net.mode;
    let endpoint = portal_net::bind(mode, true).await?;
    let mut flags = String::new();
    if mode == NetMode::Loopback {
        flags.push_str(" --loopback");
    }
    for node in &net.dht_bootstrap {
        flags.push_str(&format!(" --dht-bootstrap {node}"));
    }

    let result = match rendezvous {
        Some(url) => {
            let waiting = portal_net::rendezvous::allocate(url)
                .await
                .map_err(|e| explain(e, rendezvous, Role::Send))?;
            flags.push_str(&format!(" --rendezvous {url}"));
            show_code(&waiting.code().to_string(), &flags);
            async {
                let peer = waiting.pair(&endpoint, mode).await?;
                let progress = Progress::new();
                let result = portal_net::send(
                    &endpoint,
                    peer,
                    path,
                    manifest,
                    progress.reporter(Role::Send),
                )
                .await;
                progress.finish();
                result
            }
            .await
        }
        None => {
            let code = Code::random()?;
            show_code(&code.to_string(), &flags);
            let announcement =
                portal_net::discovery::announce(&endpoint, mode, &code, &net.discovery);
            async {
                let conn = tokio::time::timeout(
                    SEND_TIMEOUT,
                    portal_net::direct::accept(&endpoint, &code),
                )
                .await
                .map_err(|_| NetError::Timeout("waiting for the receiver"))??;
                drop(announcement);
                let progress = Progress::new();
                let result =
                    portal_net::send_on(&conn, path, manifest, progress.reporter(Role::Send)).await;
                progress.finish();
                result
            }
            .await
        }
    };
    endpoint.close().await;
    result.map_err(|e| explain(e, rendezvous, Role::Send))?;
    ui::success(format!("Sent {summary}"));
    Ok(())
}

fn show_code(code: &str, flags: &str) {
    println!("Code: {}", style(code).bold().cyan());
    println!("\nOn the other machine, run:\n\n  portal receive{flags} {code}\n");
    println!("Waiting for receiver...");
}

async fn receive(
    code: Option<String>,
    output: Option<PathBuf>,
    overwrite: bool,
    rendezvous: Option<&str>,
    net: &Net,
) -> anyhow::Result<()> {
    let typed = match code {
        Some(code) => code,
        None => ask_for_code()?,
    };
    // Parse before touching the network: a typo caught here does not use up
    // the sender's one attempt.
    let code = Code::parse(&typed)?;
    let dest = match output {
        Some(dir) => dir,
        None => std::env::current_dir().context("could not determine the current folder")?,
    };

    let endpoint = portal_net::bind(net.mode, false).await?;
    println!("Connecting...");
    let options = ReceiveOptions {
        dest: dest.clone(),
        overwrite: if overwrite {
            OverwritePolicy::Always
        } else {
            OverwritePolicy::Never
        },
    };
    let result = async {
        let progress;
        let result = match rendezvous {
            Some(url) => {
                let sender = portal_net::rendezvous::join(url, &code, &endpoint).await?;
                progress = Progress::new();
                portal_net::receive(&endpoint, sender, options, progress.reporter(Role::Receive))
                    .await
            }
            None => {
                let sender =
                    portal_net::discovery::find(&code, &net.discovery, FIND_TIMEOUT).await?;
                let conn = portal_net::direct::connect(&endpoint, sender, &code).await?;
                progress = Progress::new();
                portal_net::receive_on(&conn, options, progress.reporter(Role::Receive)).await
            }
        };
        progress.finish();
        result
    }
    .await;
    endpoint.close().await;
    let summary = result.map_err(|e| explain(e, rendezvous, Role::Receive))?;
    ui::success(format!(
        "Received {} into {}",
        describe(summary.files, summary.bytes, false),
        dest.display()
    ));
    Ok(())
}

/// Reads the code from the terminal when it was not given as an argument.
fn ask_for_code() -> anyhow::Result<String> {
    let term = console::Term::stderr();
    if !term.is_term() {
        bail!("no code given. Usage: portal receive <CODE>");
    }
    term.write_str("Enter the code shown by the sender: ")?;
    Ok(term.read_line()?)
}

/// Turns library errors into advice a user can act on.
fn explain(e: NetError, rendezvous: Option<&str>, role: Role) -> anyhow::Error {
    match e {
        NetError::Pake(PakeError::WrongCode) if role == Role::Send => anyhow!(
            "someone tried to connect with a wrong code, so the transfer was cancelled to keep your files safe. Run `portal send` again for a new code."
        ),
        NetError::Pake(PakeError::WrongCode) => anyhow!(
            "the code is wrong. Each code allows one try, so ask the sender to run `portal send` again and type the new code carefully."
        ),
        NetError::Rendezvous(RendezvousError::UnknownNameplate) => anyhow!(
            "no sender is waiting for this code. Check the number at the start, and that `portal send` is still running."
        ),
        NetError::SenderNotFound => anyhow!(
            "no sender found for this code. Check the number at the start, and that `portal send` is still running. If it was only just started, try again in a few seconds."
        ),
        NetError::WrongPeer => anyhow!(
            "the other side could not prove who it is, so the transfer was stopped. Someone may be interfering; run `portal send` again for a new code."
        ),
        NetError::RendezvousConnect(_) => anyhow::Error::new(e).context(format!(
            "Could not reach the rendezvous server at {}",
            rendezvous.unwrap_or("?")
        )),
        NetError::Sink(SinkError::Conflicts(_)) => anyhow!(
            "{e}. Use --overwrite to replace existing files, or --output to receive into another folder."
        ),
        NetError::Declined(DeclineReason::Conflicts) => anyhow!(
            "the receiver already has files with these names. They can re-run with --overwrite or --output."
        ),
        NetError::Connect(_) => anyhow::Error::new(e).context(
            "Could not reach the sender. Check that `portal send` is still running and that neither network blocks UDP",
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
