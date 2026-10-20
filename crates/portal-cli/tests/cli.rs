//! End-to-end checks of the compiled `portal` binary.

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
}

/// Two real `portal` processes transferring a folder over 127.0.0.1.
#[test]
fn send_and_receive_between_two_processes() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("project");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("hello.txt"), "hello from portal").unwrap();
    std::fs::write(src.join("sub/data.bin"), vec![42u8; 2_000_000]).unwrap();
    let dest = tmp.path().join("received");

    let mut sender = portal()
        .args(["--loopback", "send"])
        .arg(&src)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Keep draining the sender's stdout after finding the address: closing the
    // pipe would make its next `println!` fail.
    let stdout = sender.stdout.take().unwrap();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(addr) = line.split("--addr ").nth(1) {
                let _ = addr_tx.send(addr.trim().to_owned());
            }
        }
    });
    let addr = addr_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("sender printed an address");

    let out = portal()
        .args(["--loopback", "receive", "--addr", &addr, "--output"])
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

#[test]
fn receive_rejects_a_garbled_address() {
    let out = portal()
        .args(["--loopback", "receive", "--addr", "not-an-address"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("address is invalid"), "{stderr}");
}
