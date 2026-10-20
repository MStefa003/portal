//! Real transfers between two iroh endpoints over 127.0.0.1, paired through a
//! real rendezvous server, plus hand-written malicious peers to check that
//! both sides fail safely.
#![allow(clippy::unwrap_used)] // test helpers, not just #[test] fns

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portal_core::{OverwritePolicy, ScanOptions, SinkError, scan};
use portal_net::{EndpointAddr, Event, NetError, NetMode, ReceiveOptions, Reporter};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{
    Code, Control, DeclineReason, EncodedChunk, Encoding, MAX_CONTROL_FRAME, Offer, PakeError,
    RendezvousError, VersionError,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Starts an in-process rendezvous server; returns its URL.
async fn rendezvous() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(portal_rendezvous::serve(
        listener,
        portal_rendezvous::Config::default(),
    ));
    url
}

fn quiet() -> Reporter {
    Arc::new(|_| {})
}

fn recording() -> (Reporter, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    (Arc::new(move |e| sink.lock().unwrap().push(e)), events)
}

fn make_project(root: &Path) {
    fs::create_dir_all(root.join("src/empty")).unwrap();
    fs::write(root.join("README.md"), "hello\r\n".repeat(1000)).unwrap();
    fs::write(root.join("src/zero.txt"), "").unwrap();
    // Two identical ~3 MB files: the second must not cross the network.
    let mut data = Vec::new();
    let mut h = portal_core::Hash::of(b"seed");
    while data.len() < 3_000_000 {
        h = portal_core::Hash::of(h.as_bytes());
        data.extend_from_slice(h.as_bytes());
    }
    fs::write(root.join("src/random.bin"), &data).unwrap();
    fs::write(root.join("src/random-copy.bin"), &data).unwrap();
}

fn same_tree(a: &Path, b: &Path) {
    let mut left: Vec<_> = walk(a);
    let mut right: Vec<_> = walk(b);
    left.sort();
    right.sort();
    assert_eq!(left, right);
}

fn walk(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                out.push((rel, None));
                stack.push(path);
            } else {
                out.push((rel, Some(fs::read(&path).unwrap())));
            }
        }
    }
    out
}

/// Runs a real sender and receiver, paired by code; returns both results and
/// the receiver's events.
async fn run_transfer(
    src: &Path,
    dest: &Path,
    overwrite: OverwritePolicy,
) -> (
    Result<(), NetError>,
    Result<portal_core::Summary, NetError>,
    Vec<Event>,
) {
    let url = rendezvous().await;
    let scan = scan(src, &ScanOptions::default()).unwrap();
    let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
    let waiting = portal_net::rendezvous::allocate(&url).await.unwrap();
    // Go through the text form, exactly as a person would type it.
    let code = Code::parse(&waiting.code().to_string()).unwrap();

    let root = src.to_path_buf();
    let send = tokio::spawn(async move {
        let peer = waiting.pair(&sender, NetMode::Loopback).await?;
        let result = portal_net::send(&sender, peer, &root, &scan.manifest, quiet()).await;
        sender.close().await;
        result
    });

    let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
    let (reporter, events) = recording();
    let options = ReceiveOptions {
        dest: dest.to_path_buf(),
        overwrite,
    };
    let addr = portal_net::rendezvous::join(&url, &code, &receiver)
        .await
        .unwrap();
    let received = portal_net::receive(&receiver, addr, options, reporter).await;
    receiver.close().await;
    let sent = send.await.unwrap();
    let events = events.lock().unwrap().clone();
    (sent, received, events)
}

