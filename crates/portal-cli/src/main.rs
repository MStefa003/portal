//! The `portal` command-line tool: a thin layer over the Portal libraries.
#![forbid(unsafe_code)]

mod clipboard;
mod config;
mod doctor;
mod menu;
mod ui;
mod update;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand};
use console::style;
use portal_core::path::PARTIAL_DIR_NAME;
use portal_core::{OverwritePolicy, ScanOptions, SinkError};
use portal_net::{
    Confirm, DhtNetwork, Discovery, EndpointAddr, NetError, NetMode, Payload, ReceiveOptions,
    Received, SendOptions,
};
use portal_proto::{Code, DeclineReason, MAX_TEXT, Offer, PakeError, RendezvousError};
use tracing_subscriber::EnvFilter;

use crate::ui::{Progress, Role, Stats, describe};

/// How long a sender waits for its receiver: as long as a rendezvous
/// server keeps a nameplate.
const SEND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// How long a receiver looks for the sender. The sender needs some seconds
/// to reach a relay and publish to the DHT after showing the code.
const FIND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a receiver waits for a trusted device to start sending.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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

    /// Started from the file manager's menu: copy the code, and keep the
    /// window open until it has been read.
    #[arg(long, global = true, hide = true)]
    pause: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send files, folders or text. Prints what the receiver needs to type.
    Send(SendArgs),
    /// Receive files, folders or text from a sender.
    Receive {
        /// The code shown by the sender, e.g. 7-cedar-river. Asked for if omitted.
        #[arg(conflicts_with = "from")]
        code: Option<String>,

        /// Receive from this paired device instead, without a code (see
        /// `portal pair`). It runs `portal send ... --to <this device>`.
        #[arg(long, value_name = "DEVICE")]
        from: Option<String>,

        /// Folder to save into (defaults to the one set with `portal dir`, or
        /// the current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Overwrite existing files without asking.
        #[arg(long)]
        overwrite: bool,

        /// Accept the transfer without asking first.
        #[arg(short, long)]
        yes: bool,

        /// If a text message arrives, copy it to the clipboard instead of
        /// printing it.
        #[arg(long)]
        copy: bool,

        /// Receive at most this fast, e.g. 500k or 10MB/s.
        #[arg(long, value_name = "SPEED", value_parser = ui::parse_rate)]
        limit: Option<u64>,
    },
    /// Pair with another device once, so you can send between the two later
    /// without a code. Run it without a code on one device, and with the
    /// code it shows on the other.
    Pair {
        /// The code shown by `portal pair` on the other device.
        code: Option<String>,

        /// The name the other device will know this one by (default: this
        /// computer's name).
        #[arg(long)]
        name: Option<String>,
    },
    /// List paired devices.
    Devices,
    /// Forget a paired device.
    Unpair {
        /// Its name, as `portal devices` shows it.
        device: String,
    },
    /// Check what works on this network and what that means for transfers.
    Doctor,
    /// Update Portal to the newest version.
    Update {
        /// Only check whether a newer version exists.
        #[arg(long)]
        check: bool,

        /// Reinstall the newest version even if it is already installed.
        #[arg(long, conflicts_with = "check")]
        force: bool,
    },
    /// Add "Send with Portal" to the right-click menu of Explorer, Finder or
    /// the Linux file manager.
    ContextMenu {
        /// Remove it again.
        #[arg(long)]
        remove: bool,
    },
    /// Show or set the folder received files are saved into.
    Dir {
        /// Folder to save received files into from now on. Shows the current
        /// one if omitted.
        path: Option<PathBuf>,

        /// Go back to saving into the current directory.
        #[arg(long, conflicts_with = "path")]
        reset: bool,
    },
    /// Print a shell completion script.
    ///
    /// PowerShell: portal completions powershell >> $PROFILE
    /// Bash:       portal completions bash > ~/.local/share/bash-completion/completions/portal
    /// Zsh:        portal completions zsh > ~/.zfunc/_portal
    #[command(verbatim_doc_comment)]
    Completions {
        /// The shell to write the script for.
        shell: clap_complete::Shell,
    },
}

/// `portal send` options.
#[derive(Debug, clap::Args)]
struct SendArgs {
    /// Files and folders to send; several are sent together.
    #[arg(required_unless_present_any = ["text", "clipboard"], conflicts_with_all = ["text", "clipboard"])]
    paths: Vec<PathBuf>,

