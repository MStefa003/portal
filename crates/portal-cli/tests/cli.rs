//! End-to-end checks of the compiled `portal` binary.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers, not just #[test] fns

use std::process::Command;

fn portal() -> Command {
    Command::new(env!("CARGO_BIN_EXE_portal"))
}

#[test]
fn help_lists_subcommands() {
    let out = portal().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("send"));
    assert!(stdout.contains("receive"));
}

#[test]
fn version_matches_crate() {
    let out = portal().arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn receive_requires_a_code() {
    let out = portal().arg("receive").output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no code given"), "{stderr}");
}

/// Starts a rendezvous server on a background runtime; returns its URL.
fn rendezvous() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/v1", listener.local_addr().unwrap());
            tx.send(url).unwrap();
            portal_rendezvous::serve(listener, portal_rendezvous::Config::default())
                .await
                .unwrap();
        });
    });
    rx.recv().unwrap()
}

/// Two real `portal` processes transferring a folder over 127.0.0.1, paired
/// by code through a real rendezvous server.
#[test]
fn send_and_receive_between_two_processes() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let url = rendezvous();
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("project");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("hello.txt"), "hello from portal").unwrap();
    std::fs::write(src.join("sub/data.bin"), vec![42u8; 2_000_000]).unwrap();
    let dest = tmp.path().join("received");

    let mut sender = portal()
        .args(["--loopback", "send", "--rendezvous", &url])
        .arg(&src)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Keep draining the sender's stdout after finding the code: closing the
    // pipe would make its next `println!` fail.
    let stdout = sender.stdout.take().unwrap();
    let (code_tx, code_rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(code) = line.strip_prefix("Code: ") {
                let _ = code_tx.send(console::strip_ansi_codes(code).trim().to_owned());
            }
        }
    });
    let code = code_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("sender printed a code");

    let out = portal()
        .args([
            "--loopback",
            "receive",
            "--rendezvous",
            &url,
            &code,
            "--output",
        ])
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "receiver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sender.wait().unwrap().success(), "sender failed");
    drain.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("project/hello.txt")).unwrap(),
        "hello from portal"
    );
    assert_eq!(
        std::fs::read(dest.join("project/sub/data.bin")).unwrap(),
        vec![42u8; 2_000_000]
    );
}

/// Typos are caught before contacting anyone, so they do not use up the code.
#[test]
fn receive_catches_typos_offline() {
    let out = portal()
        .args([
            "--loopback",
            "receive",
            "--rendezvous",
            "ws://127.0.0.1:1/v1",
            "7-cedar-rivr",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean \"river\""), "{stderr}");
}

#[test]
fn default_rendezvous_explains_self_hosting() {
    let out = portal()
        .args(["receive", "7-cedar-river"])
        .env_remove("PORTAL_RENDEZVOUS")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SELF_HOSTING"), "{stderr}");
}
