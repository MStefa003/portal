//! Finding the other peer with a short code.
//!
//! The sender [`allocate`]s a nameplate and shows the [`Code`]; the receiver
//! [`join`]s with it. Both then run the [`portal_proto::pake`] handshake
//! through the rendezvous server, which ends with each side holding the other's
//! authenticated iroh identity:
//! - the receiver learns the sender's address (node ID plus how to reach it)
//!   and dials it; iroh's TLS guarantees it reached that node ID;
//! - the sender learns the receiver's node ID and accepts only that peer.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use portal_proto::code::Code;
use portal_proto::pake::{self, PeerMsg, Role};
use portal_proto::rendezvous::{
    ClientMsg, MAX_RELAY_PAYLOAD, MAX_RENDEZVOUS_MESSAGE, MIN_RENDEZVOUS_VERSION,
    RENDEZVOUS_VERSION, ServerMsg, compatible,
};
use portal_proto::{PakeError, RendezvousError};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::NetMode;
use crate::endpoint::local_addr;
use crate::error::NetError;

/// Connecting to the rendezvous server and the version check.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The handshake once both peers are connected.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A sender holding a nameplate, waiting for its receiver.
pub struct Waiting {
    ws: Ws,
    code: Code,
}

impl std::fmt::Debug for Waiting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiting").field("code", &self.code).finish()
    }
}

/// Sender: reserves a nameplate at the server `url` and makes up a code.
pub async fn allocate(url: &str) -> Result<Waiting, NetError> {
    let mut ws = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let mut ws = connect(url).await?;
        send(&mut ws, &ClientMsg::Allocate).await?;
        Ok::<_, NetError>(ws)
    })
    .await
    .map_err(|_| NetError::Timeout("reaching the rendezvous server"))??;
    let nameplate = match recv(&mut ws).await? {
        ServerMsg::Allocated { nameplate } => nameplate,
        _ => return Err(NetError::Protocol("expected Allocated".into())),
    };
    let code = Code::generate(nameplate).map_err(|e| NetError::Internal(e.to_string()))?;
    Ok(Waiting { ws, code })
}

impl Waiting {
    /// The code to show to the person sending.
    pub fn code(&self) -> &Code {
        &self.code
    }

    /// Waits until a receiver joins and proves it knows the code, then
    /// returns its node ID. A wrong code ends the attempt: the code is used up.
    pub async fn pair(
        mut self,
        endpoint: &Endpoint,
        mode: NetMode,
    ) -> Result<EndpointId, NetError> {
        match recv(&mut self.ws).await? {
            ServerMsg::Paired => {}
            _ => return Err(NetError::Protocol("expected Paired".into())),
        }
        let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let (sealer, opener) = exchange_pake(&mut self.ws, &self.code, Role::Sender).await?;
            // Snapshot the address now: the endpoint has had time to find
            // its relay and public addresses while the code was being typed.
            let me = postcard::to_allocvec(&local_addr(endpoint, mode).await)
                .map_err(|e| NetError::Internal(e.to_string()))?;
            relay(&mut self.ws, &sealer.seal(&me)).await?;
            let theirs = opener.open(recv_peer(&mut self.ws).await?)?;
            postcard::from_bytes::<EndpointId>(&theirs)
                .map_err(|_| NetError::Protocol("invalid receiver identity".into()))
        })
        .await
        .map_err(|_| NetError::Timeout("the code handshake"))?;
        let _ = self.ws.close(None).await;
        result
    }
}

/// Receiver: joins the sender waiting on `code` at the server `url`, proves
/// it knows the code, and returns the sender's address.
pub async fn join(url: &str, code: &Code, endpoint: &Endpoint) -> Result<EndpointAddr, NetError> {
    let mut ws = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let mut ws = connect(url).await?;
        send(
            &mut ws,
            &ClientMsg::Claim {
                nameplate: code.nameplate(),
            },
        )
        .await?;
        match recv(&mut ws).await? {
            ServerMsg::Paired => Ok(ws),
            _ => Err(NetError::Protocol("expected Paired".into())),
        }
    })
    .await
    .map_err(|_| NetError::Timeout("reaching the rendezvous server"))??;

    let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let (sealer, opener) = exchange_pake(&mut ws, code, Role::Receiver).await?;
        let me =
            postcard::to_allocvec(&endpoint.id()).map_err(|e| NetError::Internal(e.to_string()))?;
        relay(&mut ws, &sealer.seal(&me)).await?;
        let theirs = opener.open(recv_peer(&mut ws).await?)?;
        postcard::from_bytes::<EndpointAddr>(&theirs)
            .map_err(|_| NetError::Protocol("invalid sender address".into()))
    })
    .await
    .map_err(|_| NetError::Timeout("the code handshake"))?;
    let _ = ws.close(None).await;
    result
}

