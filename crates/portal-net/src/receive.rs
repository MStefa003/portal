//! Receiver side of a transfer. See `send.rs` for the message sequence.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use portal_core::manifest::MAX_MANIFEST_BYTES;
use portal_core::{Hash, Manifest, OverwritePolicy, Sink, SinkError, Summary};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{
    ALPN, Control, DeclineReason, EncodedChunk, MAX_CONTROL_FRAME, MAX_TEXT, Offer, WANT_BATCH,
    sanitize,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::error::NetError;
use crate::event::{Event, Reporter};
use crate::limit::{self, RateLimit};
use crate::{endpoint, hang_up, hello};

/// How long to wait for the sender to close after we said `Done`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Verified-but-unwritten chunks buffered between the network and the disk.
const WRITE_QUEUE: usize = 64;

/// Decides whether to accept the sender's offer. Runs on a blocking thread,
/// so it may wait for the user to answer.
pub type Confirm = Arc<dyn Fn(&Offer) -> bool + Send + Sync>;

/// Where and how to write a received transfer.
#[derive(Clone)]
pub struct ReceiveOptions {
    /// Folder to receive into.
    pub dest: PathBuf,
    /// What to do about existing files.
    pub overwrite: OverwritePolicy,
    /// Asked before anything is written; `None` accepts every offer.
    pub confirm: Option<Confirm>,
    /// At most this many bytes per second on the network; `None` for no limit.
    pub rate_limit: Option<u64>,
}

/// What was received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// Files and folders, now in the destination folder.
    Files(Summary),
    /// A text message (untrusted: may contain terminal control characters).
    Text(String),
}

impl fmt::Debug for ReceiveOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceiveOptions")
            .field("dest", &self.dest)
            .field("overwrite", &self.overwrite)
            .field("confirm", &self.confirm.is_some())
            .field("rate_limit", &self.rate_limit)
            .finish()
    }
}

/// Connects to the sender at `sender` (paired through a rendezvous server)
/// and [`receive_on`] that connection.
pub async fn receive(
    endpoint: &Endpoint,
    sender: EndpointAddr,
    options: ReceiveOptions,
    reporter: Reporter,
) -> Result<Received, NetError> {
    let expected = sender.id;
    let conn = endpoint
        .connect(sender, ALPN)
        .await
        .map_err(NetError::connect)?;
    // iroh's TLS already authenticates the node ID we dialed; checked again
    // because everything rests on it.
    if conn.remote_id() != expected {
        conn.close(2u32.into(), b"unexpected peer");
        return Err(NetError::WrongPeer);
    }
    receive_on(&conn, options, reporter).await
}

/// Receives the transfer offered by the already-authenticated sender on
/// `conn`, then hangs up.
pub async fn receive_on(
    conn: &Connection,
    options: ReceiveOptions,
    reporter: Reporter,
) -> Result<Received, NetError> {
    reporter(Event::Connected {
        peer: conn.remote_id().fmt_short().to_string(),
        route: endpoint::route(conn),
    });

    let result = run(conn, options, &reporter).await;
    match &result {
        // The sender closes once it has read our `Done`; waiting for that
        // makes sure `Done` was delivered before we exit.
        Ok(_) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, conn.closed()).await;
        }
        Err(_) => hang_up(conn, &result).await,
    }
    result
}

async fn run(
    conn: &Connection,
    options: ReceiveOptions,
    reporter: &Reporter,
) -> Result<Received, NetError> {
    let (mut tx, mut rx) = conn.open_bi().await.map_err(NetError::connection)?;
    hello(&mut tx, &mut rx).await?;
    let offer = match read_msg(&mut rx, MAX_CONTROL_FRAME).await? {
        Some(Control::Offer(offer)) => offer,
        Some(Control::Text(text)) => {
            if text.len() > MAX_TEXT {
                return Err(NetError::Protocol("the text is too long".into()));
            }
            write_msg(&mut tx, &Control::Done).await?;
            tx.finish().map_err(NetError::connection)?;
            return Ok(Received::Text(text));
        }
        Some(Control::Error { message, .. }) => return Err(NetError::Peer(sanitize(&message))),
        Some(_) => return Err(NetError::Protocol("expected Offer".into())),
        None => return Err(NetError::PeerClosed),
    };
    let manifest = read_manifest(&mut rx, &offer).await?;
    reporter(Event::Offer(offer.clone()));

    let ReceiveOptions {
        dest,
        overwrite,
        confirm,
        rate_limit,
    } = options;
    if let Some(confirm) = confirm {
        let accepted = tokio::task::spawn_blocking(move || confirm(&offer))
            .await
            .map_err(NetError::join)?;
        if !accepted {
            let _ = write_msg(&mut tx, &Control::Decline(DeclineReason::Rejected)).await;
            let _ = tx.finish();
            return Err(NetError::Cancelled);
        }
    }
    let sink = tokio::task::spawn_blocking(move || Sink::new(&dest, manifest, overwrite))
        .await
        .map_err(NetError::join)?;
    let sink = match sink {
        Ok(sink) => sink,
        Err(e) => {
            let reason = match e {
                SinkError::Conflicts(_) | SinkError::TypeConflict(_) | SinkError::Symlink(_) => {
                    DeclineReason::Conflicts
                }
                _ => DeclineReason::Destination,
            };
            let _ = write_msg(&mut tx, &Control::Decline(reason)).await;
            let _ = tx.finish();
            return Err(e.into());
        }
    };
    write_msg(&mut tx, &Control::Accept).await?;

    let limit = RateLimit::new(rate_limit).map(Arc::new);
    let outcome = receive_chunks(conn, &mut tx, rx, sink, limit, reporter).await;
    match &outcome {
        Ok(_) => {
            write_msg(&mut tx, &Control::Done).await?;
            tx.finish().map_err(NetError::connection)?;
        }
        Err(e) => {
            let (code, message) = e.for_peer();
            let msg = Control::Error {
                code,
                message: message.into(),
            };
            let _ = write_msg(&mut tx, &msg).await;
        }
    }
    outcome.map(Received::Files)
}

