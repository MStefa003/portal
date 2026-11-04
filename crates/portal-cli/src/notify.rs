//! Desktop notifications, for when a transfer ends or needs an answer while
//! nobody may be looking at the terminal. Like the clipboard, they go
//! through tools each OS ships with, so Portal needs no notification
//! library: a PowerShell toast on Windows, `osascript` on macOS and
//! `notify-send` on Linux. Best effort: without the tool there is simply
//! no notification. `portal notifications off` turns them off.

use std::process::{Command, Stdio};
use std::time::Duration;

/// A transfer started in a terminal notifies when it ends only if it took
/// at least this long: shorter ones were most likely watched.
pub const LONG_TRANSFER: Duration = Duration::from_secs(60);

/// Longest text shown; file names from the other side can be long.
const MAX_TEXT: usize = 120;

/// Shows a notification with `title` and `body`, unless they are turned off.
/// Returns at once.
pub fn show(title: &str, body: &str) {
    if !crate::config::notifications_enabled() {
        return;
    }
    let (title, body) = (shorten(title), shorten(body));
    if let Err(e) = spawn(&title, &body) {
        tracing::debug!(%e, "could not show a notification");
    }
}

fn shorten(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
/// Escapes `&`, `<` and `>` (and quotes) for the toast XML on Windows and
/// for `notify-send`, whose body may be read as markup.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn quiet(command: &mut Command) -> &mut Command {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
}

// --- Windows ---

/// The name toasts are shown under. Registered in the registry with a
/// display name and icon (see [`register`]); no Start menu shortcut needed.
#[cfg(windows)]
const APP_ID: &str = "MStefa003.Portal";

/// Builds the toast from `PORTAL_TOAST` (XML, passed through the
/// environment so nothing in it is ever read as PowerShell).
#[cfg(windows)]
const TOAST_SCRIPT: &str = "\
$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime];\
$null = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom, ContentType = WindowsRuntime];\
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument;\
$xml.LoadXml($env:PORTAL_TOAST);\
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($env:PORTAL_APP_ID).Show([Windows.UI.Notifications.ToastNotification]::new($xml))";

#[cfg(windows)]
fn spawn(title: &str, body: &str) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    /// No console window flashing up for PowerShell.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    register();
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape_markup(title),
        escape_markup(body)
    );
    quiet(&mut Command::new("powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", TOAST_SCRIPT])
        .env("PORTAL_TOAST", xml)
        .env("PORTAL_APP_ID", APP_ID)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

/// The registry key that makes Windows show [`APP_ID`]'s toasts as Portal's
/// (`uninstall.ps1` removes it).
#[cfg(windows)]
const APP_ID_KEY: &str = r"Software\Classes\AppUserModelId\MStefa003.Portal";

/// Names Portal (with its icon, saved next to the settings) as the source
/// of its toasts. Without it Windows would not show them.
#[cfg(windows)]
fn register() {
    const ICON: &[u8] = include_bytes!("../../../packaging/icon/app-256.png");
    let result = (|| -> anyhow::Result<()> {
        let icon = crate::config::config_file("notification-icon.png")?;
        if std::fs::metadata(&icon).map(|m| m.len()).ok() != Some(ICON.len() as u64) {
            std::fs::write(&icon, ICON)?;
        }
        let key = windows_registry::CURRENT_USER.create(APP_ID_KEY)?;
        key.set_string("DisplayName", "Portal")?;
        key.set_string("IconUri", icon.to_string_lossy().as_ref())?;
        Ok(())
    })();
    if let Err(e) = result {
        tracing::debug!(%e, "could not register Portal for notifications");
    }
}

// --- macOS ---

#[cfg(target_os = "macos")]
fn spawn(title: &str, body: &str) -> anyhow::Result<()> {
    // Passed as arguments, so nothing in them is read as AppleScript.
    quiet(&mut Command::new("osascript"))
        .args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ])
        .spawn()?;
    Ok(())
}

// --- Linux and other Unix ---

#[cfg(not(any(windows, target_os = "macos")))]
fn spawn(title: &str, body: &str) -> anyhow::Result<()> {
    quiet(&mut Command::new("notify-send"))
        .args(["--app-name=Portal", "--"])
        .arg(title)
        .arg(escape_markup(body))
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_shortened() {
        assert_eq!(shorten("short"), "short");
        let long = "é".repeat(500);
        let short = shorten(&long);
        assert_eq!(short.chars().count(), MAX_TEXT + 1);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escape_markup("<b>\"a\" & b</b>"),
            "&lt;b&gt;&quot;a&quot; &amp; b&lt;/b&gt;"
        );
    }
}
