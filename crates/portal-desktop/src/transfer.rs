//! Sending and receiving, run in the background while the window reports
//! how they are going. The same steps as `portal send` and `portal receive`
//! with a code, so either side can be the command line tool.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::{Stream, StreamExt, stream};
use portal_core::path::PARTIAL_DIR_NAME;
use portal_core::{OverwritePolicy, ScanOptions, SinkError};
use portal_net::{
    Confirm, Discovery, Endpoint, Event, NetError, NetMode, Payload, ReceiveOptions, Received,
    Reporter, Route, SendOptions,
};
use portal_proto::{Code, DeclineReason, Offer, PakeError};

use crate::format::describe;

/// How long a sender waits for its receiver, as in the command line tool.
const SEND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// How long a receiver looks for the sender.
const FIND_TIMEOUT: Duration = Duration::from_secs(60);

/// Which network to use.
#[derive(Debug, Clone)]
pub struct Net {
    /// The real network, or only this machine (for tests).
    pub mode: NetMode,
    /// Where to announce codes and look for them.
    pub discovery: Discovery,
}

impl Net {
    /// The real network: the local network and the public DHT, like the
    /// command line tool without extra flags.
    pub fn public() -> Self {
        Self {
            mode: NetMode::Internet,
            discovery: Discovery::public(),
        }
    }
}

/// Bytes counted as they go, read by the window a few times a second
/// (sending every chunk through [`Update`] would flood it).
#[derive(Debug, Default)]
pub struct Counters {
    /// Bytes of the files read so far (sender only).
    pub read: AtomicU64,
    /// Bytes sent or received so far.
    pub moved: AtomicU64,
}

/// How a transfer is going.
#[derive(Debug, Clone)]
pub enum Update {
    /// The sender knows what it sends: a description and how much to read.
    Preparing {
        /// e.g. `"project" (2 files, 5.00 MB)`.
        what: String,
        /// Bytes to read before the files are ready.
        total: u64,
    },
    /// The code the receiver needs.
    Code(String),
    /// The other side connected.
    Connected(Route),
    /// The sender offers this; the receiver answers through `reply`.
    Offer {
        /// What is offered.
        offer: Offer,
        /// Takes the answer: `true` to accept.
        reply: Reply,
    },
    /// Data is flowing.
    Started {
        /// Bytes that will cross the network.
        total: u64,
        /// Bytes that need no sending (already there or repeated).
        skipped: u64,
    },
    /// The transfer ended.
    Done(Result<Outcome, String>),
}

/// The receiver's answer to an [`Update::Offer`]. Dropping it declines.
#[derive(Debug, Clone)]
pub struct Reply(std::sync::mpsc::Sender<bool>);

impl Reply {
    /// Accepts (`true`) or declines the offer.
    pub fn answer(self, accept: bool) {
        let _ = self.0.send(accept);
    }
}

/// How a transfer that did not fail ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The files were sent.
    Sent {
        /// e.g. "2 files, 5.00 MB".
        what: String,
        /// How many entries could not be read and were left out.
        skipped: usize,
    },
    /// Files arrived in `dest`.
    Files {
        /// e.g. "2 files, 5.00 MB".
        what: String,
        /// The folder they are in.
        dest: PathBuf,
    },
    /// A text message arrived (untrusted).
    Text(String),
    /// The receiver said no; nothing was saved.
    Declined,
}

/// Which side this is, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Send,
    Receive,
}

type Tx = mpsc::UnboundedSender<Update>;

/// Sends `paths`, reporting through the returned stream, which ends with
/// [`Update::Done`].
pub fn send(paths: Vec<PathBuf>, net: Net, counters: Arc<Counters>) -> impl Stream<Item = Update> {
    run(move |tx| async move { send_files(paths, net, counters, &tx).await })
}