    /// Send this text instead of files ("-" reads it from standard input).
    #[arg(long, conflicts_with = "clipboard")]
    text: Option<String>,

    /// Send the text on the clipboard.
    #[arg(long)]
    clipboard: bool,

    /// Send to this paired device, without a code (see `portal pair`).
    #[arg(long, value_name = "DEVICE")]
    to: Option<String>,

    /// Send at most this fast, e.g. 500k or 10MB/s.
    #[arg(long, value_name = "SPEED", value_parser = ui::parse_rate)]
    limit: Option<u64>,

    /// Skip files ignored by .gitignore / .ignore rules.
    #[arg(long)]
    respect_gitignore: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    update::clean_up();
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

    // Not while changing the menu itself.
    if !matches!(cli.command, Command::ContextMenu { .. }) {
        menu::heal();
    }

    let rendezvous = cli.rendezvous.as_deref();
    let pause = cli.pause;
    let result = match cli.command {
        Command::Send(args) => send(args, pause, rendezvous, &net).await,
        Command::Receive {
            code,
            from,
            output,
            overwrite,
            yes,
            copy,
            limit,
        } => {
            let options = ReceiveFlags {
                output: output.map(arg_path),
                from_menu: pause,
                overwrite,
                yes,
                copy,
                limit,
            };
            receive(code, from, options, rendezvous, &net).await
        }
        Command::Pair { code, name } => pair(code, name, rendezvous, &net).await,
        Command::Devices => devices(),
        Command::Unpair { device } => unpair(&device),
        Command::Doctor => doctor::run(rendezvous).await,
        Command::Update { check, force } => update::run(check, force).await,
        Command::ContextMenu { remove } => menu::run(remove),
        Command::Dir { path, reset } => dir(path.as_deref(), reset),
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "portal", &mut std::io::stdout());
            Ok(())
        }
    };
    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e:#}", style("Error:").red().bold());
            ExitCode::FAILURE
        }
    };
    if pause {
        ui::pause(code == ExitCode::SUCCESS);
    }
    code
}

/// Network settings shared by both commands.
struct Net {
    mode: NetMode,
    discovery: Discovery,
    /// Passed through to the command printed for the receiver.
    dht_bootstrap: Vec<String>,
}

/// The command-line flags that make the receiver find this sender.
fn receiver_flags(net: &Net) -> String {
    let mut flags = String::new();
    if net.mode == NetMode::Loopback {
        flags.push_str(" --loopback");
    }
    for node in &net.dht_bootstrap {
        flags.push_str(&format!(" --dht-bootstrap {node}"));
    }
    flags
}

/// What `portal send` sends. Files are still being read when this returns.
async fn payload(args: &SendArgs) -> anyhow::Result<Pending> {
    let text = if let Some(text) = &args.text {
        Some(if text == "-" {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("could not read the text from standard input")?;
            text
        } else {
            text.clone()
        })
    } else if args.clipboard {
        Some(clipboard::get()?)
    } else {
        None
    };
    if let Some(text) = text {
        if text.trim().is_empty() {
            bail!("there is no text to send");
        }
        if text.len() > MAX_TEXT {
            bail!(
                "the text is too long ({} kB; at most {} kB). Save it to a file and send that instead.",
                text.len() / 1000,
                MAX_TEXT / 1000
            );
        }
        let chars = text.chars().count();
        let noun = if chars == 1 {
            "character"
        } else {
            "characters"
        };
        println!("Sending a text message ({chars} {noun})");
        return Ok(Pending::Ready(Payload::Text(text)));
    }

    // Finding the files is quick; reading them is not, so that goes on in
    // the background while the receiver types the code.
    let options = ScanOptions {
        respect_gitignore: args.respect_gitignore,
    };
    let paths = args.paths.clone();
    let plan = tokio::task::spawn_blocking(move || portal_core::plan(&paths, &options))
        .await
        .context("scanning failed")??;
    warn_skipped(plan.skipped());
    let summary = describe(plan.file_count(), plan.total_bytes(), plan.is_single_file());
    let name = match plan.root_count() {
        1 => style(format!("\"{}\"", plan.name())).bold().to_string(),
        n => format!(
            "{} and {} more",
            style(format!("\"{}\"", plan.name())).bold(),
            n - 1
        ),
    };
    println!("Sending {name} ({summary})");

    let bar = ui::reading_bar(plan.total_bytes());
    let warned = plan.skipped().len();
    let read = bar.clone();
    let task = tokio::task::spawn_blocking(move || plan.read(move |bytes| read.inc(bytes)));
    Ok(Pending::Reading(Reading { task, bar, warned }))
}

