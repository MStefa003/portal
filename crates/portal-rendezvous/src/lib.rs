//! The Portal rendezvous server: pairs a sender and a receiver by the number
//! at the front of their code, then relays a few small handshake messages.
//!
//! It never learns the code words, keys, file names or file contents: the
//! relayed payloads are a SPAKE2 handshake and messages sealed with its key.
//! Once the peers have each other's addresses they hang up and connect
//! directly. See `docs/PROTOCOL.md` for the message flow.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use portal_proto::code::MAX_NAMEPLATE;
use portal_proto::rendezvous::{
    ClientMsg, MAX_RELAY_PAYLOAD, MAX_RELAYED_MESSAGES, MAX_RENDEZVOUS_MESSAGE,
    MIN_RENDEZVOUS_VERSION, RENDEZVOUS_PATH, RENDEZVOUS_VERSION, RendezvousError, ServerMsg,
    compatible,
};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc, oneshot};

/// How long a fresh connection may take to say what it wants.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Keeps idle connections alive through proxies while a sender waits.
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Nameplates are drawn from `1..=max(MIN_RANGE, 4 × waiting senders)`.
const MIN_RANGE: u32 = 99;

/// Server limits.
#[derive(Debug, Clone)]
pub struct Config {
    /// How long a sender may wait for a receiver.
    pub nameplate_ttl: Duration,
    /// How long a paired handshake may take.
    pub pairing_ttl: Duration,
    /// Maximum number of senders waiting at once.
    pub max_waiting: usize,
    /// Maximum number of open WebSocket connections.
    pub max_connections: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nameplate_ttl: Duration::from_secs(60 * 60),
            pairing_ttl: Duration::from_secs(60),
            max_waiting: 10_000,
            max_connections: 20_000,
        }
    }
}

/// Serves rendezvous requests on `listener` until an I/O error occurs.
pub async fn serve(listener: TcpListener, config: Config) -> std::io::Result<()> {
    axum::serve(listener, router(config)).await
}

/// The HTTP routes: the WebSocket endpoint at [`RENDEZVOUS_PATH`] and a
/// plain-text health check at `/`.
pub fn router(config: Config) -> Router {
    let shared = Arc::new(Shared {
        connections: Arc::new(Semaphore::new(config.max_connections)),
        config,
        waiting: Mutex::new(Waiting::default()),
    });
    Router::new()
        .route("/", get(|| async { "portal-rendezvous\n" }))
        .route(RENDEZVOUS_PATH, get(upgrade))
        .with_state(shared)
}

struct Shared {
    config: Config,
    connections: Arc<Semaphore>,
    waiting: Mutex<Waiting>,
}

/// Senders waiting for a receiver, by nameplate.
#[derive(Default)]
struct Waiting {
    by_nameplate: HashMap<u32, (u64, oneshot::Sender<Pipe>)>,
    next_id: u64,
}

/// One side's view of a pairing.
struct Pipe {
    to_peer: mpsc::Sender<Vec<u8>>,
    from_peer: mpsc::Receiver<Vec<u8>>,
}

/// How a session ended badly.
enum Fail {
    /// The client went away; nothing to tell it.
    Gone,
    /// Tell the client this, then close.
    Reply(RendezvousError),
}

impl From<RendezvousError> for Fail {
    fn from(e: RendezvousError) -> Self {
        Self::Reply(e)
    }
}

async fn upgrade(ws: WebSocketUpgrade, State(shared): State<Arc<Shared>>) -> Response {
    let permit = shared.connections.clone().try_acquire_owned();
    ws.max_message_size(MAX_RENDEZVOUS_MESSAGE)
        .max_frame_size(MAX_RENDEZVOUS_MESSAGE)
        .on_upgrade(move |mut socket| async move {
            let result = match permit {
                Ok(_permit) => session(&mut socket, &shared).await,
                Err(_) => Err(Fail::Reply(RendezvousError::Busy)),
            };
            if let Err(Fail::Reply(e)) = result {
                tracing::debug!(%e, "session failed");
                let _ = send(&mut socket, &ServerMsg::Error(e)).await;
            }
            let _ = socket.send(Message::Close(None)).await;
        })
}

