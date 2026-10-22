//! Settings saved between runs, kept as small files in the user's config folder.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use portal_net::{EndpointId, SecretKey};

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

/// This device's long-term secret key, as 64 hex characters. Created on
/// first use. It identifies this device to the devices it was paired with.
const IDENTITY_FILE: &str = "identity";
/// The name this device gave itself when pairing.
const NAME_FILE: &str = "name";
/// Paired devices, one per line: node ID, a tab, the name.
const DEVICES_FILE: &str = "devices";

/// A device this one was paired with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// What the user calls it, e.g. in `--to laptop`.
    pub name: String,
    /// Its node ID, proven during pairing.
    pub id: EndpointId,
}

/// Creates the config folder and returns `file` inside it.
fn config_file(file: &str) -> anyhow::Result<PathBuf> {
    let config = config_dir()?;
    fs::create_dir_all(&config)
        .with_context(|| format!("could not create {}", config.display()))?;
    Ok(config.join(file))
}

fn read_optional(file: &Path) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", file.display())),
    }
}

/// This device's identity, created and saved on first use.
pub fn identity() -> anyhow::Result<SecretKey> {
    let file = config_file(IDENTITY_FILE)?;
    if let Some(text) = read_optional(&file)? {
        let bytes = parse_hex32(text.trim())
            .with_context(|| format!("{} is damaged; delete it and pair again", file.display()))?;
        return Ok(SecretKey::from_bytes(&bytes));
    }
    let key = SecretKey::generate();
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    write_private(&file, &hex)?;
    Ok(key)
}

fn parse_hex32(text: &str) -> anyhow::Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    if text.len() != 64 || !text.is_ascii() {
        bail!("expected 64 hex characters");
    }
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16)?;
    }
    Ok(bytes)
}

/// Writes a file only this user can read (on Unix; Windows keeps files in
/// `%APPDATA%` private to the user already).
fn write_private(file: &Path, text: &str) -> anyhow::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut f = options
        .open(file)
        .with_context(|| format!("could not write {}", file.display()))?;
    std::io::Write::write_all(&mut f, text.as_bytes())
        .with_context(|| format!("could not write {}", file.display()))
}

/// The name this device gave itself when pairing, if it was paired.
pub fn own_name() -> anyhow::Result<Option<String>> {
    let file = config_dir()?.join(NAME_FILE);
    Ok(read_optional(&file)?
        .map(|text| text.trim().to_owned())
        .filter(|name| !name.is_empty()))
}

/// Remembers the name this device gave itself.
pub fn set_own_name(name: &str) -> anyhow::Result<()> {
    let file = config_file(NAME_FILE)?;
    fs::write(&file, name).with_context(|| format!("could not write {}", file.display()))
}

/// Every paired device, in the order they were paired.
pub fn devices() -> anyhow::Result<Vec<Device>> {
    let file = config_dir()?.join(DEVICES_FILE);
    let Some(text) = read_optional(&file)? else {
        return Ok(Vec::new());
    };
    Ok(text
        .lines()
        .filter_map(|line| {
            let (id, name) = line.split_once('\t')?;
            Some(Device {
                id: id.trim().parse().ok()?,
                name: name.trim().to_owned(),
            })
        })
        .collect())
}

fn save_devices(devices: &[Device]) -> anyhow::Result<()> {
    let file = config_file(DEVICES_FILE)?;
    let text: String = devices
        .iter()
        .map(|d| format!("{}\t{}\n", d.id, d.name))
        .collect();
    fs::write(&file, text).with_context(|| format!("could not write {}", file.display()))
}

/// The paired device called `name` (ignoring letter case).
pub fn device(name: &str) -> anyhow::Result<Device> {
    let devices = devices()?;
    if let Some(found) = devices.iter().find(|d| d.name.eq_ignore_ascii_case(name)) {
        return Ok(found.clone());
    }
    if devices.is_empty() {
        bail!("no devices are paired yet. Pair one first with `portal pair`");
    }
    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
    bail!(
        "no paired device is called \"{name}\". Paired devices: {}",
        names.join(", ")
    )
}

/// Saves a newly paired device and returns the name it was saved under:
/// the name it gave, made unique if another device already has it.
/// Pairing the same device again just updates its name.
pub fn add_device(name: &str, id: EndpointId) -> anyhow::Result<String> {
    let mut devices = devices()?;
    devices.retain(|d| d.id != id);
    let taken = |candidate: &str| {
        devices
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case(candidate))
    };
    let mut unique = name.to_owned();
    let mut n = 2;
    while taken(&unique) {
        unique = format!("{name}-{n}");
        n += 1;
    }
    devices.push(Device {
        name: unique.clone(),
        id,
    });
    save_devices(&devices)?;
    Ok(unique)
}

/// Forgets the paired device called `name`; returns its exact name.
pub fn remove_device(name: &str) -> anyhow::Result<String> {
    let found = device(name)?;
    let mut devices = devices()?;
    devices.retain(|d| d.id != found.id);
    save_devices(&devices)?;
    Ok(found.name)
}