fn warn_skipped(skipped: &[portal_core::scan::Skipped]) {
    for skipped in skipped.iter().take(MAX_SKIPPED_SHOWN) {
        let what = skipped
            .path
            .as_deref()
            .map_or_else(|| "an entry".to_owned(), |p| p.display().to_string());
        ui::warn(format!("skipped {what}: {}", skipped.reason));
    }
    if skipped.len() > MAX_SKIPPED_SHOWN {
        ui::warn(format!(
            "…and {} more skipped",
            skipped.len() - MAX_SKIPPED_SHOWN
        ));
    }
}

/// What `portal send` sends: ready, or files still being read.
enum Pending {
    Ready(Payload),
    Reading(Reading),
}

type ScanResult = Result<portal_core::Scan, portal_core::scan::ScanError>;

/// Files being read on a blocking thread.
struct Reading {
    task: tokio::task::JoinHandle<ScanResult>,
    bar: indicatif::ProgressBar,
    /// How many skipped entries were already reported.
    warned: usize,
}

impl Pending {
    /// Shows how far reading has got, if it is not done yet.
    fn show_progress(&self) {
        if let Self::Reading(reading) = self
            && !reading.task.is_finished()
        {
            reading
                .bar
                .set_draw_target(indicatif::ProgressDrawTarget::stderr());
        }
    }

