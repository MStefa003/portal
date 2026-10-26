//! Sender side of a transfer.
//!
//! ```text
//! receiver                         sender
//!    │── open control stream ──────►│
//!    │◄──────── Hello ─────────────►│
//!    │◄─ Offer, ManifestPart × n ───│
//!    │── Accept ───────────────────►│
//!    │── Want × n, WantDone ───────►│   hashes go into a shared queue
//!    │◄═ chunks on 16 data streams ═│   workers: read → zstd → write
//!    │── Done ─────────────────────►│
//!    │◄──────── close ──────────────│
//! ```
//!
//! A text message (protocol 2+) replaces everything after `Hello` with one
//! `Text`, answered by `Done` (or `Decline`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};
use iroh::{Endpoint, EndpointId};
use portal_core::{Hash, Manifest, OpenFile, Source, SourceError};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{
    ALPN, Control, EncodedChunk, Encoding, ErrorCode, MANIFEST_PART_SIZE, MAX_CONTROL_FRAME,
    MAX_TEXT, Offer, sanitize,
};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use crate::error::NetError;
use crate::event::{Event, Reporter};
use crate::limit::{self, RateLimit};
use crate::{endpoint, hang_up, hello};

/// Parallel data streams per transfer. Each stream's flow-control window is
/// kept small (see `endpoint.rs`), so it takes this many to keep a fast,
/// distant link full; they also keep it full while some workers wait on disk
/// or compression.
pub(crate) const DATA_STREAMS: usize = 16;
/// How long the paired receiver has to connect (hole punching included).
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(120);

type ControlResult = Result<Option<Control>, portal_proto::FrameError>;

/// What to send.
#[derive(Debug, Clone)]
pub enum Payload {
    /// Scanned files and folders.
    Files {
        /// The scanned paths on disk ([`portal_core::Scan::roots`]).
        roots: Vec<PathBuf>,
        /// Their manifest.
        manifest: Manifest,
    },
    /// A text message of at most [`MAX_TEXT`] bytes.
    Text(String),
}

impl From<portal_core::Scan> for Payload {
    fn from(scan: portal_core::Scan) -> Self {
        Self::Files {
            roots: scan.roots,
            manifest: scan.manifest,
        }
    }
}

/// How to send.
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// At most this many bytes per second on the network; `None` for no limit.
    pub rate_limit: Option<u64>,
}

/// Waits for the receiver `peer` (paired through a rendezvous server) to
/// connect to `endpoint`, then [`send_on`] that connection. Connections from
/// any other node are refused.
pub async fn send(
    endpoint: &Endpoint,
    peer: EndpointId,
    payload: &Payload,
    options: &SendOptions,
    reporter: Reporter,
) -> Result<(), NetError> {
    let conn = accept_paired(endpoint, peer).await?;
    send_on(&conn, payload, options, reporter).await
}

/// The first half of [`send`]: waits for the receiver `peer` to connect, for
/// as long as hole punching may take.
pub async fn accept_paired(endpoint: &Endpoint, peer: EndpointId) -> Result<Connection, NetError> {
    tokio::time::timeout(ACCEPT_TIMEOUT, accept_from(endpoint, peer))
        .await
        .map_err(|_| NetError::Timeout("waiting for the receiver to connect"))?
}

/// Sends `payload` to the already-authenticated receiver on `conn`, then
/// hangs up.
pub async fn send_on(
    conn: &Connection,
    payload: &Payload,
    options: &SendOptions,
    reporter: Reporter,
) -> Result<(), NetError> {
    reporter(Event::Connected {
        peer: conn.remote_id().fmt_short().to_string(),
        route: endpoint::route(conn),
    });
    let result = serve(conn, payload, options, &reporter).await;
    hang_up(conn, &result).await;
    result
}

/// Waits for a connection from `peer` with [`ALPN`] and returns it; any
/// other connection is closed and ignored. For peers whose identity is
/// already known: paired by a rendezvous server, or a trusted device.
pub async fn accept_from(endpoint: &Endpoint, peer: EndpointId) -> Result<Connection, NetError> {
    loop {
        let incoming = endpoint
            .accept()
            .await
            .ok_or_else(|| NetError::Internal("the endpoint was closed".into()))?;
        match incoming.await {
            Ok(conn) if conn.remote_id() == peer && conn.alpn() == ALPN => return Ok(conn),
            Ok(conn) => {
                tracing::warn!("refused a connection from a peer that did not use the code");
                conn.close(2u32.into(), b"unexpected peer");
            }
            Err(e) => tracing::warn!(%e, "an incoming connection failed; still waiting"),
        }
    }
}

