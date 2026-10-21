//! Serverless pairing: senders found through a private Mainline DHT on
//! 127.0.0.1, the code handshake on the connection itself, and attackers
//! that try to get between the peers.
#![allow(clippy::unwrap_used)] // test helpers, not just #[test] fns

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use portal_core::{OverwritePolicy, ScanOptions, scan};
use portal_net::{DhtNetwork, Discovery, NetError, NetMode, ReceiveOptions, Reporter};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{Code, PAIR_ALPN, PakeError, PeerMsg};

const TEST_TIMEOUT: Duration = Duration::from_secs(90);
const FIND_TIMEOUT: Duration = Duration::from_secs(30);

fn quiet() -> Reporter {
    Arc::new(|_| {})
}

/// A private DHT; keep it alive for the whole test.
fn testnet() -> (mainline::Testnet, Discovery) {
    let net = mainline::Testnet::builder(10).build().unwrap();
    let discovery = Discovery {
        mdns: false,
        dht: Some(DhtNetwork::Local(net.bootstrap.clone())),
    };
    (net, discovery)
}

fn options(dest: &Path) -> ReceiveOptions {
    ReceiveOptions {
        dest: dest.to_path_buf(),
        overwrite: OverwritePolicy::Never,
    }
}

/// The same code with its last word changed: what a guesser would try.
fn wrong_guess(code: &Code) -> Code {
    let real = code.to_string();
    let words: Vec<&str> = real.split('-').collect();
    let other = if words[2] == "acid" { "acorn" } else { "acid" };
    Code::parse(&format!("{}-{}-{other}", words[0], words[1])).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn transfers_through_the_dht() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (_net, discovery) = testnet();
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("project");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "found without a server").unwrap();
        fs::write(src.join("sub/b.bin"), vec![9u8; 1_500_000]).unwrap();
        let manifest = scan(&src, &ScanOptions::default()).unwrap().manifest;

        let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let code = Code::random().unwrap();
        let announcement =
            portal_net::discovery::announce(&sender, NetMode::Loopback, &code, &discovery);
        let send_code = code.clone();
        let root = src.clone();
        let send = tokio::spawn(async move {
            let conn = portal_net::direct::accept(&sender, &send_code).await?;
            let result = portal_net::send_on(&conn, &root, &manifest, quiet()).await;
            sender.close().await;
            drop(announcement);
            result
        });

        // Typed by a person, from the text form.
        let typed = Code::parse(&code.to_string()).unwrap();
        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let addr = portal_net::discovery::find(&typed, &discovery, FIND_TIMEOUT)
            .await
            .unwrap();
        let conn = portal_net::direct::connect(&receiver, addr, &typed)
            .await
            .unwrap();
        let dest = tmp.path().join("out");
        let summary = portal_net::receive_on(&conn, options(&dest), quiet())
            .await
            .unwrap();
        receiver.close().await;
        send.await.unwrap().unwrap();

        assert_eq!(summary.files, 2);
        assert_eq!(
            fs::read_to_string(dest.join("project/a.txt")).unwrap(),
            "found without a server"
        );
        assert_eq!(
            fs::read(dest.join("project/sub/b.bin")).unwrap(),
            vec![9u8; 1_500_000]
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_number_is_not_found() {
    let (_net, discovery) = testnet();
    let code = Code::random().unwrap();
    let found = portal_net::discovery::find(&code, &discovery, Duration::from_secs(5)).await;
    assert!(matches!(found, Err(NetError::SenderNotFound)), "{found:?}");
}

#[tokio::test]
async fn wrong_code_fails_on_both_sides_and_uses_up_the_code() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let addr = portal_net::local_addr(&sender, NetMode::Loopback).await;
        let code = Code::random().unwrap();
        let send_code = code.clone();
        // Hands the endpoint back: dropping it would cut the connection.
        let accept = tokio::spawn(async move {
            let result = portal_net::direct::accept(&sender, &send_code).await;
            (sender, result)
        });

        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let guessed =
            portal_net::direct::connect(&receiver, addr.clone(), &wrong_guess(&code)).await;
        assert!(
            matches!(guessed, Err(NetError::Pake(PakeError::WrongCode))),
            "{guessed:?}"
        );
        let (_sender, accepted) = accept.await.unwrap();
        assert!(
            matches!(accepted, Err(NetError::Pake(PakeError::WrongCode))),
            "{accepted:?}"
        );
    })
    .await
    .unwrap();
}