    /// Waits for `connect` and for the files to be read, whichever takes
    /// longer. Fails as soon as either fails.
    async fn ready<T>(
        self,
        connect: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<(T, Payload)> {
        let mut reading = match self {
            Self::Ready(payload) => return Ok((connect.await?, payload)),
            Self::Reading(reading) => reading,
        };
        tokio::pin!(connect);
        tokio::select! {
            read = &mut reading.task => {
                let payload = reading.finish(read)?;
                Ok((connect.await?, payload))
            }
            connected = &mut connect => {
                let connected = connected?;
                let read = (&mut reading.task).await;
                Ok((connected, reading.finish(read)?))
            }
        }
    }
}

impl Reading {
    fn finish(
        &mut self,
        read: Result<ScanResult, tokio::task::JoinError>,
    ) -> anyhow::Result<Payload> {
        self.bar.finish_and_clear();
        let scan = read.context("scanning failed")??;
        warn_skipped(scan.skipped.get(self.warned..).unwrap_or_default());
        Ok(Payload::from(scan))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

async fn send(
    mut args: SendArgs,
    from_menu: bool,
    rendezvous: Option<&str>,
    net: &Net,
) -> anyhow::Result<()> {
    args.paths = args.paths.into_iter().map(arg_path).collect();
    if args.to.is_some() && rendezvous.is_some() {
        bail!("--to sends to a paired device directly; it cannot be combined with --rendezvous");
    }
    // Check the device name before scanning what may be a large folder.
    let device = args.to.as_deref().map(config::device).transpose()?;
    let pending = payload(&args).await?;
    let options = SendOptions {
        rate_limit: args.limit,
    };
    if let Some(limit) = args.limit {
        println!("Speed limit: {}", ui::rate(limit));
    }

    let mode = net.mode;
    let key = device.as_ref().map(|_| config::identity()).transpose()?;
    let endpoint = portal_net::bind_as(mode, true, key).await?;
    let mut flags = receiver_flags(net);

    let progress = Progress::new();
    let explained = |e| explain(e, rendezvous, Role::Send);
    // Each way of connecting yields the connection and the payload, read
    // while the receiver connected.
    let result: anyhow::Result<Payload> = match (&device, rendezvous) {
        (Some(device), _) => {
            let me = config::own_name()?.unwrap_or_else(ui::hostname);
            println!(
                "Waiting for {}. On it, run:\n\n  portal receive{flags} --from {}\n",
                style(&device.name).bold(),
                quote_arg(&me)
            );
            pending.show_progress();
            let announcement = portal_net::trust::announce(&endpoint, &net.discovery);
            async {
                let accept = async {
                    tokio::time::timeout(
                        SEND_TIMEOUT,
                        portal_net::accept_from(&endpoint, device.id),
                    )
                    .await
                    .map_err(|_| NetError::Timeout("waiting for the device"))?
                    .map_err(explained)
                };
                let (conn, payload) = pending.ready(accept).await?;
                drop(announcement);
                portal_net::send_on(&conn, &payload, &options, progress.reporter(Role::Send))
                    .await
                    .map_err(explained)?;
                Ok(payload)
            }
            .await
        }
        (None, Some(url)) => {
            let waiting = portal_net::rendezvous::allocate(url)
                .await
                .map_err(explained)?;
            flags.push_str(&format!(" --rendezvous {url}"));
            show_code(&waiting.code().to_string(), &flags, from_menu);
            pending.show_progress();
            async {
                let accept = async {
                    let peer = waiting.pair(&endpoint, mode).await.map_err(explained)?;
                    portal_net::accept_paired(&endpoint, peer)
                        .await
                        .map_err(explained)
                };
                let (conn, payload) = pending.ready(accept).await?;
                portal_net::send_on(&conn, &payload, &options, progress.reporter(Role::Send))
                    .await
                    .map_err(explained)?;
                Ok(payload)
            }
            .await
        }
        (None, None) => {
            let code = Code::random()?;
            show_code(&code.to_string(), &flags, from_menu);
            pending.show_progress();
            let announcement =
                portal_net::discovery::announce(&endpoint, mode, &code, &net.discovery);
            async {
                let accept = async {
                    tokio::time::timeout(SEND_TIMEOUT, portal_net::direct::accept(&endpoint, &code))
                        .await
                        .map_err(|_| NetError::Timeout("waiting for the receiver"))?
                        .map_err(explained)
                };
                let (conn, payload) = pending.ready(accept).await?;
                drop(announcement);
                portal_net::send_on(&conn, &payload, &options, progress.reporter(Role::Send))
                    .await
                    .map_err(explained)?;
                Ok(payload)
            }
            .await
        }
    };
    let stats = progress.finish();
    endpoint.close().await;
    match result? {
        Payload::Text(_) => ui::success("Sent the text"),
        Payload::Files { manifest, .. } => ui::success(format!(
            "Sent {}{}",
            describe(
                manifest.file_count(),
                manifest.total_bytes(),
                manifest.is_single_file()
            ),
            Stats::suffix(stats)
        )),
    }
    Ok(())
}

/// Quotes a device name for the command line if it needs it.
fn quote_arg(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        arg.to_owned()
    } else {
        format!("\"{arg}\"")
    }
}

/// Shows the code and how to use it. From the file manager's menu, the code
/// is also copied, since it is usually pasted into a message next.
fn show_code(code: &str, flags: &str, copy: bool) {
    println!("Code: {}", style(code).bold().cyan());
    println!("\nOn the other machine, run:\n\n  portal receive{flags} {code}\n");
    if copy && clipboard::set(code).is_ok() {
        println!("The code is copied to the clipboard.");
    }
    println!("Waiting for receiver...");
}

/// Undoes a Windows quirk: `"D:\"` on a command line arrives as `D:"` (the
/// backslash escapes the quote), e.g. from the right-click menu on a drive.
/// A path cannot end in a quote there, so it was a backslash.
fn arg_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_suffix('"')) {
        Some(p) if cfg!(windows) => PathBuf::from(format!("{p}\\")),
        _ => path,
    }
}

/// `portal receive` options besides the code and network settings.
struct ReceiveFlags {
    output: Option<PathBuf>,
    /// Started from the file manager's menu.
    from_menu: bool,
    overwrite: bool,
    yes: bool,
    copy: bool,
    limit: Option<u64>,
}

/// Who `portal receive` gets its transfer from.
enum Sender {
    Code(Code),
    Device(config::Device),
}

