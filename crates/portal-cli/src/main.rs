//! The `portal` command-line tool: a thin layer over the Portal libraries.
#![forbid(unsafe_code)]

mod ui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{Parser, Subcommand};
use console::style;
use portal_core::{OverwritePolicy, ScanOptions, SinkError};
use portal_net::{NetError, NetMode, ReceiveOptions};
use portal_proto::{Code, DeclineReason, PakeError, RendezvousError};
use tracing_subscriber::EnvFilter;

use crate::ui::{Progress, Role, describe};

/// Placeholder until a public rendezvous instance exists; see
/// docs/SELF_HOSTING.md. `.invalid` never resolves, so this fails clearly.
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
    Receive {
        /// The code shown by the sender, e.g. 7-cedar-river. Asked for if omitted.
        code: Option<String>,

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
            rendezvous,
        } => send(&path, respect_gitignore, &rendezvous, mode).await,
        Command::Receive {
            code,
            output,
            overwrite,
            rendezvous,
        } => receive(code, output, overwrite, &rendezvous, mode).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e:#}", style("Error:").red().bold());
            ExitCode::FAILURE
        }
    }
}

async fn send(
    path: &Path,
    respect_gitignore: bool,
    rendezvous: &str,
    mode: NetMode,
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

    let endpoint = portal_net::bind(mode, true).await?;
    let waiting = portal_net::rendezvous::allocate(rendezvous)
        .await
        .map_err(|e| explain(e, rendezvous, Role::Send))?;
    let code = waiting.code().to_string();
    let mut flags = String::new();
    if mode == NetMode::Loopback {
        flags.push_str(" --loopback");
    }
    if rendezvous != DEFAULT_RENDEZVOUS {
        flags.push_str(&format!(" --rendezvous {rendezvous}"));
    }
    println!("Code: {}", style(&code).bold().cyan());
    println!("\nOn the other machine, run:\n\n  portal receive{flags} {code}\n");
    println!("Waiting for receiver...");

    let result = async {
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
    .await;
    endpoint.close().await;
    result.map_err(|e| explain(e, rendezvous, Role::Send))?;
    ui::success(format!("Sent {summary}"));
    Ok(())
}

async fn receive(
    code: Option<String>,
    output: Option<PathBuf>,
    overwrite: bool,
    rendezvous: &str,
    mode: NetMode,
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

    let endpoint = portal_net::bind(mode, false).await?;
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
        let sender = portal_net::rendezvous::join(rendezvous, &code, &endpoint).await?;
        let progress = Progress::new();
        let result =
            portal_net::receive(&endpoint, sender, options, progress.reporter(Role::Receive)).await;
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
fn explain(e: NetError, rendezvous: &str, role: Role) -> anyhow::Error {
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
        NetError::RendezvousConnect(_) if rendezvous == DEFAULT_RENDEZVOUS => anyhow!(
            "there is no public rendezvous server yet. Run `portal-rendezvous` yourself (see docs/SELF_HOSTING.md) and pass --rendezvous ws://<host>:8080/v1, or set PORTAL_RENDEZVOUS."
        ),
        NetError::RendezvousConnect(_) => anyhow::Error::new(e).context(format!(
            "Could not reach the rendezvous server at {rendezvous}"
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
