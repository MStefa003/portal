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
