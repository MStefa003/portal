//! Real transfers between two iroh endpoints over 127.0.0.1, plus a
//! hand-written malicious sender to check that the receiver fails safely.
#![allow(clippy::unwrap_used)] // test helpers, not just #[test] fns

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portal_core::{OverwritePolicy, ScanOptions, SinkError, scan};
use portal_net::{Event, NetError, NetMode, ReceiveOptions, Reporter, Ticket};
use portal_proto::frame::{read_msg, write_msg};
use portal_proto::{
    Control, DeclineReason, EncodedChunk, Encoding, MAX_CONTROL_FRAME, Offer, VersionError,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Runs a real sender and receiver; returns both results and receiver events.
async fn run_transfer(
    src: &Path,
    dest: &Path,
    overwrite: OverwritePolicy,
) -> (
    Result<(), NetError>,
    Result<portal_core::Summary, NetError>,
    Vec<Event>,
) {
    let scan = scan(src, &ScanOptions::default()).unwrap();
    let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
    let ticket = portal_net::local_ticket(&sender, NetMode::Loopback).await;
    // Go through the text form, exactly as a user would copy it.
    let ticket: Ticket = ticket.to_string().parse().unwrap();

    let root = src.to_path_buf();
    let send = tokio::spawn(async move {
        let result = portal_net::send(&sender, &root, &scan.manifest, quiet()).await;
        sender.close().await;
        result
    });

    let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
    let (reporter, events) = recording();
    let options = ReceiveOptions {
        dest: dest.to_path_buf(),
        overwrite,
    };
    let received = portal_net::receive(&receiver, &ticket, options, reporter).await;
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
async fn evil_sender(src: &Path, evil: Evil) -> (Ticket, tokio::task::JoinHandle<()>) {
    let manifest = scan(src, &ScanOptions::default()).unwrap().manifest;
    let endpoint = portal_net::bind(NetMode::Loopback, true).await.unwrap();
    let ticket = portal_net::local_ticket(&endpoint, NetMode::Loopback).await;
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
    (ticket, task)
}

async fn receive_from(ticket: &Ticket, dest: &Path) -> Result<portal_core::Summary, NetError> {
    let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
    let options = ReceiveOptions {
        dest: dest.to_path_buf(),
        overwrite: OverwritePolicy::Never,
    };
    let result = portal_net::receive(&receiver, ticket, options, quiet()).await;
    receiver.close().await;
    result
}

#[tokio::test]
async fn corrupted_chunk_fails_safely() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("secret.bin");
        fs::write(&src, vec![7u8; 200_000]).unwrap();
        let dest = tmp.path().join("out");

        let (ticket, sender) = evil_sender(&src, Evil::CorruptChunk).await;
        let result = receive_from(&ticket, &dest).await;
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

        let (ticket, sender) = evil_sender(&src, Evil::FutureVersion).await;
        let result = receive_from(&ticket, &tmp.path().join("out")).await;
        let Err(NetError::Version(err @ VersionError::PeerTooNew { .. })) = result else {
            panic!("expected a version error, got {result:?}");
        };
        assert!(err.to_string().contains("update Portal"));
        sender.await.unwrap();
    })
    .await
    .unwrap();
}