/// Receives the transfer for `code` into `dest`, reporting through the
/// returned stream, which ends with [`Update::Done`].
pub fn receive(
    code: Code,
    dest: PathBuf,
    overwrite: bool,
    net: Net,
    counters: Arc<Counters>,
) -> impl Stream<Item = Update> {
    run(move |tx| async move { receive_files(code, dest, overwrite, net, counters, &tx).await })
}

/// Runs `work` on the Tokio runtime, streaming what it reports and then its
/// result. Dropping the stream (when the window cancels) stops the work.
fn run<F, Fut>(work: F) -> impl Stream<Item = Update>
where
    F: FnOnce(Tx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Outcome, String>> + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded();
    let worker = async move {
        // Spawned so blocking steps and the network run on the runtime's
        // worker threads, whatever polls this stream.
        let done = tx.clone();
        let mut task = AbortOnDrop(tokio::spawn(work(tx)));
        let result = match (&mut task.0).await {
            Ok(result) => result,
            Err(_) => Err("the transfer stopped unexpectedly".to_owned()),
        };
        let _ = done.unbounded_send(Update::Done(result));
    };
    stream::select(rx, stream::once(worker).filter_map(|()| async { None }))
}

/// Aborts the task when dropped.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn send_files(
    paths: Vec<PathBuf>,
    net: Net,
    counters: Arc<Counters>,
    tx: &Tx,
) -> Result<Outcome, String> {
    let options = ScanOptions {
        respect_gitignore: false,
    };
    let plan = tokio::task::spawn_blocking(move || portal_core::plan(&paths, &options))
        .await
        .map_err(|_| "looking at the files failed".to_owned())?
        .map_err(|e| e.to_string())?;
    let what = describe(plan.file_count(), plan.total_bytes(), plan.is_single_file());
    let name = match plan.root_count() {
        1 => format!("\"{}\"", plan.name()),
        n => format!("\"{}\" and {} more", plan.name(), n - 1),
    };
    let _ = tx.unbounded_send(Update::Preparing {
        what: format!("{name} ({what})"),
        total: plan.total_bytes(),
    });

    // Reading takes a while, so it goes on while the receiver types the code.
    let read = counters.clone();
    let reading = AbortOnDrop(tokio::task::spawn_blocking(move || {
        plan.read(move |bytes| {
            read.read.fetch_add(bytes, Ordering::Relaxed);
        })
    }));

    let endpoint = portal_net::bind_as(net.mode, true, None)
        .await
        .map_err(|e| explain(e, Role::Send))?;
    let result = serve(&endpoint, &net, reading, counters, tx).await;
    endpoint.close().await;
    let skipped = result?;
    Ok(Outcome::Sent { what, skipped })
}

/// Shows a code, waits for the receiver and the files, and sends them.
/// Returns how many entries were left out.
async fn serve(
    endpoint: &Endpoint,
    net: &Net,
    mut reading: AbortOnDrop<Result<portal_core::Scan, portal_core::scan::ScanError>>,
    counters: Arc<Counters>,
    tx: &Tx,
) -> Result<usize, String> {
    let code = Code::random().map_err(|e| e.to_string())?;
    let _ = tx.unbounded_send(Update::Code(code.to_string()));
    let announcement = portal_net::discovery::announce(endpoint, net.mode, &code, &net.discovery);

    let accept = async {
        tokio::time::timeout(SEND_TIMEOUT, portal_net::direct::accept(endpoint, &code))
            .await
            .unwrap_or(Err(NetError::Timeout("waiting for the receiver")))
            .map_err(|e| explain(e, Role::Send))
    };
    let read = async {
        (&mut reading.0)
            .await
            .map_err(|_| "reading the files failed".to_owned())?
            .map_err(|e| e.to_string())
    };
    let (conn, scan) = tokio::try_join!(accept, read)?;
    drop(announcement);

    let skipped = scan.skipped.len();
    let payload = Payload::from(scan);
    let options = SendOptions { rate_limit: None };
    portal_net::send_on(&conn, &payload, &options, reporter(tx, counters))
        .await
        .map_err(|e| explain(e, Role::Send))?;
    Ok(skipped)
}