async fn session(ws: &mut WebSocket, shared: &Shared) -> Result<(), Fail> {
    let hello = tokio::time::timeout(HELLO_TIMEOUT, recv(ws))
        .await
        .map_err(|_| RendezvousError::Protocol)??;
    let ClientMsg::Hello {
        version,
        min_version,
    } = hello
    else {
        return Err(RendezvousError::Protocol.into());
    };
    if !compatible(
        version,
        min_version,
        RENDEZVOUS_VERSION,
        MIN_RENDEZVOUS_VERSION,
    ) {
        return Err(RendezvousError::Version.into());
    }
    send(
        ws,
        &ServerMsg::Welcome {
            version: RENDEZVOUS_VERSION,
            min_version: MIN_RENDEZVOUS_VERSION,
        },
    )
    .await?;

    let request = tokio::time::timeout(HELLO_TIMEOUT, recv(ws))
        .await
        .map_err(|_| RendezvousError::Protocol)??;
    match request {
        ClientMsg::Allocate => host(ws, shared).await,
        ClientMsg::Claim { nameplate } => claim(ws, shared, nameplate).await,
        _ => Err(RendezvousError::Protocol.into()),
    }
}

/// A sender: reserve a nameplate, wait for a receiver, then relay.
async fn host(ws: &mut WebSocket, shared: &Shared) -> Result<(), Fail> {
    let (paired_tx, mut paired_rx) = oneshot::channel();
    let (nameplate, id) = shared.allocate(paired_tx)?;
    let _release = Release {
        shared,
        nameplate,
        id,
    };
    send(ws, &ServerMsg::Allocated { nameplate }).await?;

    let expiry = tokio::time::sleep(shared.config.nameplate_ttl);
    tokio::pin!(expiry);
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.reset();
    let pipe = loop {
        tokio::select! {
            pipe = &mut paired_rx => match pipe {
                Ok(pipe) => break pipe,
                // Only happens if the entry vanished without a pairing.
                Err(_) => return Err(RendezvousError::Expired.into()),
            },
            // The sender has nothing to say until it is paired.
            msg = recv(ws) => return match msg {
                Err(Fail::Gone) => Err(Fail::Gone),
                _ => Err(RendezvousError::Protocol.into()),
            },
            () = &mut expiry => return Err(RendezvousError::Expired.into()),
            _ = ping.tick() => {
                ws.send(Message::Ping(Default::default())).await.map_err(|_| Fail::Gone)?;
            }
        }
    };
    send(ws, &ServerMsg::Paired).await?;
    relay(ws, pipe, shared.config.pairing_ttl).await
}

/// A receiver: join the sender on `nameplate`, then relay.
async fn claim(ws: &mut WebSocket, shared: &Shared, nameplate: u32) -> Result<(), Fail> {
    // Taking the entry makes the nameplate single-use: a second claimant,
    // such as someone guessing, finds nothing.
    let host = shared
        .take(nameplate)
        .ok_or(RendezvousError::UnknownNameplate)?;
    let (to_host, from_claimant) = mpsc::channel(MAX_RELAYED_MESSAGES);
    let (to_claimant, from_host) = mpsc::channel(MAX_RELAYED_MESSAGES);
    host.send(Pipe {
        to_peer: to_claimant,
        from_peer: from_claimant,
    })
    .map_err(|_| RendezvousError::UnknownNameplate)?;
    send(ws, &ServerMsg::Paired).await?;
    let pipe = Pipe {
        to_peer: to_host,
        from_peer: from_host,
    };
    relay(ws, pipe, shared.config.pairing_ttl).await
}

