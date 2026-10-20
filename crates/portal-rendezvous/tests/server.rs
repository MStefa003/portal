//! Drives a real server over WebSockets with hand-written clients.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use portal_proto::rendezvous::{
    ClientMsg, MAX_RELAY_PAYLOAD, MAX_RELAYED_MESSAGES, RENDEZVOUS_PATH, RENDEZVOUS_VERSION,
    RendezvousError, ServerMsg,
};
use portal_rendezvous::Config;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start(config: Config) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(portal_rendezvous::serve(listener, config));
    format!("ws://{addr}{RENDEZVOUS_PATH}")
}

async fn send(ws: &mut Ws, msg: &ClientMsg) {
    let bytes = postcard::to_allocvec(msg).unwrap();
    ws.send(Message::Binary(bytes.into())).await.unwrap();
}

/// The next server message, or `None` once the server closed the connection.
async fn recv(ws: &mut Ws) -> Option<ServerMsg> {
    let next = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("server answered in time");
    match next {
        Some(Ok(Message::Binary(b))) => Some(postcard::from_bytes(&b).unwrap()),
        Some(Ok(Message::Close(_)) | Err(_)) | None => None,
        Some(Ok(other)) => panic!("unexpected {other:?}"),
    }
}

async fn connect(url: &str) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    send(
        &mut ws,
        &ClientMsg::Hello {
            version: RENDEZVOUS_VERSION,
            min_version: RENDEZVOUS_VERSION,
        },
    )
    .await;
    assert!(matches!(recv(&mut ws).await, Some(ServerMsg::Welcome { .. })));
    ws
}

async fn allocate(url: &str) -> (Ws, u32) {
    let mut ws = connect(url).await;
    send(&mut ws, &ClientMsg::Allocate).await;
    match recv(&mut ws).await {
        Some(ServerMsg::Allocated { nameplate }) => (ws, nameplate),
        other => panic!("{other:?}"),
    }
}

async fn claim(url: &str, nameplate: u32) -> (Ws, Option<ServerMsg>) {
    let mut ws = connect(url).await;
    send(&mut ws, &ClientMsg::Claim { nameplate }).await;
    let reply = recv(&mut ws).await;
    (ws, reply)
}

#[tokio::test]
async fn pairs_and_relays_both_ways() {
    let url = start(Config::default()).await;
    let (mut host, nameplate) = allocate(&url).await;
    assert!(nameplate >= 1);
    let (mut guest, reply) = claim(&url, nameplate).await;
    assert_eq!(reply, Some(ServerMsg::Paired));
    assert_eq!(recv(&mut host).await, Some(ServerMsg::Paired));

    send(&mut guest, &ClientMsg::Relay(b"from guest".to_vec())).await;
    send(&mut host, &ClientMsg::Relay(b"from host".to_vec())).await;
    assert_eq!(
        recv(&mut host).await,
        Some(ServerMsg::Relay(b"from guest".to_vec()))
    );
    assert_eq!(
        recv(&mut guest).await,
        Some(ServerMsg::Relay(b"from host".to_vec()))
    );

    guest.close(None).await.unwrap();
    assert_eq!(recv(&mut host).await, Some(ServerMsg::PeerLeft));
}

#[tokio::test]
async fn a_nameplate_can_be_claimed_only_once() {
    let url = start(Config::default()).await;
    let (_host, nameplate) = allocate(&url).await;
    let (_guest, reply) = claim(&url, nameplate).await;
    assert_eq!(reply, Some(ServerMsg::Paired));
    let (_intruder, reply) = claim(&url, nameplate).await;
    assert_eq!(
        reply,
        Some(ServerMsg::Error(RendezvousError::UnknownNameplate))
    );
}

#[tokio::test]
async fn unknown_nameplate_is_reported() {
    let url = start(Config::default()).await;
    let (_ws, reply) = claim(&url, 4242).await;
    assert_eq!(
        reply,
        Some(ServerMsg::Error(RendezvousError::UnknownNameplate))
    );
}

