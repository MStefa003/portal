//! `portal context-menu`: a "Send with Portal" entry in the file manager's
//! right-click menu. It opens a terminal window running `portal send
//! --pause` with the selected files, which shows the code (and copies it)
//! and closes once the transfer is done.
//!
//! - Windows: "Send with Portal" for files and folders (under "Show more
//!   options" on Windows 11), "Send this folder with Portal" and "Receive
//!   with Portal here" on a folder's empty space, and "Send to → Portal",
//!   which sends several selected items as one transfer. With paired
//!   devices, each entry is a submenu that also offers them. Per-user; no
//!   admin rights.
//! - macOS: a Finder Quick Action ("Quick Actions → Send with Portal").
//! - Linux: a Nautilus (GNOME Files) script, a Dolphin (KDE) service menu,
//!   a Nemo (Cinnamon) action and a Thunar (Xfce) custom action.
//!
//! The entries remember which program they start, so [`heal`] can point
//! them at a moved Portal and bring them up to date after an update.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use anyhow::Context;

use crate::ui;

/// The label shown in menus.
const LABEL: &str = "Send with Portal";

/// The version of the entries this Portal creates. Raise it when they
/// change, so [`heal`] replaces older ones.
const LAYOUT: u32 = 2;

/// Adds the menu entries (or, with `remove`, takes them away).
pub fn run(remove: bool) -> anyhow::Result<()> {
    // As started, not canonicalized: that would give `\\?\` paths on Windows
    // and, under Homebrew, a versioned path that an upgrade removes.
    let exe = std::env::current_exe().context("could not find this program's file")?;
    if remove {
        uninstall()?;
        ui::success(format!("Removed \"{LABEL}\" from the right-click menu"));
    } else {
        if looks_temporary(&exe) {
            ui::warn(format!(
                "Portal is running from {}, which looks like a build or temporary folder. \
                 The menu stops working if that file is removed; install Portal and run this again from there.",
                exe.display()
            ));
        }
        install(&exe)?;
        ui::success(format!("Added \"{LABEL}\" to the right-click menu"));
        println!("{}", where_to_find());
    }
    Ok(())
}

/// What the installed menu entries start.
struct Installed {
    /// The Portal program they run.
    exe: PathBuf,
    /// The [`LAYOUT`] they were created with.
    layout: u32,
}

/// Repairs installed menu entries, quietly: points them at this program if
/// the one they start is gone, and recreates them if an older Portal made
/// them. Leaves them alone when they start another Portal that still exists.
pub fn heal() {
    let (Some(installed), Ok(exe)) = (installed(), std::env::current_exe()) else {
        return;
    };
    let outdated = if same_file(&installed.exe, &exe) {
        installed.layout < LAYOUT
    } else {
        !installed.exe.exists() && !looks_temporary(&exe)
    };
    if outdated && let Err(e) = install(&exe) {
        tracing::debug!("could not update the right-click menu: {e:#}");
    }
}

/// Recreates installed entries that list paired devices, after pairing or
/// unpairing one.
pub fn refresh_devices() {
    if !cfg!(windows) {
        return;
    }
    if let Some(installed) = installed()
        && installed.exe.exists()
        && let Err(e) = install(&installed.exe)
    {
        ui::warn(format!("could not update the right-click menu: {e:#}"));
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Whether `exe` is a build output or in the temporary folder, where it
/// will not stay.
fn looks_temporary(exe: &Path) -> bool {
    let build = exe.parent().is_some_and(|dir| {
        dir.file_name()
            .is_some_and(|name| name == "debug" || name == "release")
            && dir
                .ancestors()
                .any(|d| d.file_name() == Some("target".as_ref()))
    });
    build || exe.starts_with(std::env::temp_dir())
}

#[cfg(not(windows))]
fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("could not find your home folder")
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("could not write {}", path.display()))
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

// --- Windows ---

/// Where per-user file associations live, under HKEY_CURRENT_USER.
#[cfg(windows)]
const CLASSES: &str = r"Software\Classes";

/// The entry for files. It also records the program (as its icon) and the
/// layout, for [`installed`].
#[cfg(windows)]
const FILE_KEY: &str = r"*\shell\Portal";
#[cfg(windows)]
const FOLDER_KEY: &str = r"Directory\shell\Portal";
/// A folder window's empty space; `%V` is that folder.
#[cfg(windows)]
const BACKGROUND_KEY: &str = r"Directory\Background\shell\Portal";
#[cfg(windows)]
const RECEIVE_KEY: &str = r"Directory\Background\shell\PortalReceive";

/// Names the layout value on [`FILE_KEY`].
#[cfg(windows)]
const LAYOUT_VALUE: &str = "PortalLayout";

#[cfg(windows)]
fn send_to_shortcut() -> anyhow::Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA")
        .filter(|a| !a.is_empty())
        .context("could not find your AppData folder")?;
    Ok(PathBuf::from(appdata).join(r"Microsoft\Windows\SendTo\Portal.lnk"))
}

