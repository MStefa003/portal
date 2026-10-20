//! Receiver side of a transfer. See `send.rs` for the message sequence.

use std::path::PathBuf;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use portal_core::manifest::MAX_MANIFEST_BYTES;
use portal_core::{Hash, Manifest, OverwritePolicy, Sink, SinkError, Summary};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{
    ALPN, Control, DeclineReason, EncodedChunk, MAX_CONTROL_FRAME, Offer, WANT_BATCH, sanitize,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::error::NetError;
use crate::event::{Event, Reporter};
use crate::{endpoint, hang_up, hello};

/// How long to wait for the sender to close after we said `Done`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Verified-but-unwritten chunks buffered between the network and the disk.
const WRITE_QUEUE: usize = 64;

/// Where and how to write a received transfer.
#[derive(Debug, Clone)]
pub struct ReceiveOptions {
    /// Folder to receive into.
    pub dest: PathBuf,
    /// What to do about existing files.
    pub overwrite: OverwritePolicy,
}

/// Connects to the sender at `sender` and receives its transfer.
pub async fn receive(
    endpoint: &Endpoint,
    sender: EndpointAddr,
    options: ReceiveOptions,
    reporter: Reporter,
) -> Result<Summary, NetError> {
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
    reporter(Event::Connected {
        peer: conn.remote_id().fmt_short().to_string(),
        route: endpoint::route(&conn),
    });

    let result = run(&conn, options, &reporter).await;
    match &result {
        // The sender closes once it has read our `Done`; waiting for that
        // makes sure `Done` was delivered before we exit.
        Ok(_) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, conn.closed()).await;
        }
        Err(_) => hang_up(&conn, &result).await,
    }
    result
}

async fn run(
    conn: &Connection,
    options: ReceiveOptions,
    reporter: &Reporter,
) -> Result<Summary, NetError> {
    let (mut tx, mut rx) = conn.open_bi().await.map_err(NetError::connection)?;
    hello(&mut tx, &mut rx).await?;
    let (offer, manifest) = read_offer(&mut rx).await?;
    reporter(Event::Offer(offer));

    let ReceiveOptions { dest, overwrite } = options;
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

    let outcome = receive_chunks(conn, &mut tx, rx, sink, reporter).await;
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
    outcome
}

/// Reads the `Offer` and the manifest that follows, checking that they agree.
async fn read_offer(rx: &mut RecvStream) -> Result<(Offer, Manifest), NetError> {
    let offer = match read_msg(rx, MAX_CONTROL_FRAME).await? {
        Some(Control::Offer(offer)) => offer,
        Some(Control::Error { message, .. }) => return Err(NetError::Peer(sanitize(&message))),
        Some(_) => return Err(NetError::Protocol("expected Offer".into())),
        None => return Err(NetError::PeerClosed),
    };
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
    if manifest.name() != offer.name
        || manifest.file_count() != offer.files
        || manifest.total_bytes() != offer.bytes
    {
        return Err(NetError::Protocol(
            "the offer does not match the manifest".into(),
        ));
    }
    Ok((offer, manifest))
}

/// Requests every missing chunk and feeds arrivals into the sink until done.
async fn receive_chunks(
    conn: &Connection,
    tx: &mut SendStream,
    rx: RecvStream,
    sink: Sink,
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
    background.spawn(accept_streams(conn.clone(), chunk_tx, reporter.clone()));
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
    reporter: Reporter,
) -> Result<(), NetError> {
    let mut readers = JoinSet::new();
    loop {
        tokio::select! {
            stream = conn.accept_uni() => {
                let stream = stream.map_err(NetError::connection)?;
                readers.spawn(read_stream(stream, chunk_tx.clone(), reporter.clone()));
            }
            Some(joined) = readers.join_next() => joined.map_err(NetError::join)??,
        }
    }
}

async fn read_stream(
    mut stream: RecvStream,
    chunk_tx: mpsc::Sender<(Hash, Vec<u8>)>,
    reporter: Reporter,
) -> Result<(), NetError> {
    while let Some(chunk) = EncodedChunk::read(&mut stream).await? {
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