/// Reads the manifest that follows `offer`, checking that they agree.
async fn read_manifest(rx: &mut RecvStream, offer: &Offer) -> Result<Manifest, NetError> {
    let len = usize::try_from(offer.manifest_len)
        .ok()
        .filter(|&len| len <= MAX_MANIFEST_BYTES)
        .ok_or_else(|| NetError::Protocol("the manifest is too large".into()))?;

    // Grow as parts arrive instead of trusting the announced length up front.
    let mut bytes = Vec::new();
    while bytes.len() < len {
        match read_msg(rx, MAX_CONTROL_FRAME).await? {
            Some(Control::ManifestPart(part)) if !part.is_empty() => {
                if bytes.len() + part.len() > len {
                    return Err(NetError::Protocol(
                        "the manifest is longer than announced".into(),
                    ));
                }
                bytes.extend_from_slice(&part);
            }
            Some(_) => return Err(NetError::Protocol("expected ManifestPart".into())),
            None => return Err(NetError::PeerClosed),
        }
    }
    if Hash::of(&bytes) != offer.transfer_id {
        return Err(NetError::Protocol(
            "the manifest does not match the offer".into(),
        ));
    }
    let manifest = Manifest::from_bytes(&bytes)?;
    if manifest.display_name() != offer.name
        || manifest.file_count() != offer.files
        || manifest.total_bytes() != offer.bytes
    {
        return Err(NetError::Protocol(
            "the offer does not match the manifest".into(),
        ));
    }
    Ok(manifest)
}

/// Requests every missing chunk and feeds arrivals into the sink until done.
async fn receive_chunks(
    conn: &Connection,
    tx: &mut SendStream,
    rx: RecvStream,
    sink: Sink,
    limit: Option<Arc<RateLimit>>,
    reporter: &Reporter,
) -> Result<Summary, NetError> {
    let missing = sink.missing_chunks();
    let total_bytes = sink.missing_bytes();
    reporter(Event::Started {
        total_bytes,
        skipped_bytes: sink.manifest().total_bytes().saturating_sub(total_bytes),
    });
    for batch in missing.chunks(WANT_BATCH) {
        write_msg(tx, &Control::Want(batch.to_vec())).await?;
    }
    write_msg(tx, &Control::WantDone).await?;

    // All disk writes happen on one blocking thread that owns the sink.
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<(Hash, Vec<u8>)>(WRITE_QUEUE);
    let mut writer = tokio::task::spawn_blocking(move || {
        let mut sink = sink;
        while !sink.is_complete() {
            match chunk_rx.blocking_recv() {
                Some((hash, data)) => sink.write_chunk(&hash, &data)?,
                None => break,
            }
        }
        sink.finish()
    });

    let mut background = JoinSet::new();
    background.spawn(accept_streams(
        conn.clone(),
        chunk_tx,
        limit,
        reporter.clone(),
    ));
    background.spawn(watch_control(rx));

    loop {
        tokio::select! {
            written = &mut writer => return Ok(written.map_err(NetError::join)??),
            Some(joined) = background.join_next() => joined.map_err(NetError::join)??,
        }
    }
}

/// Accepts data streams and decodes their chunks into the write queue.
async fn accept_streams(
    conn: Connection,
    chunk_tx: mpsc::Sender<(Hash, Vec<u8>)>,
    limit: Option<Arc<RateLimit>>,
    reporter: Reporter,
) -> Result<(), NetError> {
    let mut readers = JoinSet::new();
    loop {
        tokio::select! {
            stream = conn.accept_uni() => {
                let stream = stream.map_err(NetError::connection)?;
                readers.spawn(read_stream(
                    stream,
                    chunk_tx.clone(),
                    limit.clone(),
                    reporter.clone(),
                ));
            }
            Some(joined) = readers.join_next() => joined.map_err(NetError::join)??,
        }
    }
}

async fn read_stream(
    mut stream: RecvStream,
    chunk_tx: mpsc::Sender<(Hash, Vec<u8>)>,
    limit: Option<Arc<RateLimit>>,
    reporter: Reporter,
) -> Result<(), NetError> {
    while let Some(chunk) = EncodedChunk::read(&mut stream).await? {
        // Reading slower makes QUIC flow control slow the sender down.
        limit::take(limit.as_deref(), chunk.payload.len()).await;
        let raw_len = u64::from(chunk.raw_len);
        let decoded = tokio::task::spawn_blocking(move || chunk.into_raw())
            .await
            .map_err(NetError::join)??;
        if chunk_tx.send(decoded).await.is_err() {
            break; // The writer stopped; its result explains why.
        }
        reporter(Event::Progress { bytes: raw_len });
    }
    Ok(())
}

/// After setup the sender only speaks on the control stream to report errors.
async fn watch_control(mut rx: RecvStream) -> Result<(), NetError> {
    match read_msg(&mut rx, MAX_CONTROL_FRAME).await? {
        Some(Control::Error { message, .. }) => Err(NetError::Peer(sanitize(&message))),
        Some(_) => Err(NetError::Protocol("unexpected message".into())),
        None => Err(NetError::PeerClosed),
    }
}