#[cfg(windows)]
fn install(exe: &Path) -> anyhow::Result<()> {
    // Rebuilt from scratch, so a forgotten device's entry goes too.
    remove_verbs()?;
    let exe_path = exe.display().to_string();
    let portal = win_quote(&exe_path);
    let devices = crate::config::devices().unwrap_or_default();
    // The selected item comes last: `"C:\"` (a drive) would swallow anything
    // after it. Portal turns its trailing quote back into a backslash.
    let send = |item: &str| {
        let mut commands = vec![(
            "With a code".to_owned(),
            format!("{portal} send --pause \"{item}\""),
        )];
        commands.extend(devices.iter().map(|d| {
            (
                format!("To {}", d.name),
                format!(
                    "{portal} send --pause --to {} \"{item}\"",
                    win_quote(&d.name)
                ),
            )
        }));
        commands
    };
    add_verb(FILE_KEY, LABEL, &exe_path, true, &send("%1"))?;
    add_verb(FOLDER_KEY, LABEL, &exe_path, true, &send("%1"))?;
    add_verb(
        BACKGROUND_KEY,
        "Send this folder with Portal",
        &exe_path,
        false,
        &send("%V"),
    )?;
    let mut receive = vec![(
        "With a code".to_owned(),
        format!("{portal} receive --pause --output \"%V\""),
    )];
    receive.extend(devices.iter().map(|d| {
        (
            format!("From {}", d.name),
            format!(
                "{portal} receive --pause --from {} --output \"%V\"",
                win_quote(&d.name)
            ),
        )
    }));
    add_verb(
        RECEIVE_KEY,
        "Receive with Portal here",
        &exe_path,
        false,
        &receive,
    )?;
    open_key(FILE_KEY)?
        .set_u32(LAYOUT_VALUE, LAYOUT)
        .context("could not change the right-click menu")?;

    // "Send to" passes every selected item to one process: one transfer.
    write_file(
        &send_to_shortcut()?,
        shortcut(&exe_path, "send --pause", "Send with Portal"),
    )
}

/// Adds a verb running `commands`: the command itself if there is one, a
/// submenu of them if there are several. `single` hides it when several
/// items are selected, since each would get a window of its own.
#[cfg(windows)]
fn add_verb(
    path: &str,
    label: &str,
    icon: &str,
    single: bool,
    commands: &[(String, String)],
) -> anyhow::Result<()> {
    let set = |key: &windows_registry::Key, name: &str, value: &str| {
        key.set_string(name, value)
            .context("could not change the right-click menu")
    };
    let key = open_key(path)?;
    set(&key, "MUIVerb", label)?;
    set(&key, "Icon", icon)?;
    if single {
        set(&key, "MultiSelectModel", "Single")?;
    }
    match commands {
        [(_, command)] => set(&open_key(&format!(r"{path}\command"))?, "", command),
        _ => {
            // An empty SubCommands makes the verb a submenu of its `shell` key.
            set(&key, "SubCommands", "")?;
            for (i, (label, command)) in commands.iter().enumerate() {
                let item = format!(r"{path}\shell\{i:02}");
                set(&open_key(&item)?, "MUIVerb", label)?;
                set(&open_key(&format!(r"{item}\command"))?, "", command)?;
            }
            Ok(())
        }
    }
}

