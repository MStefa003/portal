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

#[test]
fn prints_shell_completions() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let out = portal().args(["completions", shell]).output().unwrap();
        assert!(out.status.success(), "{shell}");
        let script = String::from_utf8(out.stdout).unwrap();
        assert!(script.contains("receive"), "{shell}: {script}");
    }
}

/// `portal dir` saves a folder, shows it, and forgets it again.
#[test]
fn dir_sets_shows_and_resets_the_download_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let downloads = tmp.path().join("downloads");
    let run = |args: &[&std::ffi::OsStr]| {
        let out = portal()
            .arg("dir")
            .args(args)
            .env("PORTAL_CONFIG_DIR", &config)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    assert!(run(&[]).contains("Not set"));
    run(&[downloads.as_os_str()]);
    assert!(downloads.is_dir(), "the folder is created when set");
    assert_eq!(run(&[]).trim(), downloads.display().to_string());
    run(&["--reset".as_ref()]);
    assert!(run(&[]).contains("Not set"));
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

/// Runs `portal send` and `portal receive` with `flags` on both sides and
/// checks that the folder arrived.
fn transfer_between_two_processes(flags: &[&str]) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("project");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("hello.txt"), "hello from portal").unwrap();
    std::fs::write(src.join("sub/data.bin"), vec![42u8; 2_000_000]).unwrap();
    let dest = tmp.path().join("received");

    let mut sender = portal()
        .args(flags)
        .arg("send")
        .arg(&src)
        .env_remove("PORTAL_RENDEZVOUS")
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
        .args(flags)
        .args(["receive", &code, "--output"])
        .arg(&dest)
        .env_remove("PORTAL_RENDEZVOUS")
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

/// Paired through a real rendezvous server.
#[test]
fn send_and_receive_through_a_rendezvous_server() {
    let url = rendezvous();
    transfer_between_two_processes(&["--loopback", "--rendezvous", &url]);
}

/// No server: the receiver finds the sender in a (private) DHT.
#[test]
fn send_and_receive_through_the_dht() {
    let net = mainline::Testnet::builder(10).build().unwrap();
    let mut flags = vec!["--loopback".to_owned()];
    for node in &net.bootstrap {
        flags.push("--dht-bootstrap".to_owned());
        flags.push(node.clone());
    }
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    transfer_between_two_processes(&flags);
}

/// Typos are caught before contacting anyone, so they do not use up the code.
#[test]
fn receive_catches_typos_offline() {
    let out = portal()
        .args(["--loopback", "receive", "7-cedar-rivr"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean \"river\""), "{stderr}");
}

/// Starts `portal` with `args` and returns it once it printed a line
/// starting with `prefix`, with the rest of that line. Its output keeps
/// being drained in the background.
fn start_and_read(
    args: &[&std::ffi::OsStr],
    config: &std::path::Path,
    prefix: &'static str,
) -> (std::process::Child, String, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = portal()
        .args(args)
        .env_remove("PORTAL_RENDEZVOUS")
        .env("PORTAL_CONFIG_DIR", config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = console::strip_ansi_codes(&line.unwrap()).into_owned();
            if let Some(rest) = line.strip_prefix(prefix) {
                let _ = tx.send(rest.trim().to_owned());
            }
        }
    });
    let found = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("portal printed {prefix:?}"));
    (child, found, drain)
}

fn os(args: &[&str]) -> Vec<std::ffi::OsString> {
    args.iter().map(Into::into).collect()
}

/// Several paths go in one transfer, and text arrives as text.
#[test]
fn sends_several_items_and_text() {
    let url = rendezvous();
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let folder = tmp.path().join("photos");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.jpg"), "jpeg").unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "notes").unwrap();
    let dest = tmp.path().join("received");

    let mut args = os(&["--loopback", "--rendezvous", &url, "send"]);
    args.push(folder.into());
    args.push(file.into());
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let (mut sender, code, drain) = start_and_read(&args, &config, "Code: ");
    let out = portal()
        .args([
            "--loopback",
            "--rendezvous",
            &url,
            "receive",
            &code,
            "--output",
        ])
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sender.wait().unwrap().success());
    drain.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("photos/a.jpg")).unwrap(),
        "jpeg"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("notes.txt")).unwrap(),
        "notes"
    );

    let text = "https://example.com\nsecond line ünïcode";
    let args = os(&["--loopback", "--rendezvous", &url, "send", "--text", text]);
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let (mut sender, code, drain) = start_and_read(&args, &config, "Code: ");
    let out = portal()
        .args(["--loopback", "--rendezvous", &url, "receive", &code])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sender.wait().unwrap().success());
    drain.join().unwrap();
    // Not a terminal: the exact text, nothing else on stdout.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.ends_with(text), "{stdout:?}");
}

/// Two devices pair with a code and list each other; unpairing forgets.
#[test]
fn devices_pair_and_unpair() {
    let net = mainline::Testnet::builder(10).build().unwrap();
    let mut flags = vec!["--loopback".to_owned()];
    for node in &net.bootstrap {
        flags.push("--dht-bootstrap".to_owned());
        flags.push(node.clone());
    }
    let tmp = tempfile::tempdir().unwrap();
    let desk = tmp.path().join("desk");
    let laptop = tmp.path().join("laptop");

    let mut args = os(&flags.iter().map(String::as_str).collect::<Vec<_>>());
    args.extend(os(&["pair", "--name", "desk"]));
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let (mut shower, code, drain) = start_and_read(&args, &desk, "Pairing code: ");
    let out = portal()
        .args(&flags)
        .args(["pair", &code, "--name", "laptop"])
        .env("PORTAL_CONFIG_DIR", &laptop)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("--to desk"));
    assert!(shower.wait().unwrap().success());
    drain.join().unwrap();

    let list = |config: &std::path::Path| {
        let out = portal()
            .arg("devices")
            .env("PORTAL_CONFIG_DIR", config)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let on_desk = list(&desk);
    assert!(
        on_desk.contains("laptop") && on_desk.contains("This device: desk"),
        "{on_desk}"
    );
    assert!(list(&laptop).contains("desk"));

    let out = portal()
        .args(["unpair", "LAPTOP"])
        .env("PORTAL_CONFIG_DIR", &desk)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(list(&desk).contains("No paired devices"));
}

#[test]
fn unknown_devices_and_speeds_are_explained() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let out = portal()
        .args(["--loopback", "send"])
        .arg(&file)
        .args(["--to", "nowhere"])
        .env("PORTAL_CONFIG_DIR", tmp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("portal pair"), "{stderr}");

    let out = portal()
        .args(["send", "--limit", "fast"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("10MB/s"), "{stderr}");
}