async fn receive_files(
    code: Code,
    dest: PathBuf,
    overwrite: bool,
    net: Net,
    counters: Arc<Counters>,
    tx: &Tx,
) -> Result<Outcome, String> {
    let offers = tx.clone();
    // Runs on a blocking thread, so waiting for the answer here is fine.
    let confirm: Confirm = Arc::new(move |offer: &Offer| {
        let (reply, answer) = std::sync::mpsc::channel();
        let update = Update::Offer {
            offer: offer.clone(),
            reply: Reply(reply),
        };
        offers.unbounded_send(update).is_ok() && answer.recv().unwrap_or(false)
    });
    let options = ReceiveOptions {
        dest: dest.clone(),
        overwrite: if overwrite {
            OverwritePolicy::Always
        } else {
            OverwritePolicy::Never
        },
        confirm: Some(confirm),
        rate_limit: None,
    };

    let endpoint = portal_net::bind_as(net.mode, false, None)
        .await
        .map_err(|e| explain(e, Role::Receive))?;
    let result = async {
        let sender = portal_net::discovery::find(&code, &net.discovery, FIND_TIMEOUT).await?;
        let conn = portal_net::direct::connect(&endpoint, sender, &code).await?;
        portal_net::receive_on(&conn, options, reporter(tx, counters.clone())).await
    }
    .await;
    endpoint.close().await;

    match result {
        Ok(Received::Files(summary)) => Ok(Outcome::Files {
            what: describe(summary.files, summary.bytes, false),
            dest,
        }),
        Ok(Received::Text(text)) => Ok(Outcome::Text(text)),
        Err(NetError::Cancelled) => Ok(Outcome::Declined),
        Err(e) => {
            let mut message = explain(e, Role::Receive);
            if counters.moved.load(Ordering::Relaxed) > 0 && kept_partial(&dest) {
                message.push_str(
                    "\n\nWhat arrived so far was kept. If the sender sends the same files again, only the rest is transferred.",
                );
            }
            Err(message)
        }
    }
}

fn kept_partial(dest: &Path) -> bool {
    dest.join(PARTIAL_DIR_NAME).exists()
}

/// Passes milestones on as [`Update`]s and counts bytes in `counters`.
fn reporter(tx: &Tx, counters: Arc<Counters>) -> Reporter {
    let tx = tx.clone();
    Arc::new(move |event| match event {
        Event::Connected { route, .. } => {
            let _ = tx.unbounded_send(Update::Connected(route));
        }
        // The receiver hears of the offer through its confirm callback.
        Event::Offer(_) => {}
        Event::Started {
            total_bytes,
            skipped_bytes,
        } => {
            let _ = tx.unbounded_send(Update::Started {
                total: total_bytes,
                skipped: skipped_bytes,
            });
        }
        Event::Progress { bytes } => {
            counters.moved.fetch_add(bytes, Ordering::Relaxed);
        }
    })
}

/// Turns library errors into advice someone using the window can act on.
fn explain(e: NetError, role: Role) -> String {
    let text = match e {
        NetError::Pake(PakeError::WrongCode) if role == Role::Send => {
            "Someone tried to connect with a wrong code, so the transfer was cancelled to keep your files safe. Send again for a new code.".to_owned()
        }
        NetError::Pake(PakeError::WrongCode) => {
            "The code is wrong. Each code allows one try, so ask the sender for a new code and type it carefully.".to_owned()
        }
        NetError::SenderNotFound => {
            "No sender found for this code. Check the number at the start, and that the sender is still waiting. If they only just started, try again in a few seconds.".to_owned()
        }
        NetError::WrongPeer => {
            "The other side could not prove who it is, so the transfer was stopped. Someone may be interfering; start again with a new code.".to_owned()
        }
        NetError::Sink(SinkError::Conflicts(_)) => format!(
            "{e}. Tick \"Replace files that already exist\", or choose another folder."
        ),
        NetError::Declined(DeclineReason::Conflicts) => {
            "The receiver already has different files with these names.".to_owned()
        }
        NetError::Declined(DeclineReason::Rejected) => {
            "The receiver declined the transfer.".to_owned()
        }
        NetError::Connect(_) => {
            "Could not reach the other computer. Check that it is still waiting, and that neither network blocks UDP.".to_owned()
        }
        other => other.to_string(),
    };
    capitalize(text)
}

