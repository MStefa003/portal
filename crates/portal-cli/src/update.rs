//! `portal update`: replaces this program with the newest release from GitHub.
//!
//! The newest version is read from where `github.com/<repo>/releases/latest`
//! redirects (like the install scripts, so no API rate limit applies). The
//! archive is checked against its published SHA-256 and unpacked with the
//! `tar` that Windows 10+, macOS and Linux include. The running program is
//! replaced by renaming: Windows lets a running `.exe` be renamed but not
//! overwritten, so the old one is left as `portal.exe.old` and removed on
//! the next run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use console::style;
use sha2::{Digest, Sha256};

use crate::ui;

const REPO: &str = "MStefa003/portal";
const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;
/// The programs a release contains; the second is only updated if installed.
const PROGRAMS: [&str; 2] = ["portal", "portal-rendezvous"];

/// A release version, e.g. `v0.3.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// This program's version.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self(0, 0, 0))
    }

    /// Parses `1.2.3` or `v1.2.3`; pre-releases (`1.2.3-rc.1`) are not accepted.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// HTTPS client with the same TLS setup as the rest of Portal (rustls with
/// ring and the Mozilla root certificates), following redirects or not.
fn client(follow_redirects: bool) -> anyhow::Result<reqwest::Client> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("could not set up TLS")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let redirects = if follow_redirects {
        reqwest::redirect::Policy::limited(10)
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(redirects)
        .user_agent(concat!("portal/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("could not set up HTTPS")
}

/// The newest released version.
pub async fn latest() -> anyhow::Result<Version> {
    let response = client(false)?
        .get(format!("https://github.com/{REPO}/releases/latest"))
        .send()
        .await
        .context("could not reach GitHub")?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok())
        .unwrap_or_default();
    location
        .rsplit_once("/releases/tag/")
        .and_then(|(_, tag)| Version::parse(tag))
        .ok_or_else(|| anyhow!("could not find the newest Portal release on GitHub"))
}

/// The release archive name part for this platform, as the release
/// workflow builds them.
fn target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => return None,
    })
}

/// How to update when a package manager or cargo installed this program,
/// which would not notice a replaced file.
fn managed_by(exe: &Path) -> Option<&'static str> {
    let path = exe.to_string_lossy().to_lowercase().replace('\\', "/");
    if path.contains("/target/debug/") || path.contains("/target/release/") {
        Some("This is a development build; update it with `git pull` and `cargo build`.")
    } else if path.contains("/scoop/") {
        Some("Portal was installed with Scoop; update it with `scoop update portal`.")
    } else if path.contains("/winget/") {
        Some("Portal was installed with winget; update it with `winget upgrade MStefa003.Portal`.")
    } else if path.contains("/cellar/")
        || path.contains("/homebrew/")
        || path.contains("/linuxbrew/")
    {
        Some("Portal was installed with Homebrew; update it with `brew upgrade portal`.")
    } else if path.contains("/.cargo/bin/") {
        Some(
            "Portal was installed with cargo; update it with `cargo binstall portal-cli` or `cargo install`.",
        )
    } else {
        None
    }
}

/// `portal update`: installs the newest release, or with `check` only says
/// whether there is one. With `force`, reinstalls it even if it is this
/// version (to repair an installation).
pub async fn run(check: bool, force: bool) -> anyhow::Result<()> {
    let current = Version::current();
    let latest = latest().await?;
    if latest <= current && !force {
        ui::success(format!("Portal {current} is the newest version"));
        return Ok(());
    }
    if latest > current {
        println!("Portal {latest} is available (this is {current}).");
    }
    if check {
        println!("Run `portal update` to install it.");
        return Ok(());
    }
    let exe = std::env::current_exe().context("could not find this program's file")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    if let Some(advice) = managed_by(&exe) {
        bail!("{advice}");
    }
    let target = target().ok_or_else(|| {
        anyhow!("there are no ready-made downloads for this platform; build Portal from source")
    })?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("could not find the folder Portal is installed in"))?;

    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let name = format!("portal-v{latest}-{target}");
    let url = format!("https://github.com/{REPO}/releases/download/v{latest}/{name}.{ext}");
    println!("Downloading {url}");
    let client = client(true)?;
    let archive = download(&client, &url).await?;
    let checksum = String::from_utf8(download(&client, &format!("{url}.sha256")).await?)
        .context("the checksum file is not text")?;
    let expected = checksum.split_whitespace().next().unwrap_or_default();
    let actual: String = Sha256::digest(&archive)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !expected.eq_ignore_ascii_case(&actual) {
        bail!("the download is damaged (checksum mismatch). Please try again.");
    }

    let work = dir.join(format!(".portal-update-{}", std::process::id()));
    let result = install(&archive, ext, &work, &name, dir);
    let _ = fs::remove_dir_all(&work);
    result?;
    if latest > current {
        ui::success(format!("Updated Portal {current} → {latest}"));
    } else {
        ui::success(format!("Reinstalled Portal {latest}"));
    }
    println!(
        "See what changed: {}",
        style(format!("https://github.com/{REPO}/releases/tag/v{latest}")).underlined()
    );
    Ok(())
}

