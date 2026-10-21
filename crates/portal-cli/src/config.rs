//! Settings saved between runs, kept as small files in the user's config folder.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Overrides the config folder (for tests).
const CONFIG_DIR_ENV: &str = "PORTAL_CONFIG_DIR";
/// Holds the folder `portal receive` saves into when `--output` is not given.
const DOWNLOAD_DIR_FILE: &str = "download-dir";

/// The folder holding Portal's settings, e.g. `%APPDATA%\portal` on Windows,
/// `~/Library/Application Support/portal` on macOS, `~/.config/portal` elsewhere.
fn config_dir() -> anyhow::Result<PathBuf> {
    let var = |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var(CONFIG_DIR_ENV) {
        return Ok(dir);
    }
    let base = if cfg!(windows) {
        var("APPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Application Support"))
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")))
    };
    base.map(|dir| dir.join("portal"))
        .context("could not find a folder to save settings in")
}

/// The saved download folder, if one was set.
pub fn download_dir() -> anyhow::Result<Option<PathBuf>> {
    let file = config_dir()?.join(DOWNLOAD_DIR_FILE);
    match fs::read_to_string(&file) {
        Ok(text) if !text.trim().is_empty() => Ok(Some(PathBuf::from(text.trim()))),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", file.display())),
    }
}

/// Saves `dir` as the download folder, creating it if missing. Returns the
/// absolute path that was saved, so a relative one keeps meaning the same folder.
pub fn set_download_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let dir =
        std::path::absolute(dir).with_context(|| format!("could not resolve {}", dir.display()))?;
    if dir.exists() && !dir.is_dir() {
        bail!("{} is not a folder", dir.display());
    }
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let Some(text) = dir.to_str() else {
        bail!("{} contains characters that cannot be saved", dir.display());
    };
    let config = config_dir()?;
    fs::create_dir_all(&config)
        .with_context(|| format!("could not create {}", config.display()))?;
    let file = config.join(DOWNLOAD_DIR_FILE);
    fs::write(&file, text).with_context(|| format!("could not write {}", file.display()))?;
    Ok(dir)
}

/// Forgets the saved download folder.
pub fn reset_download_dir() -> anyhow::Result<()> {
    let file = config_dir()?.join(DOWNLOAD_DIR_FILE);
    match fs::remove_file(&file) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            Err(e).with_context(|| format!("could not remove {}", file.display()))
        }
        _ => Ok(()),
    }
}