#[tokio::test]
async fn transfers_a_folder() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a/project");
        let dest = tmp.path().join("b");
        make_project(&src);

        let (sent, received, events) = run_transfer(&src, &dest, OverwritePolicy::Never).await;
        sent.unwrap();
        let summary = received.unwrap();
        assert_eq!(summary.files, 4);
        same_tree(&src, &dest.join("project"));

        // The duplicate file was skipped, and progress adds up to what was sent.
        let (total, skipped) = events
            .iter()
            .find_map(|e| match e {
                Event::Started {
                    total_bytes,
                    skipped_bytes,
                } => Some((*total_bytes, *skipped_bytes)),
                _ => None,
            })
            .unwrap();
        assert!(
            skipped >= 3_000_000,
            "copy should be deduplicated: {skipped}"
        );
        let progressed: u64 = events
            .iter()
            .filter_map(|e| match e {
                Event::Progress { bytes } => Some(*bytes),
                _ => None,
            })
            .sum();
        assert_eq!(progressed, total);
        assert!(matches!(events.first(), Some(Event::Connected { .. })));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn transfers_a_single_file() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("notes.txt");
        fs::write(&src, "just one file").unwrap();
        let dest = tmp.path().join("out");
        let (sent, received, _) = run_transfer(&src, &dest, OverwritePolicy::Never).await;
        sent.unwrap();
        received.unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("notes.txt")).unwrap(),
            "just one file"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn existing_files_make_the_receiver_decline() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("project");
        make_project(&src);
        let dest = tmp.path().join("out");
        fs::create_dir_all(dest.join("project")).unwrap();
        fs::write(dest.join("project/README.md"), "mine").unwrap();

        let (sent, received, _) = run_transfer(&src, &dest, OverwritePolicy::Never).await;
        assert!(matches!(
            received,
            Err(NetError::Sink(SinkError::Conflicts(_)))
        ));
        assert!(matches!(
            sent,
            Err(NetError::Declined(DeclineReason::Conflicts))
        ));
        assert_eq!(
            fs::read_to_string(dest.join("project/README.md")).unwrap(),
            "mine"
        );

        let (sent, received, _) = run_transfer(&src, &dest, OverwritePolicy::Always).await;
        sent.unwrap();
        received.unwrap();
        same_tree(&src, &dest.join("project"));
    })
    .await
    .unwrap();
}

/// What the fake sender does after the `Hello` exchange.
#[derive(Clone, Copy)]
enum Evil {
    /// Claims an incompatible protocol version.
    FutureVersion,
    /// Sends a chunk whose bytes do not match its hash.
    CorruptChunk,
}

/// A sender written directly against the protocol, to misbehave on purpose.
async fn evil_sender(src: &Path, evil: Evil) -> (EndpointAddr, tokio::task::JoinHandle<()>) {
    let manifest = scan(src, &ScanOptions::default()).unwrap().manifest;
    let endpoint = portal_net::bind(NetMode::Loopback, true).await.unwrap();
    let addr = portal_net::local_addr(&endpoint, NetMode::Loopback).await;
    let task = tokio::spawn(async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        let (mut tx, mut rx) = conn.accept_bi().await.unwrap();
        let _hello: Option<Control> = read_msg(&mut rx, MAX_CONTROL_FRAME).await.unwrap();
        let (version, min_version) = match evil {
            Evil::FutureVersion => (99, 99),
            Evil::CorruptChunk => (1, 1),
        };
        write_msg(
            &mut tx,
            &Control::Hello {
                version,
                min_version,
            },
        )
        .await
        .unwrap();
        if let Evil::CorruptChunk = evil {
            let bytes = manifest.to_bytes().unwrap();
            let offer = Offer {
                name: manifest.name().into(),
                files: manifest.file_count(),
                bytes: manifest.total_bytes(),
                manifest_len: bytes.len() as u64,
                transfer_id: portal_core::Hash::of(&bytes),
            };
            write_msg(&mut tx, &Control::Offer(offer)).await.unwrap();
            write_msg(&mut tx, &Control::ManifestPart(bytes))
                .await
                .unwrap();
            let _accept: Option<Control> = read_msg(&mut rx, MAX_CONTROL_FRAME).await.unwrap();
            let Some(Control::Want(wanted)) = read_msg(&mut rx, MAX_CONTROL_FRAME).await.unwrap()
            else {
                panic!("expected Want");
            };
            // Right length, wrong content.
            let len = manifest.files().next().unwrap().2.chunks[0].len as usize;
            let chunk = EncodedChunk {
                hash: wanted[0],
                encoding: Encoding::Raw,
                raw_len: len as u32,
                payload: vec![0x42; len],
            };
            let mut data = conn.open_uni().await.unwrap();
            chunk.write(&mut data).await.unwrap();
            let _ = data.finish();
        }
        // Wait until the receiver hangs up.
        conn.closed().await;
        endpoint.close().await;
    });
    (addr, task)
}

async fn receive_from(addr: EndpointAddr, dest: &Path) -> Result<portal_core::Summary, NetError> {
    let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
    let result = receive_with(&receiver, addr, dest).await;
    receiver.close().await;
    result
}

async fn receive_with(
    endpoint: &portal_net::Endpoint,
    addr: EndpointAddr,
    dest: &Path,
) -> Result<portal_core::Summary, NetError> {
    let options = ReceiveOptions {
        dest: dest.to_path_buf(),
        overwrite: OverwritePolicy::Never,
    };
    portal_net::receive(endpoint, addr, options, quiet()).await
}