/// Opens (creating if needed) a key under [`CLASSES`].
#[cfg(windows)]
fn open_key(path: &str) -> anyhow::Result<windows_registry::Key> {
    windows_registry::CURRENT_USER
        .create(format!(r"{CLASSES}\{path}"))
        .context("could not change the right-click menu")
}

#[cfg(windows)]
fn remove_verbs() -> anyhow::Result<()> {
    /// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`: nothing to remove.
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;
    for key in [FILE_KEY, FOLDER_KEY, BACKGROUND_KEY, RECEIVE_KEY] {
        match windows_registry::CURRENT_USER.remove_tree(format!(r"{CLASSES}\{key}")) {
            Err(e) if e.code().0 != NOT_FOUND => {
                return Err(e).context("could not change the right-click menu");
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(windows)]
fn uninstall() -> anyhow::Result<()> {
    remove_verbs()?;
    remove_path(&send_to_shortcut()?)
}

#[cfg(windows)]
fn installed() -> Option<Installed> {
    let key = windows_registry::CURRENT_USER
        .open(format!(r"{CLASSES}\{FILE_KEY}"))
        .ok()?;
    Some(Installed {
        exe: PathBuf::from(key.get_string("Icon").ok()?),
        // Portal 0.5 and older wrote no layout.
        layout: key.get_u32(LAYOUT_VALUE).unwrap_or(1),
    })
}

#[cfg(windows)]
fn where_to_find() -> &'static str {
    "Right-click a file or folder and choose \"Send with Portal\" (on Windows 11, under \"Show more options\"),\n\
     or select several and choose \"Send to\" → \"Portal\" to send them together.\n\
     Right-click the empty space in a folder to send that folder or receive into it.\n\
     Paired devices (`portal pair`) appear in these menus too."
}

/// Quotes `arg` for a Windows command line, as programs split it.
#[cfg(any(windows, test))]
fn win_quote(arg: &str) -> String {
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            // Backslashes before a quote are doubled, and the quote escaped.
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes + 1));
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        quoted.push(c);
    }
    quoted.push_str(&"\\".repeat(backslashes));
    quoted.push('"');
    quoted
}

/// A Windows shortcut (`.lnk`, MS-SHLLINK) to `target` with `arguments`,
/// the target's icon and `description`. The target is given as an
/// "environment variable" path, which Windows resolves like a typed one, so
/// the binary item ID list of a usual shortcut is not needed.
#[cfg(any(windows, test))]
fn shortcut(target: &str, arguments: &str, description: &str) -> Vec<u8> {
    const HAS_NAME: u32 = 0x04;
    const HAS_ARGUMENTS: u32 = 0x20;
    const HAS_ICON_LOCATION: u32 = 0x40;
    const IS_UNICODE: u32 = 0x80;
    const HAS_EXP_STRING: u32 = 0x200;
    /// EnvironmentVariableDataBlock: its size and signature.
    const EXP_STRING_BLOCK: [u32; 2] = [0x314, 0xa000_0001];
    /// MAX_PATH: the block's fixed path length in characters.
    const MAX_PATH: usize = 260;

    let u32le = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    let mut out = Vec::new();
    // ShellLinkHeader.
    u32le(&mut out, 0x4c);
    out.extend_from_slice(&[
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ]);
    u32le(
        &mut out,
        HAS_NAME | HAS_ARGUMENTS | HAS_ICON_LOCATION | IS_UNICODE | HAS_EXP_STRING,
    );
    // Attributes, three times, size and icon index: all zero.
    out.extend_from_slice(&[0; 4 + 24 + 4 + 4]);
    u32le(&mut out, 1); // SW_SHOWNORMAL
    out.extend_from_slice(&[0; 2 + 2 + 4 + 4]); // Hot key, reserved.

    // StringData: counted UTF-16, in this order.
    for s in [description, arguments, target] {
        let units: Vec<u16> = s.encode_utf16().collect();
        out.extend_from_slice(&u16::try_from(units.len()).unwrap_or(u16::MAX).to_le_bytes());
        out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    }

    // The target, as ANSI (for very old readers; Windows uses the Unicode
    // copy) and UTF-16, each in a fixed, zero-padded field.
    for v in EXP_STRING_BLOCK {
        u32le(&mut out, v);
    }
    let mut ansi: Vec<u8> = target
        .chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .take(MAX_PATH - 1)
        .collect();
    ansi.resize(MAX_PATH, 0);
    out.extend_from_slice(&ansi);
    let mut unicode: Vec<u16> = target.encode_utf16().take(MAX_PATH - 1).collect();
    unicode.resize(MAX_PATH, 0);
    out.extend(unicode.iter().flat_map(|u| u.to_le_bytes()));

    // Terminal block.
    u32le(&mut out, 0);
    out
}