async fn receive(
    code: Option<String>,
    from: Option<String>,
    flags: ReceiveFlags,
    rendezvous: Option<&str>,
    net: &Net,
) -> anyhow::Result<()> {
    let ReceiveFlags {
        output,
        from_menu,
        overwrite,
        yes,
        copy,
        limit,
    } = flags;
    let from = match (from, code) {
        (Some(_), _) if rendezvous.is_some() => bail!(
            "--from receives from a paired device directly; it cannot be combined with --rendezvous"
        ),
        (Some(name), _) => Sender::Device(config::device(&name)?),
        // Parse before touching the network: a typo caught here does not
        // use up the sender's one attempt.
        (None, Some(code)) => Sender::Code(Code::parse(&code)?),
        (None, None) => Sender::Code(Code::parse(&ask_for_code()?)?),
    };
    let dest = match output {
        Some(dir) => dir,
        None => match config::download_dir()? {
            Some(dir) => dir,
            None => std::env::current_dir().context("could not determine the current folder")?,
        },
    };

    // Ask only when someone can answer; scripts and pipes accept as before.
    let confirm: Option<Confirm> = if yes || !ui::can_ask() {
        None
    } else {
        let question = format!("Save it into {}?", dest.display());
        Some(Arc::new(move |_: &Offer| ui::confirm(&question)))
    };

    let key = match &from {
        Sender::Device(_) => Some(config::identity()?),
        Sender::Code(_) => None,
    };
    let endpoint = portal_net::bind_as(net.mode, false, key).await?;
    match &from {
        Sender::Device(device) => println!("Waiting for {} to send...", style(&device.name).bold()),
        Sender::Code(_) => println!("Connecting..."),
    }
    let options = ReceiveOptions {
        dest: dest.clone(),
        overwrite: if overwrite {
            OverwritePolicy::Always
        } else {
            OverwritePolicy::Never
        },
        confirm,
        rate_limit: limit,
    };
    let progress = Progress::new();
    let reporter = progress.reporter(Role::Receive);
    let result = async {
        match (&from, rendezvous) {
            (Sender::Device(device), _) => {
                let sender = EndpointAddr::from(device.id);
                let conn =
                    portal_net::trust::dial(&endpoint, sender, &net.discovery, DEVICE_TIMEOUT)
                        .await?;
                portal_net::receive_on(&conn, options, reporter).await
            }
            (Sender::Code(code), Some(url)) => {
                let sender = portal_net::rendezvous::join(url, code, &endpoint).await?;
                portal_net::receive(&endpoint, sender, options, reporter).await
            }
            (Sender::Code(code), None) => {
                let sender =
                    portal_net::discovery::find(code, &net.discovery, FIND_TIMEOUT).await?;
                let conn = portal_net::direct::connect(&endpoint, sender, code).await?;
                portal_net::receive_on(&conn, options, reporter).await
            }
        }
    }
    .await;
    let stats = progress.finish();
    endpoint.close().await;
    let summary = match result {
        Ok(Received::Files(summary)) => summary,
        Ok(Received::Text(text)) => return show_text(&text, copy, from_menu),
        Err(NetError::Cancelled) => {
            println!("Declined. Nothing was saved.");
            return Ok(());
        }
        Err(NetError::DeviceNotFound) => {
            let name = match &from {
                Sender::Device(device) => device.name.as_str(),
                Sender::Code(_) => "the device",
            };
            bail!(
                "{name} did not start sending. Run `portal send <files> --to <this device>` on {name}, then try again."
            );
        }
        Err(e) => {
            let e = explain(e, rendezvous, Role::Receive);
            // Data arrived before the failure and was kept for a retry.
            if stats.is_some() && dest.join(PARTIAL_DIR_NAME).exists() {
                return Err(anyhow!(
                    "{e:#}\n\nWhat arrived so far was kept. If the sender runs `portal send` again with the same files, only the rest is transferred."
                ));
            }
            return Err(e);
        }
    };
    ui::success(format!(
        "Received {} into {}{}",
        describe(summary.files, summary.bytes, false),
        dest.display(),
        Stats::suffix(stats),
    ));
    Ok(())
}

/// Prints a received text message, or copies it to the clipboard. From the
/// file manager's menu, it does both and keeps the window open.
fn show_text(text: &str, copy: bool, from_menu: bool) -> anyhow::Result<()> {
    if copy {
        clipboard::set(text)?;
        let chars = text.chars().count();
        ui::success(format!(
            "Received a text message ({chars} characters) and copied it to the clipboard"
        ));
        return Ok(());
    }
    // A file or pipe gets the exact text; a terminal gets it made harmless.
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        ui::success("Received a text message:\n");
        println!("{}", ui::terminal_safe(text));
        if from_menu {
            ui::keep_open();
            if clipboard::set(text).is_ok() {
                println!("\n(Also copied to the clipboard.)");
            }
        }
    } else {
        print!("{text}");
    }
    Ok(())
}