#[tokio::test]
async fn corrupted_chunk_fails_safely() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("secret.bin");
        fs::write(&src, vec![7u8; 200_000]).unwrap();
        let dest = tmp.path().join("out");

        let (addr, sender) = evil_sender(&src, Evil::CorruptChunk).await;
        let result = receive_from(addr, &dest).await;
        assert!(
            matches!(result, Err(NetError::Sink(SinkError::CorruptChunk(_)))),
            "{result:?}"
        );
        assert!(
            !dest.join("secret.bin").exists(),
            "no file at the final path"
        );
        sender.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn incompatible_version_fails_clearly() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("f.txt");
        fs::write(&src, "x").unwrap();

        let (addr, sender) = evil_sender(&src, Evil::FutureVersion).await;
        let result = receive_from(addr, &tmp.path().join("out")).await;
        let Err(NetError::Version(err @ VersionError::PeerTooNew { .. })) = result else {
            panic!("expected a version error, got {result:?}");
        };
        assert!(err.to_string().contains("update Portal"));
        sender.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn wrong_code_fails_on_both_sides() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let url = rendezvous().await;
        let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let waiting = portal_net::rendezvous::allocate(&url).await.unwrap();
        let real = waiting.code().to_string();
        let words: Vec<&str> = real.split('-').collect();
        // Same number, one wrong word: what a guessing attacker would try.
        let other = if words[2] == "acid" { "acorn" } else { "acid" };
        let guess = Code::parse(&format!("{}-{}-{other}", words[0], words[1])).unwrap();

        let pair = tokio::spawn(async move { waiting.pair(&sender, NetMode::Loopback).await });
        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let joined = portal_net::rendezvous::join(&url, &guess, &receiver).await;
        assert!(
            matches!(joined, Err(NetError::Pake(PakeError::WrongCode))),
            "{joined:?}"
        );
        let paired = pair.await.unwrap();
        assert!(
            matches!(paired, Err(NetError::Pake(PakeError::WrongCode))),
            "{paired:?}"
        );

        // The code is used up: even the right words no longer work.
        let real = Code::parse(&real).unwrap();
        let retry = portal_net::rendezvous::join(&url, &real, &receiver).await;
        assert!(
            matches!(
                retry,
                Err(NetError::Rendezvous(RendezvousError::UnknownNameplate))
            ),
            "{retry:?}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unknown_code_number_is_reported() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let url = rendezvous().await;
        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let code = Code::parse("4242-cedar-river").unwrap();
        let joined = portal_net::rendezvous::join(&url, &code, &receiver).await;
        assert!(
            matches!(
                joined,
                Err(NetError::Rendezvous(RendezvousError::UnknownNameplate))
            ),
            "{joined:?}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unreachable_rendezvous_is_reported() {
    let result = portal_net::rendezvous::allocate("ws://127.0.0.1:1/v1").await;
    assert!(
        matches!(result, Err(NetError::RendezvousConnect(_))),
        "{result:?}"
    );
    let result = portal_net::rendezvous::allocate("https://example.com").await;
    assert!(matches!(result, Err(NetError::RendezvousUrl)), "{result:?}");
}

/// A node that learned the sender's address some other way, but never
/// proved it knows the code, is refused; the real receiver still gets in.
#[tokio::test]
async fn sender_only_accepts_the_paired_receiver() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("f.txt");
        fs::write(&src, "for the right receiver only").unwrap();
        let manifest = scan(&src, &ScanOptions::default()).unwrap().manifest;

        let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
        let sender_addr = portal_net::local_addr(&sender, NetMode::Loopback).await;
        let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let peer = receiver.id();
        let send =
            tokio::spawn(
                async move { portal_net::send(&sender, peer, &src, &manifest, quiet()).await },
            );

        let stranger = portal_net::bind(NetMode::Loopback, false).await.unwrap();
        let stolen = receive_with(&stranger, sender_addr.clone(), &tmp.path().join("x")).await;
        assert!(stolen.is_err(), "{stolen:?}");
        assert!(!tmp.path().join("x/f.txt").exists());

        receive_with(&receiver, sender_addr, &tmp.path().join("y"))
            .await
            .unwrap();
        send.await.unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("y/f.txt")).unwrap(),
            "for the right receiver only"
        );
    })
    .await
    .unwrap();
}
