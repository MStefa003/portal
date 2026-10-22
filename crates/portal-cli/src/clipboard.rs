//! Reading and writing the clipboard through the tools each OS ships with,
//! so Portal needs no clipboard library: PowerShell on Windows,
//! `pbcopy`/`pbpaste` on macOS, and `wl-clipboard`, `xclip` or `xsel` on Linux.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

/// The clipboard's text.
pub fn get() -> anyhow::Result<String> {
    let (program, args) = paste_command()?;
    let out = Command::new(program)
        .args(args)
        .env("LC_CTYPE", "UTF-8")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {program} to read the clipboard"))?;
    if !out.status.success() {
        bail!(
            "could not read the clipboard: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut text = String::from_utf8(out.stdout).context("the clipboard does not hold text")?;
    // PowerShell ends its output with a line break of its own.
    if cfg!(windows) && text.ends_with("\r\n") {
        text.truncate(text.len() - 2);
    }
    Ok(text)
}

/// Puts `text` on the clipboard.
pub fn set(text: &str) -> anyhow::Result<()> {
    let (program, args) = copy_command()?;
    let mut child = Command::new(program)
        .args(args)
        .env("LC_CTYPE", "UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program} to copy to the clipboard"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .context("could not copy to the clipboard")?;
    }
    let out = child
        .wait_with_output()
        .context("could not copy to the clipboard")?;
    if !out.status.success() {
        bail!(
            "could not copy to the clipboard: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

type Tool = (&'static str, &'static [&'static str]);

#[cfg(windows)]
fn paste_command() -> anyhow::Result<Tool> {
    Ok((
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw",
        ],
    ))
}

#[cfg(windows)]
fn copy_command() -> anyhow::Result<Tool> {
    Ok((
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::InputEncoding = [Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ],
    ))
}

#[cfg(target_os = "macos")]
fn paste_command() -> anyhow::Result<Tool> {
    Ok(("pbpaste", &[]))
}

#[cfg(target_os = "macos")]
fn copy_command() -> anyhow::Result<Tool> {
    Ok(("pbcopy", &[]))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn paste_command() -> anyhow::Result<Tool> {
    linux_tool(&[
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-out"]),
        ("xsel", &["--clipboard", "--output"]),
    ])
}

#[cfg(not(any(windows, target_os = "macos")))]
fn copy_command() -> anyhow::Result<Tool> {
    linux_tool(&[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard", "-in"]),
        ("xsel", &["--clipboard", "--input"]),
    ])
}

/// The first of `tools` that is installed; Wayland's only under Wayland.
#[cfg(not(any(windows, target_os = "macos")))]
fn linux_tool(tools: &[Tool]) -> anyhow::Result<Tool> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    tools
        .iter()
        .filter(|(program, _)| wayland || !program.starts_with("wl-"))
        .find(|(program, _)| {
            Command::new("sh")
                .args(["-c", &format!("command -v {program}")])
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .copied()
        .context(
            "no clipboard tool found. Install wl-clipboard (Wayland) or xclip (X11) to use the clipboard",
        )
}