async fn serve(
    conn: &Connection,
    payload: &Payload,
    options: &SendOptions,
    reporter: &Reporter,
) -> Result<(), NetError> {
    let (mut tx, mut rx) = conn.accept_bi().await.map_err(NetError::connection)?;
    let version = hello(&mut tx, &mut rx).await?;
    let (roots, manifest) = match payload {
        Payload::Text(text) => {
            if text.len() > MAX_TEXT {
                return Err(NetError::TextTooLong);
            }
            needs_version(&mut tx, version, "text messages").await?;
            write_msg(&mut tx, &Control::Text(text.clone())).await?;
            reporter(Event::Started {
                total_bytes: text.len() as u64,
                skipped_bytes: 0,
            });
            return read_reply(&mut rx, Control::Done).await;
        }
        Payload::Files { roots, manifest } => (roots, manifest),
    };
    if manifest.root_count() > 1 {
        needs_version(&mut tx, version, "several items at once").await?;
    }

    let manifest_bytes = manifest.to_bytes()?;
    let offer = Offer {
        name: manifest.display_name(),
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
        manifest_len: manifest_bytes.len() as u64,
        transfer_id: Hash::of(&manifest_bytes),
    };
    write_msg(&mut tx, &Control::Offer(offer)).await?;
    for part in manifest_bytes.chunks(MANIFEST_PART_SIZE) {
        write_msg(&mut tx, &Control::ManifestPart(part.to_vec())).await?;
    }
    read_reply(&mut rx, Control::Accept).await?;

    let limit = RateLimit::new(options.rate_limit).map(Arc::new);
    let outcome = stream_chunks(conn, rx, roots, manifest, limit, reporter).await;
    if let Err(e) = &outcome {
        let (code, message) = e.for_peer();
        let msg = Control::Error {
            code,
            message: message.into(),
        };
        // Best effort: the connection may already be gone.
        let _ = write_msg(&mut tx, &msg).await;
    }
    outcome
}

/// Fails, telling the receiver why, if the negotiated protocol `version`
/// cannot carry `what` (protocol 2 features: text, several items).
async fn needs_version(
    tx: &mut SendStream,
    version: u16,
    what: &'static str,
) -> Result<(), NetError> {
    if version >= 2 {
        return Ok(());
    }
    let msg = Control::Error {
        code: ErrorCode::Protocol,
        message: format!(
            "the sender is sending {what}, which needs a newer Portal. Please update Portal here."
        ),
    };
    let _ = write_msg(tx, &msg).await;
    Err(NetError::PeerTooOld(what))
}

/// Reads the receiver's answer, which should be `expected` (`Accept` or `Done`).
async fn read_reply(
    rx: &mut iroh::endpoint::RecvStream,
    expected: Control,
) -> Result<(), NetError> {
    match read_msg(rx, MAX_CONTROL_FRAME).await? {
        Some(msg) if msg == expected => Ok(()),
        Some(Control::Decline(reason)) => Err(NetError::Declined(reason)),
        Some(Control::Error { message, .. }) => Err(NetError::Peer(sanitize(&message))),
        Some(_) => Err(NetError::Protocol(format!(
            "expected {expected:?} or Decline"
        ))),
        None => Err(NetError::PeerClosed),
    }
}

/// Serves `Want`s until the receiver says `Done`.
async fn stream_chunks(
    conn: &Connection,
    mut rx: iroh::endpoint::RecvStream,
    roots: &[PathBuf],
    manifest: &Manifest,
    limit: Option<Arc<RateLimit>>,
    reporter: &Reporter,
) -> Result<(), NetError> {
    // Frame reads are not cancel-safe, so the control stream is read by its
    // own task and we `select!` on the channel instead.
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ControlResult>(16);
    let mut background = JoinSet::new();
    background.spawn(async move {
        loop {
            let msg = read_msg::<_, Control>(&mut rx, MAX_CONTROL_FRAME).await;
            let last = !matches!(msg, Ok(Some(_)));
            if ctl_tx.send(msg).await.is_err() || last {
                break;
            }
        }
    });

    let chunks = Arc::new(Chunks::new(Source::new(roots, manifest)));
    let (want_tx, want_rx) = mpsc::unbounded_channel::<Hash>();
    let queue = Arc::new(Mutex::new(want_rx));
    let mut workers = JoinSet::new();
    for _ in 0..DATA_STREAMS {
        workers.spawn(worker(
            conn.clone(),
            chunks.clone(),
            queue.clone(),
            limit.clone(),
            reporter.clone(),
        ));
    }

    // Chunk sizes, to tell the progress display how much will be sent.
    let sizes: HashMap<Hash, u32> = manifest
        .files()
        .flat_map(|(_, _, file)| file.chunks.iter().map(|c| (c.hash, c.len)))
        .collect();
    let mut wanted_bytes = 0u64;

    let mut want_tx = Some(want_tx);
    loop {
        tokio::select! {
            msg = ctl_rx.recv() => match msg {
                Some(Ok(Some(Control::Want(hashes)))) => {
                    let Some(tx) = &want_tx else {
                        return Err(NetError::Protocol("Want after WantDone".into()));
                    };
                    for hash in hashes {
                        wanted_bytes += sizes.get(&hash).copied().map_or(0, u64::from);
                        // Workers only stop early on error, which surfaces below.
                        let _ = tx.send(hash);
                    }
                }
                // Dropping the sender lets workers drain the queue and finish.
                Some(Ok(Some(Control::WantDone))) => {
                    want_tx = None;
                    reporter(Event::Started {
                        total_bytes: wanted_bytes,
                        skipped_bytes: manifest.total_bytes().saturating_sub(wanted_bytes),
                    });
                }
                Some(Ok(Some(Control::Done))) => return Ok(()),
                Some(Ok(Some(Control::Error { message, .. }))) => {
                    return Err(NetError::Peer(sanitize(&message)));
                }
                Some(Ok(Some(_))) => return Err(NetError::Protocol("unexpected message".into())),
                Some(Ok(None)) | None => return Err(NetError::PeerClosed),
                Some(Err(e)) => return Err(e.into()),
            },
            Some(joined) = workers.join_next() => joined.map_err(NetError::join)??,
        }
    }
}

