//! Settings saved between runs. The desktop app reads and writes the same
//! files as the `portal` command line tool, so a download folder set in one
//! is used by the other.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Overrides the config folder (for tests).
const CONFIG_DIR_ENV: &str = "PORTAL_CONFIG_DIR";
/// Holds the folder received files are saved into.
const DOWNLOAD_DIR_FILE: &str = "download-dir";

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The folder holding Portal's settings, as in the command line tool:
/// `%APPDATA%\portal` on Windows, `~/Library/Application Support/portal` on
/// macOS, `~/.config/portal` elsewhere.
fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = var(CONFIG_DIR_ENV) {
        return Some(dir);
    }
    let base = if cfg!(windows) {
        var("APPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Application Support"))
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")))
    };
    base.map(|dir| dir.join("portal"))
}

/// Where received files go: the saved download folder, else the user's
/// Downloads folder, else their home folder.
pub fn download_dir() -> PathBuf {
    let saved = config_dir()
        .and_then(|dir| fs::read_to_string(dir.join(DOWNLOAD_DIR_FILE)).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    if let Some(dir) = saved {
        return PathBuf::from(dir);
    }
    let home = if cfg!(windows) {
        var("USERPROFILE")
    } else {
        var("HOME")
    };
    match home {
        Some(home) if home.join("Downloads").is_dir() => home.join("Downloads"),
        Some(home) => home,
        None => PathBuf::from("."),
    }
}

/// Saves `dir` as the download folder.
pub fn set_download_dir(dir: &Path) -> Result<(), String> {
    let Some(text) = dir.to_str() else {
        return Err(format!(
            "{} contains characters that cannot be saved",
            dir.display()
        ));
    };
    let config = config_dir().ok_or("could not find a folder to save settings in")?;
    let write = || {
        fs::create_dir_all(&config)?;
        fs::write(config.join(DOWNLOAD_DIR_FILE), text)
    };
    write().map_err(|e| match e.kind() {
        ErrorKind::PermissionDenied => {
            format!("not allowed to save settings in {}", config.display())
        }
        _ => format!("could not save the setting: {e}"),
    })
}
