//! Pairing on the connection itself, with no rendezvous server.
//!
//! The receiver dials whatever address [`crate::discovery`] found, using
//! [`PAIR_ALPN`], and opens a stream for the [`portal_proto::pake`]
//! handshake. Each side seals its own node ID and checks the one it opens
//! against the node ID that the connection's TLS authenticated. So a fake
//! discovery record, or a machine relaying the handshake between the real
//! peers, makes the handshake fail instead of letting anyone in the middle.
//!
//! The sender handles one connection at a time and gives up for good after
//! the first attempt that got as far as a handshake message: that attempt
//! was a guess at the code, and a code allows one guess.

use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use portal_proto::code::Code;
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::pake::{self, PeerMsg, Role};
use portal_proto::{PAIR_ALPN, PakeError};

use crate::error::NetError;

/// Largest handshake frame: a SPAKE2 message or a sealed node ID, with room.
const MAX_PAKE_FRAME: usize = 4096;
/// How long a connection may take to send its first handshake message
/// before the sender drops it and waits for another.
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// The rest of the handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Sender: waits for a receiver that proves it knows `code`, and returns
/// its connection. Fails as soon as one attempt uses the wrong code.
pub async fn accept(endpoint: &Endpoint, code: &Code) -> Result<Connection, NetError> {
    loop {
        let incoming = endpoint
            .accept()
            .await
            .ok_or_else(|| NetError::Internal("the endpoint was closed".into()))?;
        let conn = match incoming.await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!(%e, "an incoming connection failed; still waiting");
                continue;
            }
        };
        if conn.alpn() != PAIR_ALPN {
            conn.close(2u32.into(), b"pair first");
            continue;
        }
        match handshake_as_sender(&conn, code, endpoint.id()).await {
            Ok(()) => return Ok(conn),
            Err(Attempt::NoGuess(e)) => {
                tracing::warn!(%e, "a connection left before trying the code; still waiting");
                conn.close(1u32.into(), b"handshake");
            }
            Err(Attempt::Guessed(e)) => {
                conn.close(1u32.into(), b"wrong code");
                return Err(e);
            }
        }
    }
}

/// How a sender-side attempt failed.
enum Attempt {
    /// Before the peer's first handshake message: the code was not tried.
    NoGuess(NetError),
    /// After it: the code counts as guessed.
    Guessed(NetError),
}

async fn handshake_as_sender(
    conn: &Connection,
    code: &Code,
    me: EndpointId,
) -> Result<(), Attempt> {
    let (mut tx, mut rx) = tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| Attempt::NoGuess(NetError::Timeout("the code handshake")))?
        .map_err(|e| Attempt::NoGuess(NetError::connection(e)))?;
    let (state, mine) = pake::start(code);
    let theirs = tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, async {
        write_msg(&mut tx, &mine).await?;
        read_peer(&mut rx).await
    })
    .await
    .map_err(|_| Attempt::NoGuess(NetError::Timeout("the code handshake")))?
    .map_err(Attempt::NoGuess)?;

    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let (sealer, opener) = state.finish(theirs, Role::Sender)?;
        confirm(&mut tx, &mut rx, sealer, opener, me, conn.remote_id()).await
    })
    .await
    .map_err(|_| NetError::Timeout("the code handshake"))
    .and_then(|r| r)
    .map_err(Attempt::Guessed)
}

/// Receiver: connects to `sender` and proves it knows `code`.
pub async fn connect(
    endpoint: &Endpoint,
    sender: EndpointAddr,
    code: &Code,
) -> Result<Connection, NetError> {
    let expected = sender.id;
    let conn = endpoint
        .connect(sender, PAIR_ALPN)
        .await
        .map_err(NetError::connect)?;
    if conn.remote_id() != expected {
        conn.close(2u32.into(), b"unexpected peer");
        return Err(NetError::WrongPeer);
    }
    let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let (mut tx, mut rx) = conn.open_bi().await.map_err(NetError::connection)?;
        let (state, mine) = pake::start(code);
        write_msg(&mut tx, &mine).await?;
        let theirs = read_peer(&mut rx).await?;
        let (sealer, opener) = state.finish(theirs, Role::Receiver)?;
        confirm(&mut tx, &mut rx, sealer, opener, endpoint.id(), expected).await
    })
    .await
    .map_err(|_| NetError::Timeout("the code handshake"))
    .and_then(|r| r);
    match result {
        Ok(()) => Ok(conn),
        Err(e) => {
            conn.close(1u32.into(), b"handshake");
            Err(e)
        }
    }
}

/// Sends our node ID `me` sealed, opens the peer's, and checks it is `peer`.
///
/// On failure, waits (briefly) until the peer has our sealed message before
/// returning, so it can tell a wrong code from a dropped connection when we
/// hang up.
async fn confirm(
    tx: &mut SendStream,
    rx: &mut RecvStream,
    sealer: pake::Sealer,
    opener: pake::Opener,
    me: EndpointId,
    peer: EndpointId,
) -> Result<(), NetError> {
    let me = postcard::to_allocvec(&me).map_err(|e| NetError::Internal(e.to_string()))?;
    write_msg(tx, &sealer.seal(&me)).await?;
    tx.finish().map_err(NetError::connection)?;
    let result = async {
        let opened = opener.open(read_peer(rx).await?)?;
        let claimed = postcard::from_bytes::<EndpointId>(&opened)
            .map_err(|_| NetError::Pake(PakeError::Malformed))?;
        if claimed != peer {
            // The peer knows the code but is not the node we are talking to:
            // someone is relaying the handshake.
            return Err(NetError::WrongPeer);
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::time::timeout(crate::LINGER, tx.stopped()).await;
    }
    result
}

async fn read_peer(rx: &mut RecvStream) -> Result<PeerMsg, NetError> {
    read_msg(rx, MAX_PAKE_FRAME)
        .await?
        .ok_or(NetError::PeerClosed)
}