/// Sends our SPAKE2 message, reads the peer's, and derives the sealing keys.
async fn exchange_pake(
    ws: &mut Ws,
    code: &Code,
    me: Role,
) -> Result<(pake::Sealer, pake::Opener), NetError> {
    let (handshake, msg) = pake::start(code);
    relay(ws, &msg).await?;
    let theirs = recv_peer(ws).await?;
    Ok(handshake.finish(theirs, me)?)
}

/// Opens the WebSocket and exchanges versions with the server.
async fn connect(url: &str) -> Result<Ws, NetError> {
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        return Err(NetError::RendezvousUrl);
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_RENDEZVOUS_MESSAGE))
        .max_frame_size(Some(MAX_RENDEZVOUS_MESSAGE));
    let (mut ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), true)
        .await
        .map_err(|e| NetError::RendezvousConnect(Box::new(e)))?;
    send(
        &mut ws,
        &ClientMsg::Hello {
            version: RENDEZVOUS_VERSION,
            min_version: MIN_RENDEZVOUS_VERSION,
        },
    )
    .await?;
    match recv(&mut ws).await? {
        ServerMsg::Welcome {
            version,
            min_version,
        } if compatible(
            version,
            min_version,
            RENDEZVOUS_VERSION,
            MIN_RENDEZVOUS_VERSION,
        ) =>
        {
            Ok(ws)
        }
        ServerMsg::Welcome { .. } => Err(NetError::Rendezvous(RendezvousError::Version)),
        _ => Err(NetError::Protocol("expected Welcome".into())),
    }
}

async fn send(ws: &mut Ws, msg: &ClientMsg) -> Result<(), NetError> {
    let bytes = postcard::to_allocvec(msg).map_err(|e| NetError::Internal(e.to_string()))?;
    ws.send(Message::Binary(bytes.into()))
        .await
        .map_err(|_| NetError::RendezvousLost)
}

/// The next message from the server. Server errors become [`NetError`]s.
async fn recv(ws: &mut Ws) -> Result<ServerMsg, NetError> {
    loop {
        let msg = match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => postcard::from_bytes(&bytes)
                .map_err(|_| NetError::Protocol("invalid rendezvous message".into()))?,
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Ok(Message::Text(_))) => {
                return Err(NetError::Protocol("invalid rendezvous message".into()));
            }
            Some(Ok(Message::Close(_)) | Err(_)) | None => return Err(NetError::RendezvousLost),
        };
        return match msg {
            ServerMsg::Error(e) => Err(NetError::Rendezvous(e)),
            ServerMsg::PeerLeft => Err(NetError::PeerLeft),
            other => Ok(other),
        };
    }
}

/// Sends `msg` to the peer through the server.
async fn relay(ws: &mut Ws, msg: &PeerMsg) -> Result<(), NetError> {
    let bytes = postcard::to_allocvec(msg).map_err(|e| NetError::Internal(e.to_string()))?;
    if bytes.len() > MAX_RELAY_PAYLOAD {
        return Err(NetError::Internal("handshake message too large".into()));
    }
    send(ws, &ClientMsg::Relay(bytes)).await
}

/// The next message from the peer.
async fn recv_peer(ws: &mut Ws) -> Result<PeerMsg, NetError> {
    match recv(ws).await? {
        ServerMsg::Relay(bytes) => {
            postcard::from_bytes(&bytes).map_err(|_| NetError::Pake(PakeError::Malformed))
        }
        _ => Err(NetError::Protocol("expected a relayed message".into())),
    }
}
