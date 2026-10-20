//! Networking for Portal: iroh/QUIC connections and the transfer protocol
//! that moves a [`portal_core::Manifest`] and its chunks between two peers.
//!
//! - [`bind`] creates an iroh endpoint; [`Ticket`] encodes a peer's address.
//! - [`send`] waits for one receiver and serves it.
//! - [`receive`] connects to a sender and writes into a destination folder.
//!
//! Progress is reported through a [`Reporter`] callback so the CLI (and later
//! the GUI) can render it however it likes.
#![forbid(unsafe_code)]

mod endpoint;
mod error;
mod event;
mod receive;
mod send;

pub use endpoint::{NetMode, Ticket, bind, local_ticket, route};
pub use error::NetError;
pub use event::{Event, Reporter, Route};
pub use iroh::Endpoint;
pub use receive::{ReceiveOptions, receive};
pub use send::send;

use std::time::Duration;

use iroh::endpoint::Connection;
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{Control, MAX_CONTROL_FRAME, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};
use tokio::io::{AsyncRead, AsyncWrite};

/// Exchanges `Hello`s and returns the negotiated protocol version.
///
/// Each side writes first, then reads, so neither waits on the other.
async fn hello<W, R>(tx: &mut W, rx: &mut R) -> Result<u16, NetError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    write_msg(
        tx,
        &Control::Hello {
            version: PROTOCOL_VERSION,
            min_version: MIN_PROTOCOL_VERSION,
        },
    )
    .await?;
    match read_msg(rx, MAX_CONTROL_FRAME).await? {
        Some(Control::Hello {
            version,
            min_version,
        }) => Ok(portal_proto::negotiate(version, min_version)?),
        Some(_) => Err(NetError::Protocol("expected Hello".into())),
        None => Err(NetError::PeerClosed),
    }
}

/// How long the side that detected an error waits for the peer to hang up.
const LINGER: Duration = Duration::from_secs(3);

/// Closes `conn` after a transfer ended with `result`.
///
/// Closing a QUIC connection discards data the peer has not received yet. So
/// the side that *detected* a problem (and just sent `Error`/`Decline`)
/// waits for the peer to hang up first; the side that was *told* about it
/// hangs up right away, which ends the other side's wait.
async fn hang_up<T>(conn: &Connection, result: &Result<T, NetError>) {
    match result {
        Ok(_) => conn.close(0u32.into(), b"done"),
        Err(e) if e.came_from_peer() => conn.close(1u32.into(), b"error"),
        Err(_) => {
            let _ = tokio::time::timeout(LINGER, conn.closed()).await;
            conn.close(1u32.into(), b"error");
        }
    }
}