/// `portal pair`: shows a code (without `code`) or uses the other device's.
async fn pair(
    code: Option<String>,
    name: Option<String>,
    rendezvous: Option<&str>,
    net: &Net,
) -> anyhow::Result<()> {
    if rendezvous.is_some() {
        bail!(
            "pairing finds the other device without a rendezvous server; run it without --rendezvous"
        );
    }
    let code = code.map(|c| Code::parse(&c)).transpose()?;
    let my_name =
        portal_net::trust::clean_name(&name.or(config::own_name()?).unwrap_or_else(ui::hostname));
    let key = config::identity()?;
    let endpoint = portal_net::bind_as(net.mode, code.is_none(), Some(key)).await?;
    let result = match code {
        None => {
            let code = Code::random()?;
            println!("Pairing code: {}", style(code.to_string()).bold().cyan());
            println!(
                "\nOn the other device, run:\n\n  portal pair{} {code}\n",
                receiver_flags(net)
            );
            println!("Waiting for the other device...");
            let announcement =
                portal_net::discovery::announce(&endpoint, net.mode, &code, &net.discovery);
            let paired = tokio::time::timeout(
                SEND_TIMEOUT,
                portal_net::trust::pair_accept(&endpoint, &code, &my_name),
            )
            .await
            .map_err(|_| NetError::Timeout("waiting for the other device"));
            drop(announcement);
            paired.and_then(|r| r)
        }
        Some(code) => {
            println!("Connecting...");
            async {
                let peer = portal_net::discovery::find(&code, &net.discovery, FIND_TIMEOUT).await?;
                portal_net::trust::pair_join(&endpoint, peer, &code, &my_name).await
            }
            .await
        }
    };
    endpoint.close().await;
    let paired = result.map_err(|e| match e {
        NetError::Pake(PakeError::WrongCode) => anyhow!(
            "the code was wrong, so the devices were not paired. Run `portal pair` again for a new code."
        ),
        NetError::SenderNotFound => anyhow!(
            "no device found for this code. Check the number at the start, and that `portal pair` is still running on the other device."
        ),
        other => anyhow::Error::new(other),
    })?;
    config::set_own_name(&my_name)?;
    let saved = config::add_device(&paired.name, paired.id)?;
    menu::refresh_devices();
    ui::success(format!(
        "Paired with {}. This device is \"{my_name}\" to it.",
        style(&saved).bold()
    ));
    println!(
        "\nSend to it:        portal send <files> --to {}\nReceive from it:   portal receive --from {}",
        quote_arg(&saved),
        quote_arg(&saved)
    );
    Ok(())
}

/// `portal devices`: lists paired devices.
fn devices() -> anyhow::Result<()> {
    let devices = config::devices()?;
    if devices.is_empty() {
        println!("No paired devices. Pair one with `portal pair`.");
        return Ok(());
    }
    if let Some(me) = config::own_name()? {
        println!("This device: {}\n", style(me).bold());
    }
    println!("Paired devices:");
    for device in devices {
        println!(
            "  {:<24} {}",
            device.name,
            style(device.id.fmt_short()).dim()
        );
    }
    Ok(())
}

/// `portal unpair`: forgets a paired device.
fn unpair(name: &str) -> anyhow::Result<()> {
    let removed = config::remove_device(name)?;
    menu::refresh_devices();
    ui::success(format!(
        "Forgot {removed}. It can no longer send to or receive from this device without a code."
    ));
    Ok(())
}

/// `portal dir`: shows, sets or resets the saved download folder.
fn dir(path: Option<&Path>, reset: bool) -> anyhow::Result<()> {
    if reset {
        config::reset_download_dir()?;
        ui::success("Received files will be saved into the current directory");
    } else if let Some(path) = path {
        let saved = config::set_download_dir(path)?;
        ui::success(format!(
            "Received files will be saved into {}",
            saved.display()
        ));
    } else {
        match config::download_dir()? {
            Some(dir) => println!("{}", dir.display()),
            None => println!("Not set: received files are saved into the current directory."),
        }
    }
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
            "the receiver already has different files with these names. They can re-run with --overwrite or --output."
        ),
        NetError::Declined(DeclineReason::Rejected) => {
            anyhow!("the receiver declined the transfer.")
        }
        NetError::Connect(_) => anyhow::Error::new(e).context(
            "Could not reach the sender. Check that `portal send` is still running and that neither network blocks UDP (`portal doctor` checks this network)",
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

    #[cfg(windows)]
    #[test]
    fn a_drive_quoted_by_the_menu_keeps_its_backslash() {
        assert_eq!(arg_path(PathBuf::from("D:\"")), PathBuf::from(r"D:\"));
        assert_eq!(
            arg_path(PathBuf::from(r"C:\Users\me")),
            PathBuf::from(r"C:\Users\me")
        );
    }
}