async fn download(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("could not download {url}"))?;
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("could not download {url}"))?
        .to_vec())
}

/// Unpacks `archive` in `work` (inside the install folder, so renames stay
/// on one drive) and swaps the new programs in.
fn install(archive: &[u8], ext: &str, work: &Path, name: &str, dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(work).with_context(|| {
        format!(
            "could not write to {}. Is Portal installed in a folder you can change?",
            dir.display()
        )
    })?;
    let file = work.join(format!("download.{ext}"));
    fs::write(&file, archive).with_context(|| format!("could not write {}", file.display()))?;
    let status = Command::new(tar())
        .arg("-xf")
        .arg(&file)
        .arg("-C")
        .arg(work)
        .status()
        .context("could not run tar to unpack the download")?;
    if !status.success() {
        bail!("could not unpack the download");
    }
    let unpacked = work.join(name);
    for (i, program) in PROGRAMS.iter().enumerate() {
        let file_name = format!("{program}{EXE_SUFFIX}");
        let new = unpacked.join(&file_name);
        let installed = dir.join(&file_name);
        // portal itself always; the others only where they are installed.
        if !new.exists() || (i > 0 && !installed.exists()) {
            continue;
        }
        replace(&new, &installed)?;
    }
    Ok(())
}

/// Moves `new` over `installed`, which may be the running program.
fn replace(new: &Path, installed: &Path) -> anyhow::Result<()> {
    let staged = with_suffix(installed, ".new");
    fs::copy(new, &staged).with_context(|| format!("could not write {}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    }
    if cfg!(windows) && installed.exists() {
        let old = with_suffix(installed, ".old");
        let _ = fs::remove_file(&old);
        fs::rename(installed, &old).with_context(|| {
            format!(
                "could not replace {}. Close other windows where it is running and try again",
                installed.display()
            )
        })?;
    }
    fs::rename(&staged, installed)
        .with_context(|| format!("could not replace {}", installed.display()))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Windows' own `tar.exe` reads zip files; another `tar` on the PATH (such
/// as Git's) may not.
fn tar() -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let tar = Path::new(&root).join("System32").join("tar.exe");
        if tar.exists() {
            return tar;
        }
    }
    PathBuf::from("tar")
}

/// Removes what a previous update left behind (Windows only). Cheap: a
/// couple of file checks.
pub fn clean_up() {
    if !cfg!(windows) {
        return;
    }
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return;
    };
    for program in PROGRAMS {
        let old = dir.join(format!("{program}{EXE_SUFFIX}.old"));
        if old.exists() {
            let _ = fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(Version::parse("v0.10.0"), Some(Version(0, 10, 0)));
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
        assert_eq!(Version::parse("v1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("1.2.3-rc.1"), None);
        assert_eq!(
            Version::current().to_string(),
            env!("CARGO_PKG_VERSION").to_owned()
        );
    }

    #[test]
    fn package_managers_are_detected() {
        let cases = [
            (r"C:\Users\me\scoop\apps\portal\current\portal.exe", "scoop"),
            ("/opt/homebrew/Cellar/portal/0.3.0/bin/portal", "brew"),
            ("/home/me/.cargo/bin/portal", "cargo"),
            (r"C:\src\portal\target\debug\portal.exe", "development"),
        ];
        for (path, expected) in cases {
            let advice = managed_by(Path::new(path)).unwrap();
            assert!(advice.contains(expected), "{path}: {advice}");
        }
        assert!(
            managed_by(Path::new(
                r"C:\Users\me\AppData\Local\Programs\Portal\portal.exe"
            ))
            .is_none()
        );
        assert!(managed_by(Path::new("/home/me/.local/bin/portal")).is_none());
    }
}
