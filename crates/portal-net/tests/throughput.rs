//! Throughput benchmark: scans and sends a large folder over 127.0.0.1 and
//! prints how long each part took. Ignored by default; run it with
//!
//! ```console
//! cargo test --release -p portal-net --test throughput -- --ignored --nocapture
//! ```
//!
//! `PORTAL_BENCH_MB` sets the size (default 1024), half random and half
//! compressible text.
#![allow(clippy::unwrap_used)] // test helpers, not just #[test] fns

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use portal_core::{OverwritePolicy, ScanOptions, scan};
use portal_net::{NetMode, Payload, ReceiveOptions, Received, SendOptions};
use portal_proto::Code;

const MB: usize = 1024 * 1024;

/// Fast deterministic pseudo-random bytes (xorshift64*).
fn random_file(path: &Path, len: usize, seed: u64) {
    let mut out = fs::File::create(path).unwrap();
    let mut state = seed | 1;
    let mut buf = Vec::with_capacity(MB);
    for _ in 0..len / MB {
        buf.clear();
        while buf.len() < MB {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            buf.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
        }
        out.write_all(&buf).unwrap();
    }
}

fn text_file(path: &Path, len: usize) {
    let mut out = fs::File::create(path).unwrap();
    let line = b"2026-10-23 12:00:00 INFO portal: chunk received, verified and written\n";
    let mut written = 0;
    let mut n = 0u64;
    while written < len {
        let s = format!("{n:>10} ");
        out.write_all(s.as_bytes()).unwrap();
        out.write_all(line).unwrap();
        written += s.len() + line.len();
        n += 1;
    }
}

fn rate(bytes: usize, secs: f64) -> String {
    format!("{secs:.2}s ({:.0} MB/s)", bytes as f64 / MB as f64 / secs)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
async fn throughput() {
    let total = std::env::var("PORTAL_BENCH_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024usize)
        * MB;
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("bench");
    fs::create_dir_all(&src).unwrap();
    // A few large files and many small ones.
    for i in 0..4 {
        random_file(&src.join(format!("random-{i}.bin")), total / 8, i + 1);
    }
    text_file(&src.join("log.txt"), total / 4);
    let small = src.join("small");
    fs::create_dir_all(&small).unwrap();
    for i in 0..(total / 4 / (256 * 1024)) {
        random_file(&small.join(format!("{i}.bin")), MB / 4, i as u64 + 100);
    }

    let start = Instant::now();
    let payload = Payload::from(scan(&src, &ScanOptions::default()).unwrap());
    let scanned = start.elapsed().as_secs_f64();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(portal_rendezvous::serve(
        listener,
        portal_rendezvous::Config::default(),
    ));
    let sender = portal_net::bind(NetMode::Loopback, true).await.unwrap();
    let waiting = portal_net::rendezvous::allocate(&url).await.unwrap();
    let code = Code::parse(&waiting.code().to_string()).unwrap();
    let send = tokio::spawn(async move {
        let peer = waiting.pair(&sender, NetMode::Loopback).await?;
        let result = portal_net::send(
            &sender,
            peer,
            &payload,
            &SendOptions::default(),
            Arc::new(|_| {}),
        )
        .await;
        sender.close().await;
        result
    });

    let receiver = portal_net::bind(NetMode::Loopback, false).await.unwrap();
    let addr = portal_net::rendezvous::join(&url, &code, &receiver)
        .await
        .unwrap();
    let options = ReceiveOptions {
        dest: tmp.path().join("out"),
        overwrite: OverwritePolicy::Never,
        confirm: None,
        rate_limit: None,
    };
    let start = Instant::now();
    let received = portal_net::receive(&receiver, addr, options, Arc::new(|_| {}))
        .await
        .unwrap();
    let transferred = start.elapsed().as_secs_f64();
    receiver.close().await;
    send.await.unwrap().unwrap();
    assert!(matches!(received, Received::Files(_)));

    println!("scan:     {}", rate(total, scanned));
    println!("transfer: {}", rate(total, transferred));
}

/// The ceiling: raw bytes over 4 QUIC streams, no disk, no hashing.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
async fn raw_quic() {
    use portal_proto::ALPN;
    let total = 1024 * MB;
    let a = portal_net::bind(NetMode::Loopback, true).await.unwrap();
    let b = portal_net::bind(NetMode::Loopback, false).await.unwrap();
    let addr = portal_net::local_addr(&a, NetMode::Loopback).await;
    let server = tokio::spawn(async move {
        let conn = a.accept().await.unwrap().await.unwrap();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let mut s = conn.open_uni().await.unwrap();
            tasks.push(tokio::spawn(async move {
                let buf = vec![7u8; MB];
                for _ in 0..total / MB / 4 {
                    s.write_all(&buf).await.unwrap();
                }
                s.finish().unwrap();
                let _ = s.stopped().await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        conn.closed().await;
    });
    let conn = b.connect(addr, ALPN).await.unwrap();
    let start = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let mut s = conn.accept_uni().await.unwrap();
        tasks.push(tokio::spawn(async move {
            let mut n = 0;
            let mut buf = vec![0u8; MB];
            while let Some(k) = s.read(&mut buf).await.unwrap() {
                n += k;
            }
            n
        }));
    }
    let mut got = 0;
    for t in tasks {
        got += t.await.unwrap();
    }
    println!("raw quic: {}", rate(got, start.elapsed().as_secs_f64()));
    conn.close(0u32.into(), b"");
    let _ = server.await;
}

/// Scanning one large file, which cannot be split across files like a folder.
#[test]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
fn scan_one_large_file() {
    let total = std::env::var("PORTAL_BENCH_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024usize)
        * MB;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("video.bin");
    random_file(&file, total, 7);
    let start = Instant::now();
    scan(&file, &ScanOptions::default()).unwrap();
    println!(
        "scan one file: {}",
        rate(total, start.elapsed().as_secs_f64())
    );
}