/// Connections that never try the code (port scanners, flaky networks)
/// and plain-ALPN connections do not use it up.
#[tokio::test]
async fn strangers_that_do_not_guess_leave_the_code_intact() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let addr = portal_net::local_addr(&sender, NetMode::Loopback).await;
        let code = Code::random().unwrap();
        let send_code = code.clone();
        // Hands the endpoint back: dropping it would cut the connection.
        let accept = tokio::spawn(async move {
            let result = portal_net::direct::accept(&sender, &send_code).await;
            (sender, result)
        });

        let stranger = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        // Connects for pairing, then hangs up without a handshake message.
        let conn = stranger.connect(addr.clone(), PAIR_ALPN).await.unwrap();
        conn.close(0u32.into(), b"bye");
        // Skips pairing altogether.
        if let Ok(conn) = stranger.connect(addr.clone(), portal_proto::ALPN).await {
            let _ = conn.closed().await;
        }

        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        // Kept open, as for a transfer: dropping it would hang up.
        let _paired = portal_net::direct::connect(&receiver, addr, &code)
            .await
            .unwrap();
        let (_sender, accepted) = accept.await.unwrap();
        let conn = accepted.unwrap();
        assert_eq!(conn.remote_id(), receiver.id());
    })
    .await
    .unwrap();
}

/// A machine that got between the peers (say, by overwriting the DHT
/// record) and relays the handshake untouched learns nothing, and both
/// sides notice that the peer that knows the code is not the one they are
/// connected to.
#[tokio::test]
async fn relaying_the_handshake_is_detected() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let sender_addr = portal_net::local_addr(&sender, NetMode::Loopback).await;
        let code = Code::random().unwrap();
        let send_code = code.clone();
        // Hands the endpoint back: dropping it would cut the connection.
        let accept = tokio::spawn(async move {
            let result = portal_net::direct::accept(&sender, &send_code).await;
            (sender, result)
        });

        // The attacker listens like a sender and dials the real one.
        let mitm = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let mitm_addr = portal_net::local_addr(&mitm, NetMode::Loopback).await;
        let relay = tokio::spawn(async move {
            let from_receiver = mitm.accept().await.unwrap().await.unwrap();
            let to_sender = mitm.connect(sender_addr, PAIR_ALPN).await.unwrap();
            let (mut r_tx, mut r_rx) = from_receiver.accept_bi().await.unwrap();
            let (mut s_tx, mut s_rx) = to_sender.open_bi().await.unwrap();
            // Two messages each way: the SPAKE2 message and the sealed ID.
            for _ in 0..2 {
                let a: Option<PeerMsg> = read_msg(&mut r_rx, 4096).await.unwrap();
                write_msg(&mut s_tx, &a.unwrap()).await.unwrap();
                let b: Option<PeerMsg> = read_msg(&mut s_rx, 4096).await.unwrap();
                write_msg(&mut r_tx, &b.unwrap()).await.unwrap();
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), from_receiver.closed()).await;
            mitm.close().await;
        });

        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let got = portal_net::direct::connect(&receiver, mitm_addr, &code).await;
        assert!(matches!(got, Err(NetError::WrongPeer)), "{got:?}");
        let (_sender, accepted) = accept.await.unwrap();
        assert!(matches!(accepted, Err(NetError::WrongPeer)), "{accepted:?}");
        relay.await.unwrap();
    })
    .await
    .unwrap();
}