/// Forwards payloads both ways until either side leaves or `ttl` passes.
async fn relay(ws: &mut WebSocket, mut pipe: Pipe, ttl: Duration) -> Result<(), Fail> {
    let deadline = tokio::time::sleep(ttl);
    tokio::pin!(deadline);
    let mut relayed = 0;
    loop {
        tokio::select! {
            msg = recv(ws) => {
                let ClientMsg::Relay(payload) = msg? else {
                    return Err(RendezvousError::Protocol.into());
                };
                relayed += 1;
                if payload.len() > MAX_RELAY_PAYLOAD || relayed > MAX_RELAYED_MESSAGES {
                    return Err(RendezvousError::Protocol.into());
                }
                // The channel holds MAX_RELAYED_MESSAGES, so this never waits.
                if pipe.to_peer.send(payload).await.is_err() {
                    send(ws, &ServerMsg::PeerLeft).await?;
                    return Ok(());
                }
            }
            payload = pipe.from_peer.recv() => match payload {
                Some(payload) => send(ws, &ServerMsg::Relay(payload)).await?,
                None => {
                    send(ws, &ServerMsg::PeerLeft).await?;
                    return Ok(());
                }
            },
            () = &mut deadline => return Err(RendezvousError::Expired.into()),
        }
    }
}

/// Reads the next client message. Cancel-safe: a message is either fully
/// read or still waiting in the socket.
async fn recv(ws: &mut WebSocket) -> Result<ClientMsg, Fail> {
    loop {
        match ws.recv().await {
            Some(Ok(Message::Binary(bytes))) => {
                return postcard::from_bytes(&bytes).map_err(|_| RendezvousError::Protocol.into());
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Text(_))) => return Err(RendezvousError::Protocol.into()),
            Some(Ok(Message::Close(_)) | Err(_)) | None => return Err(Fail::Gone),
        }
    }
}

async fn send(ws: &mut WebSocket, msg: &ServerMsg) -> Result<(), Fail> {
    let bytes = postcard::to_allocvec(msg).map_err(|_| RendezvousError::Protocol)?;
    ws.send(Message::Binary(bytes.into()))
        .await
        .map_err(|_| Fail::Gone)
}

impl Shared {
    /// Reserves a free nameplate for a waiting sender.
    fn allocate(&self, paired: oneshot::Sender<Pipe>) -> Result<(u32, u64), RendezvousError> {
        let mut waiting = self.waiting.lock().map_err(|_| RendezvousError::Busy)?;
        if waiting.by_nameplate.len() >= self.config.max_waiting {
            return Err(RendezvousError::Busy);
        }
        // Small numbers make short codes; the range grows with demand so a
        // free number is quick to find.
        let count = u32::try_from(waiting.by_nameplate.len()).unwrap_or(MAX_NAMEPLATE);
        let range = count.saturating_mul(4).clamp(MIN_RANGE, MAX_NAMEPLATE);
        let nameplate = loop {
            let n = 1 + getrandom::u32().map_err(|_| RendezvousError::Busy)? % range;
            if !waiting.by_nameplate.contains_key(&n) {
                break n;
            }
        };
        let id = waiting.next_id;
        waiting.next_id += 1;
        waiting.by_nameplate.insert(nameplate, (id, paired));
        Ok((nameplate, id))
    }

    fn take(&self, nameplate: u32) -> Option<oneshot::Sender<Pipe>> {
        let mut waiting = self.waiting.lock().ok()?;
        waiting.by_nameplate.remove(&nameplate).map(|(_, tx)| tx)
    }
}

/// Frees a sender's nameplate when its session ends, unless it was claimed
/// (and possibly reused by a newer sender) in the meantime.
struct Release<'a> {
    shared: &'a Shared,
    nameplate: u32,
    id: u64,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        if let Ok(mut waiting) = self.shared.waiting.lock()
            && waiting
                .by_nameplate
                .get(&self.nameplate)
                .is_some_and(|(id, _)| *id == self.id)
        {
            waiting.by_nameplate.remove(&self.nameplate);
        }
    }
}