/// Pulls hashes from the queue and writes the chunks to one data stream,
/// reading and compressing the next chunk while the current one is written.
async fn worker(
    conn: Connection,
    chunks: Arc<Chunks>,
    queue: Queue,
    limit: Option<Arc<RateLimit>>,
    reporter: Reporter,
) -> Result<(), NetError> {
    // Opened lazily, so tiny transfers do not open idle streams.
    let mut stream: Option<SendStream> = None;
    let mut next = prepare(&chunks, &queue, OpenFile::default()).await;
    while let Some(task) = next {
        let (chunk, open) = task.await.map_err(NetError::join)?;
        let chunk = chunk?;
        let s = match &mut stream {
            Some(s) => s,
            None => stream.insert(conn.open_uni().await.map_err(NetError::connection)?),
        };
        limit::take(limit.as_deref(), chunk.payload.len()).await;
        let (written, following) = tokio::join!(chunk.write(s), prepare(&chunks, &queue, open));
        written?;
        reporter(Event::Progress {
            bytes: u64::from(chunk.raw_len),
        });
        next = following;
    }
    if let Some(mut s) = stream {
        s.finish().map_err(NetError::connection)?;
    }
    Ok(())
}

/// The chunks still to send, shared by the workers.
type Queue = Arc<Mutex<mpsc::UnboundedReceiver<Hash>>>;
/// A chunk being read and compressed, and the file it was read from.
type Prepared = tokio::task::JoinHandle<(Result<EncodedChunk, SourceError>, OpenFile)>;

/// Takes the next hash from the queue and starts reading and compressing
/// its chunk on a blocking thread; `None` once the queue is done.
async fn prepare(chunks: &Arc<Chunks>, queue: &Queue, mut open: OpenFile) -> Option<Prepared> {
    let hash = queue.lock().await.recv().await?;
    let chunks = chunks.clone();
    Some(tokio::task::spawn_blocking(move || {
        let chunk = chunks
            .source
            .read_chunk_with(&hash, &mut open)
            .map(|raw| chunks.encode(hash, raw));
        (chunk, open)
    }))
}

/// Chunks served from disk, compressed where that pays off.
struct Chunks {
    source: Source,
    /// Files with a chunk that did not get smaller: most likely already
    /// compressed (video, images, archives), so their other chunks are sent
    /// as they are instead of spending time on zstd.
    incompressible: Vec<AtomicBool>,
}

impl Chunks {
    fn new(source: Source) -> Self {
        let incompressible = (0..source.file_count())
            .map(|_| AtomicBool::new(false))
            .collect();
        Self {
            source,
            incompressible,
        }
    }

    fn encode(&self, hash: Hash, raw: Vec<u8>) -> EncodedChunk {
        let flag = self
            .source
            .file_of(&hash)
            .and_then(|file| self.incompressible.get(file));
        if flag.is_some_and(|f| f.load(Ordering::Relaxed)) {
            return EncodedChunk::raw(hash, raw);
        }
        let chunk = EncodedChunk::compress(hash, raw);
        if chunk.encoding == Encoding::Raw
            && let Some(flag) = flag
        {
            flag.store(true, Ordering::Relaxed);
        }
        chunk
    }
}