fn capitalize(text: String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => first.to_uppercase().chain(chars).collect(),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    use portal_net::DhtNetwork;

    use super::*;

    /// A private DHT on this machine, as in the command line tool's tests:
    /// transfers find each other without touching the real network.
    struct Private {
        _dht: mainline::Testnet,
        net: Net,
        /// The same network as flags for the `portal` command.
        flags: Vec<String>,
    }

    fn private() -> Private {
        let dht = mainline::Testnet::builder(10)
            .build()
            .expect("a private DHT");
        let mut flags = vec!["--loopback".to_owned()];
        for node in &dht.bootstrap {
            flags.extend(["--dht-bootstrap".to_owned(), node.clone()]);
        }
        let net = Net {
            mode: NetMode::Loopback,
            discovery: Discovery {
                mdns: false,
                dht: Some(DhtNetwork::Local(dht.bootstrap.clone())),
            },
        };
        Private {
            _dht: dht,
            net,
            flags,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().expect("a Tokio runtime")
    }

    /// Runs a [`send`] until it shows its code, then lets it finish in the
    /// background.
    async fn start_sending(
        paths: Vec<PathBuf>,
        net: Net,
    ) -> (String, tokio::task::JoinHandle<Result<Outcome, String>>) {
        let mut sending = Box::pin(send(paths, net, Arc::default()));
        let code = loop {
            match sending.next().await {
                Some(Update::Code(code)) => break code,
                Some(Update::Done(result)) => panic!("the sender ended early: {result:?}"),
                Some(_) => {}
                None => panic!("the sender stopped"),
            }
        };
        let rest = tokio::spawn(async move {
            while let Some(update) = sending.next().await {
                if let Update::Done(result) = update {
                    return result;
                }
            }
            Err("the sender stopped without a result".to_owned())
        });
        (code, rest)
    }

    /// Runs a [`receive`] to the end, accepting the offer.
    async fn receive_all(code: &str, dest: &Path, net: Net) -> Result<Outcome, String> {
        let code = Code::parse(code).map_err(|e| e.to_string())?;
        let mut receiving = Box::pin(receive(code, dest.to_owned(), false, net, Arc::default()));
        while let Some(update) = receiving.next().await {
            match update {
                Update::Offer { reply, .. } => reply.answer(true),
                Update::Done(result) => return result,
                _ => {}
            }
        }
        Err("the receiver stopped without a result".to_owned())
    }

    fn project(dir: &Path) -> PathBuf {
        let src = dir.join("project");
        std::fs::create_dir_all(src.join("sub")).expect("a folder");
        std::fs::write(src.join("hello.txt"), "hello from portal").expect("a file");
        std::fs::write(src.join("sub/data.bin"), vec![42u8; 2_000_000]).expect("a file");
        src
    }

    fn assert_project(dest: &Path) {
        let hello = std::fs::read_to_string(dest.join("project/hello.txt"));
        assert_eq!(hello.ok().as_deref(), Some("hello from portal"));
        let data = std::fs::read(dest.join("project/sub/data.bin"));
        assert!(data.is_ok_and(|d| d == vec![42u8; 2_000_000]));
    }

    /// One window sends a folder to another.
    #[test]
    fn sends_and_receives_a_folder() {
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let src = project(tmp.path());
        let dest = tmp.path().join("received");
        let private = private();
        runtime().block_on(async {
            let (code, sending) = start_sending(vec![src], private.net.clone()).await;
            let received = receive_all(&code, &dest, private.net.clone()).await;
            assert!(
                matches!(received, Ok(Outcome::Files { .. })),
                "{received:?}"
            );
            let sent = sending.await.expect("the sender task");
            assert!(
                matches!(sent, Ok(Outcome::Sent { skipped: 0, .. })),
                "{sent:?}"
            );
        });
        assert_project(&dest);
    }

    /// The `portal` command line program to test against, from `PORTAL_CLI`
    /// (CI builds it for this).
    fn cli() -> Option<PathBuf> {
        let cli = std::env::var_os("PORTAL_CLI").map(PathBuf::from);
        if cli.is_none() {
            eprintln!("PORTAL_CLI is not set; skipping");
        }
        cli
    }

    fn portal(cli: &Path, private: &Private, config: &Path, args: &[OsString]) -> Command {
        let mut command = Command::new(cli);
        command
            .args(&private.flags)
            .args(args)
            .env_remove("PORTAL_RENDEZVOUS")
            .env("PORTAL_CONFIG_DIR", config);
        command
    }

    /// Starts `portal send` and returns it with the code it printed.
    fn cli_send(command: &mut Command) -> (std::process::Child, String) {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("portal send starts");
        let stdout = child.stdout.take().expect("its output");
        let (code_tx, code_rx) = std::sync::mpsc::channel();
        // Keeps reading after the code: a closed pipe would fail its next line.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(code) = line.strip_prefix("Code: ") {
                    let _ = code_tx.send(strip_ansi(code).trim().to_owned());
                }
            }
        });
        let code = code_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("portal send printed a code");
        (child, code)
    }

    /// `text` without terminal color codes ("ESC [ ... letter").
    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The window sends, `portal receive` receives.
    #[test]
    fn sends_to_the_command_line_tool() {
        let Some(cli) = cli() else { return };
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let src = project(tmp.path());
        let dest = tmp.path().join("received");
        let config = tmp.path().join("config");
        let private = private();
        runtime().block_on(async {
            let (code, sending) = start_sending(vec![src], private.net.clone()).await;
            let args = [
                "receive".into(),
                code.into(),
                "--yes".into(),
                "--output".into(),
                dest.clone().into(),
            ];
            let mut receiver = portal(&cli, &private, &config, &args);
            let out = tokio::task::spawn_blocking(move || receiver.output())
                .await
                .expect("portal receive ran")
                .expect("portal receive starts");
            assert!(
                out.status.success(),
                "portal receive failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let sent = sending.await.expect("the sender task");
            assert!(matches!(sent, Ok(Outcome::Sent { .. })), "{sent:?}");
        });
        assert_project(&dest);
    }

    /// `portal send` sends files and then a text message; the window receives.
    #[test]
    fn receives_from_the_command_line_tool() {
        let Some(cli) = cli() else { return };
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let src = project(tmp.path());
        let dest = tmp.path().join("received");
        let config = tmp.path().join("config");
        let private = private();
        let rt = runtime();

        let args = ["send".into(), src.into()];
        let (mut sender, code) = cli_send(&mut portal(&cli, &private, &config, &args));
        let received = rt.block_on(receive_all(&code, &dest, private.net.clone()));
        assert!(
            matches!(received, Ok(Outcome::Files { .. })),
            "{received:?}"
        );
        assert!(
            sender.wait().is_ok_and(|s| s.success()),
            "portal send failed"
        );
        assert_project(&dest);

        let args = ["send".into(), "--text".into(), "hello, window".into()];
        let (mut sender, code) = cli_send(&mut portal(&cli, &private, &config, &args));
        let received = rt.block_on(receive_all(&code, &dest, private.net.clone()));
        assert!(
            matches!(&received, Ok(Outcome::Text(text)) if text == "hello, window"),
            "{received:?}"
        );
        assert!(
            sender.wait().is_ok_and(|s| s.success()),
            "portal send failed"
        );
    }
}
