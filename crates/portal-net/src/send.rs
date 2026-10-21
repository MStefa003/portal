//! Sender side of a transfer.
//!
//! ```text
//! receiver                         sender
//!    │── open control stream ──────►│
//!    │◄──────── Hello ─────────────►│
//!    │◄─ Offer, ManifestPart × n ───│
//!    │── Accept ───────────────────►│
//!    │── Want × n, WantDone ───────►│   hashes go into a shared queue
//!    │◄═ chunks on 4 data streams ══│   workers: read → zstd → write
//!    │── Done ─────────────────────►│
//!    │◄──────── close ──────────────│
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};
use iroh::{Endpoint, EndpointId};
use portal_core::{Hash, Manifest, Source};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{
    ALPN, Control, EncodedChunk, MANIFEST_PART_SIZE, MAX_CONTROL_FRAME, Offer, sanitize,
};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use crate::error::NetError;
use crate::event::{Event, Reporter};
use crate::{endpoint, hang_up, hello};

/// Parallel data streams per transfer: enough to keep the pipe full while
/// one worker waits on disk or compression.
const DATA_STREAMS: usize = 4;
/// How long the paired receiver has to connect (hole punching included).
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(120);

type ControlResult = Result<Option<Control>, portal_proto::FrameError>;

/// Waits for the receiver `peer` (paired through a rendezvous server) to
/// connect to `endpoint`, then [`send_on`] that connection. Connections from
/// any other node are refused.
pub async fn send(
    endpoint: &Endpoint,
    peer: EndpointId,
    root: &Path,
    manifest: &Manifest,
    reporter: Reporter,
) -> Result<(), NetError> {
    let conn = tokio::time::timeout(ACCEPT_TIMEOUT, accept_from(endpoint, peer))
        .await
        .map_err(|_| NetError::Timeout("waiting for the receiver to connect"))??;
    send_on(&conn, root, manifest, reporter).await
}

/// Sends `manifest`'s content, read from `root` (the file or folder that was
/// scanned), to the already-authenticated receiver on `conn`, then hangs up.
pub async fn send_on(
    conn: &Connection,
    root: &Path,
    manifest: &Manifest,
    reporter: Reporter,
) -> Result<(), NetError> {
    reporter(Event::Connected {
        peer: conn.remote_id().fmt_short().to_string(),
        route: endpoint::route(conn),
    });
    let result = serve(conn, root, manifest, &reporter).await;
    hang_up(conn, &result).await;
    result
}

/// The first connection from `peer`; others are closed and ignored.
async fn accept_from(endpoint: &Endpoint, peer: EndpointId) -> Result<Connection, NetError> {
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
    root: &Path,
    manifest: &Manifest,
    reporter: &Reporter,
) -> Result<(), NetError> {
    let (mut tx, mut rx) = conn.accept_bi().await.map_err(NetError::connection)?;
    hello(&mut tx, &mut rx).await?;

    let manifest_bytes = manifest.to_bytes()?;
    let offer = Offer {
        name: manifest.name().to_owned(),
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
        manifest_len: manifest_bytes.len() as u64,
        transfer_id: Hash::of(&manifest_bytes),
    };
    write_msg(&mut tx, &Control::Offer(offer)).await?;
    for part in manifest_bytes.chunks(MANIFEST_PART_SIZE) {
        write_msg(&mut tx, &Control::ManifestPart(part.to_vec())).await?;
    }
    match read_msg(&mut rx, MAX_CONTROL_FRAME).await? {
        Some(Control::Accept) => {}
        Some(Control::Decline(reason)) => return Err(NetError::Declined(reason)),
        Some(Control::Error { message, .. }) => return Err(NetError::Peer(sanitize(&message))),
        Some(_) => return Err(NetError::Protocol("expected Accept or Decline".into())),
        None => return Err(NetError::PeerClosed),
    }

    let outcome = stream_chunks(conn, rx, root, manifest, reporter).await;
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

/// Serves `Want`s until the receiver says `Done`.
async fn stream_chunks(
    conn: &Connection,
    mut rx: iroh::endpoint::RecvStream,
    root: &Path,
    manifest: &Manifest,
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

    let source = Arc::new(Source::new(root, manifest));
    let (want_tx, want_rx) = mpsc::unbounded_channel::<Hash>();
    let queue = Arc::new(Mutex::new(want_rx));
    let mut workers = JoinSet::new();
    for _ in 0..DATA_STREAMS {
        workers.spawn(worker(
            conn.clone(),
            source.clone(),
            queue.clone(),
            reporter.clone(),
        ));
    }

    let mut want_tx = Some(want_tx);
    loop {
        tokio::select! {
            msg = ctl_rx.recv() => match msg {
                Some(Ok(Some(Control::Want(hashes)))) => {
                    let Some(tx) = &want_tx else {
                        return Err(NetError::Protocol("Want after WantDone".into()));
                    };
                    for hash in hashes {
                        // Workers only stop early on error, which surfaces below.
                        let _ = tx.send(hash);
                    }
                }
                // Dropping the sender lets workers drain the queue and finish.
                Some(Ok(Some(Control::WantDone))) => want_tx = None,
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

/// Pulls hashes from the queue and writes the chunks to one data stream.
async fn worker(
    conn: Connection,
    source: Arc<Source>,
    queue: Arc<Mutex<mpsc::UnboundedReceiver<Hash>>>,
    reporter: Reporter,
) -> Result<(), NetError> {
    // Opened lazily, so tiny transfers do not open idle streams.
    let mut stream: Option<SendStream> = None;
    loop {
        let next = queue.lock().await.recv().await;
        let Some(hash) = next else { break };

        let source = source.clone();
        let chunk = tokio::task::spawn_blocking(move || {
            source
                .read_chunk(&hash)
                .map(|raw| EncodedChunk::compress(hash, raw))
        })
        .await
        .map_err(NetError::join)??;

        let s = match &mut stream {
            Some(s) => s,
            None => stream.insert(conn.open_uni().await.map_err(NetError::connection)?),
        };
        chunk.write(s).await?;
        reporter(Event::Progress {
            bytes: u64::from(chunk.raw_len),
        });
    }
    if let Some(mut s) = stream {
        s.finish().map_err(NetError::connection)?;
    }
    Ok(())
}