// --- macOS ---

#[cfg(target_os = "macos")]
fn workflow() -> anyhow::Result<PathBuf> {
    Ok(home()?.join("Library/Services/Send with Portal.workflow"))
}

/// Inside the workflow: the layout and the program, one per line.
#[cfg(target_os = "macos")]
const MACOS_MARKER: &str = "Contents/Resources/portal";

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
        MACOS_INFO_PLIST.replace("{LABEL}", LABEL),
    )?;
    write_file(
        &dir.join("Contents/document.wflow"),
        MACOS_WORKFLOW.replace("{SCRIPT}", &xml_escape(&script)),
    )?;
    write_file(
        &dir.join(MACOS_MARKER),
        format!("{LAYOUT}\n{}\n", exe.display()),
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
fn installed() -> Option<Installed> {
    let text = fs::read_to_string(workflow().ok()?.join(MACOS_MARKER)).ok()?;
    let (layout, exe) = text.split_once('\n')?;
    Some(Installed {
        exe: PathBuf::from(exe.trim_end_matches('\n')),
        layout: layout.parse().ok()?,
    })
}

#[cfg(target_os = "macos")]
fn where_to_find() -> &'static str {
    "In Finder, right-click files or folders and choose Quick Actions → \"Send with Portal\".\nIf it is missing, enable it in System Settings → Keyboard → Keyboard Shortcuts → Services."
}

#[cfg(any(not(windows), test))]
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

/// Thunar's custom actions, shared with the user's own.
#[cfg(not(any(windows, target_os = "macos")))]
fn thunar_actions() -> anyhow::Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".config"),
    };
    Ok(config.join("Thunar/uca.xml"))
}

/// The Portal logo for the menu entries, in the icon theme as `portal-send`.
#[cfg(not(any(windows, target_os = "macos")))]
const ICON: &[u8] = include_bytes!("../../../packaging/icon/app-256.png");

/// Files this creates: a launcher that opens a terminal, the entries for
/// Nautilus, Dolphin and Nemo that call it, and their icon.
#[cfg(not(any(windows, target_os = "macos")))]
fn linux_files() -> anyhow::Result<[PathBuf; 5]> {
    let data = data_home()?;
    Ok([
        data.join("portal/send-in-terminal"),
        data.join(format!("nautilus/scripts/{LABEL}")),
        data.join("kio/servicemenus/portal-send.desktop"),
        data.join("nemo/actions/portal-send.nemo_action"),
        data.join("icons/hicolor/256x256/apps/portal-send.png"),
    ])
}

/// The launcher's line naming the layout.
#[cfg(not(any(windows, target_os = "macos")))]
const LAYOUT_LINE: &str = "# Layout: ";