#[tokio::test]
async fn nameplate_is_freed_when_the_sender_leaves() {
    let url = start(Config::default()).await;
    let (mut host, nameplate) = allocate(&url).await;
    host.close(None).await.unwrap();
    drop(host);
    // Give the server a moment to notice.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_ws, reply) = claim(&url, nameplate).await;
    assert_eq!(
        reply,
        Some(ServerMsg::Error(RendezvousError::UnknownNameplate))
    );
}

#[tokio::test]
async fn waiting_senders_get_distinct_nameplates() {
    let url = start(Config::default()).await;
    let mut seen = std::collections::HashSet::new();
    let mut hosts = Vec::new();
    for _ in 0..60 {
        let (ws, n) = allocate(&url).await;
        assert!(seen.insert(n), "nameplate {n} handed out twice");
        hosts.push(ws);
    }
}

#[tokio::test]
async fn incompatible_versions_are_refused() {
    let url = start(Config::default()).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(
        &mut ws,
        &ClientMsg::Hello {
            version: 99,
            min_version: 99,
        },
    )
    .await;
    assert_eq!(
        recv(&mut ws).await,
        Some(ServerMsg::Error(RendezvousError::Version))
    );
    assert_eq!(recv(&mut ws).await, None);
}

#[tokio::test]
async fn messages_before_hello_are_refused() {
    let url = start(Config::default()).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut ws, &ClientMsg::Allocate).await;
    assert_eq!(
        recv(&mut ws).await,
        Some(ServerMsg::Error(RendezvousError::Protocol))
    );
}

#[tokio::test]
async fn garbage_is_refused() {
    let url = start(Config::default()).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::Binary(vec![0xff, 0xff, 0xff].into()))
        .await
        .unwrap();
    assert_eq!(
        recv(&mut ws).await,
        Some(ServerMsg::Error(RendezvousError::Protocol))
    );
}

#[tokio::test]
async fn oversized_payloads_are_refused() {
    let url = start(Config::default()).await;
    let (mut host, nameplate) = allocate(&url).await;
    let (_guest, _) = claim(&url, nameplate).await;
    assert_eq!(recv(&mut host).await, Some(ServerMsg::Paired));
    send(&mut host, &ClientMsg::Relay(vec![0; MAX_RELAY_PAYLOAD + 1])).await;
    assert_eq!(
        recv(&mut host).await,
        Some(ServerMsg::Error(RendezvousError::Protocol))
    );
}

#[tokio::test]
async fn huge_websocket_messages_are_cut_off() {
    let url = start(Config::default()).await;
    let mut ws = connect(&url).await;
    // Larger than MAX_RENDEZVOUS_MESSAGE: the server drops the connection.
    let _ = ws.send(Message::Binary(vec![0; 1024 * 1024].into())).await;
    assert_eq!(recv(&mut ws).await, None);
}

#[tokio::test]
async fn relay_count_is_capped() {
    let url = start(Config::default()).await;
    let (mut host, nameplate) = allocate(&url).await;
    let (mut guest, _) = claim(&url, nameplate).await;
    assert_eq!(recv(&mut host).await, Some(ServerMsg::Paired));
    for _ in 0..=MAX_RELAYED_MESSAGES {
        send(&mut guest, &ClientMsg::Relay(vec![1])).await;
    }
    let mut last = None;
    while let Some(msg) = recv(&mut guest).await {
        last = Some(msg);
    }
    assert_eq!(last, Some(ServerMsg::Error(RendezvousError::Protocol)));
}

#[tokio::test]
async fn unclaimed_nameplates_expire() {
    let url = start(Config {
        nameplate_ttl: Duration::from_millis(200),
        ..Config::default()
    })
    .await;
    let (mut host, _) = allocate(&url).await;
    assert_eq!(
        recv(&mut host).await,
        Some(ServerMsg::Error(RendezvousError::Expired))
    );
}

#[tokio::test]
async fn full_server_says_busy() {
    let url = start(Config {
        max_waiting: 1,
        ..Config::default()
    })
    .await;
    let (_host, _) = allocate(&url).await;
    let mut ws = connect(&url).await;
    send(&mut ws, &ClientMsg::Allocate).await;
    assert_eq!(
        recv(&mut ws).await,
        Some(ServerMsg::Error(RendezvousError::Busy))
    );
}
