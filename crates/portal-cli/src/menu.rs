//! `portal context-menu`: a "Send with Portal" entry in the file manager's
//! right-click menu. It opens a terminal window running `portal send
//! --pause` with the selected files, which shows the code and stays open
//! until the transfer is done.
//!
//! - Windows: a "Send with Portal" verb for files and folders (under "Show
//!   more options" on Windows 11), plus "Send to → Portal", which sends
//!   several selected items as one transfer. Per-user; no admin rights.
//! - macOS: a Finder Quick Action ("Quick Actions → Send with Portal").
//! - Linux: a Nautilus (GNOME Files) script and a Dolphin (KDE) service menu.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::process::Command;

use anyhow::Context;
#[cfg(windows)]
use anyhow::bail;

use crate::ui;

/// The label shown in menus.
const LABEL: &str = "Send with Portal";

/// Adds the menu entries (or, with `remove`, takes them away).
pub fn run(remove: bool) -> anyhow::Result<()> {
    // As started, not canonicalized: that would give `\\?\` paths on Windows
    // and, under Homebrew, a versioned path that an upgrade removes.
    let exe = std::env::current_exe().context("could not find this program's file")?;
    if remove {
        uninstall()?;
        ui::success(format!("Removed \"{LABEL}\" from the right-click menu"));
    } else {
        install(&exe)?;
        ui::success(format!("Added \"{LABEL}\" to the right-click menu"));
        println!("{}", where_to_find());
    }
    Ok(())
}

#[cfg(not(windows))]
fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("could not find your home folder")
}

#[cfg(not(windows))]
fn write_file(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("could not remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(windows)]
fn run_quietly(command: &mut Command, what: &str) -> anyhow::Result<()> {
    let out = command
        .output()
        .with_context(|| format!("could not {what}"))?;
    if !out.status.success() {
        bail!(
            "could not {what}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

// --- Windows ---

/// Registry keys for the verb, under HKEY_CURRENT_USER.
#[cfg(windows)]
const VERB_KEYS: [&str; 2] = [
    r"Software\Classes\*\shell\Portal",
    r"Software\Classes\Directory\shell\Portal",
];

#[cfg(windows)]
fn send_to_shortcut() -> anyhow::Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA")
        .filter(|a| !a.is_empty())
        .context("could not find your AppData folder")?;
    Ok(PathBuf::from(appdata).join(r"Microsoft\Windows\SendTo\Portal.lnk"))
}

/// A `.reg` file string: backslashes and quotes escaped.
#[cfg(windows)]
fn reg_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

#[cfg(windows)]
fn reg_import(contents: &str) -> anyhow::Result<()> {
    let file = std::env::temp_dir().join(format!("portal-menu-{}.reg", std::process::id()));
    // reg.exe reads .reg files as UTF-16 with a byte order mark.
    let mut bytes = vec![0xff, 0xfe];
    for unit in contents.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&file, bytes).with_context(|| format!("could not write {}", file.display()))?;
    let result = run_quietly(
        Command::new("reg").arg("import").arg(&file),
        "change the right-click menu",
    );
    let _ = fs::remove_file(&file);
    result
}

#[cfg(windows)]
fn install(exe: &Path) -> anyhow::Result<()> {
    let exe = exe.display().to_string();
    let command = format!("\"{exe}\" send --pause \"%1\"");
    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n");
    for key in VERB_KEYS {
        reg.push_str(&format!(
            "\r\n[HKEY_CURRENT_USER\\{key}]\r\n@={}\r\n\"Icon\"={}\r\n\"MultiSelectModel\"=\"Single\"\r\n\r\n[HKEY_CURRENT_USER\\{key}\\command]\r\n@={}\r\n",
            reg_string(LABEL),
            reg_string(&exe),
            reg_string(&command),
        ));
    }
    reg_import(&reg)?;

    // "Send to" passes every selected item to one process: one transfer.
    let shortcut = send_to_shortcut()?;
    run_quietly(
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:PORTAL_LNK); \
                 $s.TargetPath = $env:PORTAL_EXE; $s.Arguments = 'send --pause'; \
                 $s.IconLocation = $env:PORTAL_EXE; $s.Description = 'Send with Portal'; $s.Save()",
            ])
            .env("PORTAL_LNK", &shortcut)
            .env("PORTAL_EXE", &exe),
        "create the Send to shortcut",
    )
}

#[cfg(windows)]
fn uninstall() -> anyhow::Result<()> {
    let mut reg = String::from("Windows Registry Editor Version 5.00\r\n");
    for key in VERB_KEYS {
        reg.push_str(&format!("\r\n[-HKEY_CURRENT_USER\\{key}]\r\n"));
    }
    reg_import(&reg)?;
    remove_path(&send_to_shortcut()?)
}

#[cfg(windows)]
fn where_to_find() -> &'static str {
    "Right-click a file or folder and choose \"Send with Portal\" (on Windows 11, under \"Show more options\"),\nor select several and choose \"Send to\" → \"Portal\" to send them together."
}

// --- macOS ---

#[cfg(target_os = "macos")]
fn workflow() -> anyhow::Result<PathBuf> {
    Ok(home()?.join("Library/Services/Send with Portal.workflow"))
}