#[cfg(not(any(windows, target_os = "macos")))]
fn install(exe: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let [launcher, nautilus, dolphin, nemo, icon] = linux_files()?;
    let portal = shell_quote(&exe.display().to_string());
    write_file(
        &launcher,
        format!(
            r#"#!/bin/sh
# Opens a terminal running `portal send` with the given files.
# Created by `portal context-menu`; removed by `portal context-menu --remove`.
{LAYOUT_LINE}{LAYOUT}
portal={portal}
for t in "$TERMINAL" x-terminal-emulator ptyxis kgx gnome-terminal konsole xfce4-terminal kitty wezterm alacritty foot xterm; do
  [ -n "$t" ] && command -v "$t" >/dev/null 2>&1 || continue
  case "${{t##*/}}" in
    ptyxis) exec "$t" --new-window -- "$portal" send --pause "$@" ;;
    kgx|gnome-terminal) exec "$t" -- "$portal" send --pause "$@" ;;
    xfce4-terminal) exec "$t" -x "$portal" send --pause "$@" ;;
    kitty|foot) exec "$t" "$portal" send --pause "$@" ;;
    wezterm) exec "$t" start -- "$portal" send --pause "$@" ;;
    *) exec "$t" -e "$portal" send --pause "$@" ;;
  esac