#[cfg(target_os = "macos")]
fn install(exe: &Path) -> anyhow::Result<()> {
    let dir = workflow()?;
    // Opens Terminal with `portal send --pause` and the selected items.
    let script = format!(
        r#"cmd={}
for f in "$@"; do
  cmd="$cmd '$(printf '%s' "$f" | sed "s/'/'\\\\''/g")'"
done
osascript -e 'on run argv' -e 'tell application "Terminal"' -e 'activate' -e 'do script (item 1 of argv)' -e 'end tell' -e 'end run' "$cmd"
"#,
        shell_quote(&format!(
            "{} send --pause",
            shell_quote(&exe.display().to_string())
        ))
    );
    write_file(
        &dir.join("Contents/Info.plist"),
        &MACOS_INFO_PLIST.replace("{LABEL}", LABEL),
    )?;
    write_file(
        &dir.join("Contents/document.wflow"),
        &MACOS_WORKFLOW.replace("{SCRIPT}", &xml_escape(&script)),
    )?;
    // Make the Services menu notice the new Quick Action now.
    let _ = Command::new("/System/Library/CoreServices/pbs")
        .arg("-update")
        .output();
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall() -> anyhow::Result<()> {
    remove_path(&workflow()?)?;
    let _ = Command::new("/System/Library/CoreServices/pbs")
        .arg("-update")
        .output();
    Ok(())
}

#[cfg(target_os = "macos")]
fn where_to_find() -> &'static str {
    "In Finder, right-click files or folders and choose Quick Actions → \"Send with Portal\".\nIf it is missing, enable it in System Settings → Keyboard → Keyboard Shortcuts → Services."
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(target_os = "macos")]
const MACOS_INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSMenuItem</key>
			<dict>
				<key>default</key>
				<string>{LABEL}</string>
			</dict>
			<key>NSMessage</key>
			<string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict>
				<key>NSApplicationIdentifier</key>
				<string>com.apple.finder</string>
			</dict>
			<key>NSSendFileTypes</key>
			<array>
				<string>public.item</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
"#;

#[cfg(target_os = "macos")]
const MACOS_WORKFLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>523</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{SCRIPT}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>1</integer>
					<key>shell</key>
					<string>/bin/sh</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Category</key>
				<array>
					<string>AMCategoryUtilities</string>
				</array>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>6D9E3D5A-7B1E-4C5F-9C1A-2F3B4C5D6E70</string>
				<key>Keywords</key>
				<array>
					<string>Shell</string>
					<string>Script</string>
				</array>
				<key>OutputUUID</key>
				<string>6D9E3D5A-7B1E-4C5F-9C1A-2F3B4C5D6E71</string>
				<key>UUID</key>
				<string>6D9E3D5A-7B1E-4C5F-9C1A-2F3B4C5D6E72</string>
				<key>UnlocalizedApplications</key>
				<array>
					<string>Automator</string>
				</array>
				<key>isViewVisible</key>
				<integer>1</integer>
			</dict>
			<key>isViewVisible</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>applicationBundleIDsByPath</key>
		<dict/>
		<key>applicationPaths</key>
		<array/>
		<key>inputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject</string>
		<key>outputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>presentationMode</key>
		<integer>15</integer>
		<key>processesInput</key>
		<false/>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<false/>
		<key>systemImageName</key>
		<string>NSActionTemplate</string>
		<key>useAutomaticInputType</key>
		<false/>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#;

// --- Linux and other Unix ---

#[cfg(not(any(windows, target_os = "macos")))]
fn data_home() -> anyhow::Result<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(home()?.join(".local/share")),
    }
}

/// Files this creates: a launcher that opens a terminal, and the entries
/// for Nautilus and Dolphin that call it.
#[cfg(not(any(windows, target_os = "macos")))]
fn linux_files() -> anyhow::Result<[PathBuf; 3]> {
    let data = data_home()?;
    Ok([
        data.join("portal/send-in-terminal"),
        data.join(format!("nautilus/scripts/{LABEL}")),
        data.join("kio/servicemenus/portal-send.desktop"),
    ])
}

#[cfg(not(any(windows, target_os = "macos")))]
fn install(exe: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let [launcher, nautilus, dolphin] = linux_files()?;
    let portal = shell_quote(&exe.display().to_string());
    write_file(
        &launcher,
        &format!(
            r#"#!/bin/sh
# Opens a terminal running `portal send` with the given files.
# Created by `portal context-menu`; removed by `portal context-menu --remove`.
portal={portal}
for t in x-terminal-emulator gnome-terminal konsole xfce4-terminal alacritty xterm; do
  command -v "$t" >/dev/null 2>&1 || continue
  case "$t" in
    gnome-terminal) exec gnome-terminal -- "$portal" send --pause "$@" ;;
    xfce4-terminal) exec xfce4-terminal -x "$portal" send --pause "$@" ;;
    *) exec "$t" -e "$portal" send --pause "$@" ;;
  esac
done
echo "No terminal program found to run Portal in." >&2
exit 1
"#
        ),
    )?;
    let launcher_q = shell_quote(&launcher.display().to_string());
    write_file(
        &nautilus,
        &format!(
            "#!/bin/sh\n# Nautilus passes the selected files as arguments.\nexec {launcher_q} \"$@\"\n"
        ),
    )?;
    write_file(
        &dolphin,
        &format!(
            "[Desktop Entry]\nType=Service\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nMimeType=all/all;\nActions=send\n\n[Desktop Action send]\nName={LABEL}\nIcon=document-send\nExec={} %F\n",
            launcher.display()
        ),
    )?;
    for file in [&launcher, &nautilus, &dolphin] {
        fs::set_permissions(file, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("could not make {} executable", file.display()))?;
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn uninstall() -> anyhow::Result<()> {
    for file in linux_files()? {
        remove_path(&file)?;
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn where_to_find() -> &'static str {
    "In GNOME Files (Nautilus), right-click files and choose Scripts → \"Send with Portal\".\nIn Dolphin (KDE), right-click and look under Actions."
}

/// Quotes `s` for a POSIX shell.
#[cfg(not(windows))]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