done
echo "No terminal program found to run Portal in. Set TERMINAL to the one you use." >&2
exit 1
"#
        ),
    )?;
    let launcher_q = shell_quote(&launcher.display().to_string());
    write_file(
        &nautilus,
        format!(
            "#!/bin/sh\n# Nautilus passes the selected files as arguments.\nexec {launcher_q} \"$@\"\n"
        ),
    )?;
    write_file(
        &dolphin,
        format!(
            "[Desktop Entry]\nType=Service\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nMimeType=all/all;\nActions=send\n\n[Desktop Action send]\nName={LABEL}\nIcon=portal-send\nExec={} %F\n",
            launcher.display()
        ),
    )?;
    write_file(
        &nemo,
        format!(
            "[Nemo Action]\nName={LABEL}\nComment=Send the selected items with Portal\nIcon-Name=portal-send\nExec={launcher_q} %F\nEscape-Spaces=true\nSelection=NotNone\nExtensions=any;\n"
        ),
    )?;
    for file in [&launcher, &nautilus, &dolphin] {
        fs::set_permissions(file, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("could not make {} executable", file.display()))?;
    }
    write_file(&icon, ICON)?;

    // Only into an existing file: Thunar creates it with its own default
    // actions, which a new file would hide.
    let thunar = thunar_actions()?;
    if let Ok(text) = fs::read_to_string(&thunar)
        && let Some(text) = thunar_add(&text, &launcher.display().to_string())
    {
        write_file(&thunar, text)?;
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn uninstall() -> anyhow::Result<()> {
    for file in linux_files()? {
        remove_path(&file)?;
    }
    let thunar = thunar_actions()?;
    if let Ok(text) = fs::read_to_string(&thunar) {
        let without = thunar_remove(&text);
        if without != text {
            write_file(&thunar, without)?;
        }
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn installed() -> Option<Installed> {
    let [launcher, ..] = linux_files().ok()?;
    let text = fs::read_to_string(launcher).ok()?;
    let exe = text.lines().find_map(|l| l.strip_prefix("portal="))?;
    Some(Installed {
        exe: PathBuf::from(shell_unquote(exe)?),
        // Portal 0.5 and older wrote no layout.
        layout: text
            .lines()
            .find_map(|l| l.strip_prefix(LAYOUT_LINE)?.parse().ok())
            .unwrap_or(1),
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn where_to_find() -> &'static str {
    "In GNOME Files (Nautilus), right-click files and choose Scripts → \"Send with Portal\".\n\
     In Dolphin (KDE), right-click and look under Actions. In Nemo (Cinnamon) and Thunar (Xfce),\n\
     it is in the right-click menu itself (Thunar may need a restart: thunar -q)."
}

/// Marks Portal's action among the user's own in Thunar's `uca.xml`.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
const THUNAR_ID: &str = "<unique-id>portal-send</unique-id>";

/// `uca.xml` with Portal's action running `launcher` (replacing an older
/// one), or `None` if the file does not look like Thunar's.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn thunar_add(text: &str, launcher: &str) -> Option<String> {
    let mut text = thunar_remove(text);
    let at = text.rfind("</actions>")?;
    let action = format!(
        "<action>\n\t<icon>portal-send</icon>\n\t<name>{LABEL}</name>\n\t<submenu></submenu>\n\t{THUNAR_ID}\n\
         \t<command>{} %F</command>\n\t<description>Send the selected items with Portal</description>\n\
         \t<range>*</range>\n\t<patterns>*</patterns>\n\t<directories/>\n\t<audio-files/>\n\
         \t<image-files/>\n\t<other-files/>\n\t<text-files/>\n\t<video-files/>\n</action>\n",
        xml_escape(&shell_quote(launcher))
    );
    text.insert_str(at, &action);
    Some(text)
}

/// `uca.xml` without Portal's action.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn thunar_remove(text: &str) -> String {
    let found = text.find(THUNAR_ID).and_then(|id| {
        let start = text[..id].rfind("<action>")?;
        let end = id + text[id..].find("</action>")? + "</action>".len();
        Some((start, end + usize::from(text[end..].starts_with('\n'))))
    });
    match found {
        Some((start, end)) => format!("{}{}", &text[..start], &text[end..]),
        None => text.to_owned(),
    }
}

/// Quotes `s` for a POSIX shell.
#[cfg(any(not(windows), test))]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Undoes [`shell_quote`].
#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn shell_unquote(s: &str) -> Option<String> {
    Some(
        s.strip_prefix('\'')?
            .strip_suffix('\'')?
            .replace(r"'\''", "'"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_quoting_survives_backslashes_and_quotes() {
        assert_eq!(
            win_quote(r"C:\Program Files\portal.exe"),
            r#""C:\Program Files\portal.exe""#
        );
        assert_eq!(win_quote(r"ends\"), r#""ends\\""#);
        assert_eq!(win_quote(r#"a "b" c"#), r#""a \"b\" c""#);
        assert_eq!(win_quote(r#"x\"y"#), r#""x\\\"y""#);
    }

    #[test]
    fn shell_quoting_round_trips() {
        for s in ["/usr/bin/portal", "/home/o'brien/bin/portal", "a b"] {
            assert_eq!(shell_unquote(&shell_quote(s)).as_deref(), Some(s));
        }
    }

    #[test]
    fn thunar_action_is_added_once_and_removed_cleanly() {
        let original = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n<action>\n\t<name>Open Terminal Here</name>\n\t<unique-id>1</unique-id>\n</action>\n</actions>\n";
        let added = thunar_add(original, "/home/me/.local/share/portal/send-in-terminal")
            .expect("valid file");
        assert_eq!(added.matches(THUNAR_ID).count(), 1);
        assert!(added.contains("Open Terminal Here"));
        let again = thunar_add(&added, "/elsewhere/send-in-terminal").expect("valid file");
        assert_eq!(again.matches(THUNAR_ID).count(), 1);
        assert!(again.contains("/elsewhere/"));
        assert_eq!(thunar_remove(&again), original);
        assert_eq!(thunar_add("not thunar", "x"), None);
    }

    #[test]
    fn shortcut_has_the_expected_layout() {
        let target = r"C:\Users\me\portal.exe";
        let lnk = shortcut(target, "send --pause", "Send with Portal");
        let u32_at = |i: usize| u32::from_le_bytes([lnk[i], lnk[i + 1], lnk[i + 2], lnk[i + 3]]);
        assert_eq!(u32_at(0), 0x4c);
        // Header, three counted strings, the target block, the terminal block.
        let strings: usize = ["Send with Portal", "send --pause", target]
            .iter()
            .map(|s| 2 + 2 * s.len())
            .sum();
        assert_eq!(lnk.len(), 0x4c + strings + 0x314 + 4);
        assert_eq!(u32_at(0x4c + strings + 4), 0xa000_0001);
        assert_eq!(u32_at(lnk.len() - 4), 0);
    }

    #[test]
    fn build_folders_look_temporary() {
        assert!(looks_temporary(Path::new(
            "/src/portal/target/release/portal"
        )));
        assert!(looks_temporary(Path::new(
            "/src/portal/target/x86_64-pc-windows-msvc/debug/portal"
        )));
        assert!(!looks_temporary(Path::new("/home/me/.local/bin/portal")));
    }
}
