# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.5.1] - 2026-10-26

### Added
- Right-click menu on Windows: "Send this folder with Portal" and "Receive
  with Portal here" on a folder's empty space, and paired devices in the
  menus ("Send with Portal → To laptop"), updated by `portal pair` and
  `portal unpair`.
- Right-click menu on Linux: Nemo (Cinnamon) and Thunar (Xfce) entries. The
  terminal opened for it can be chosen with `TERMINAL`, and Ptyxis, GNOME
  Console, kitty, WezTerm and foot are found too.

### Changed
- From the right-click menu, the code is copied to the clipboard, and the
  window closes 5 seconds after a successful transfer instead of waiting
  for Enter (it still waits after an error or a received text message).
- The right-click menu is repaired the next time Portal runs if Portal was
  moved, and brought up to date after an update.
- On Windows, the menu is set up without starting `reg.exe` or PowerShell,
  which security software or policies sometimes blocked.

### Fixed
- `portal send "D:\"` and `--output "D:\"` on Windows: the quote after the
  backslash no longer ends up in the path.

## [0.5.0] - 2026-10-26

### Changed
- `portal send` shows the code at once and reads the files in the
  background, with a progress bar, while the receiver types the code.
  Before, a large file had to be read completely first (about 25 s for a
  20 GB file on a fast SSD).
- Reading a large file is about twice as fast (0.9 to 1.4–1.8 GB/s), with
  exactly the same chunks as before, so resuming and skipping unchanged
  chunks keep working with older versions.

### Fixed
- Large transfers could abort with "too many gaps in stream buffer" when
  the receiving machine was busy and packets were lost. Each data stream
  now has less in flight (2 MiB), with 16 streams instead of 4, so a
  connection still has 32 MiB in flight in total.

## [0.4.0] - 2026-10-23

### Added
- `portal receive` checks for free disk space before accepting a transfer
  and refuses up front, instead of failing halfway with a full disk. The
  sender is told why (protocol version 3; older senders hear "cannot write
  to its destination folder").
- Release archives are signed with Portal's release key (minisign format,
  `.minisig` next to each archive). `portal update` installs only correctly
  signed archives, and `install.sh` checks the signature when `minisign` is
  installed. See SECURITY.md.
- SECURITY.md: how to report vulnerabilities, what Portal protects against,
  and how to verify downloads.
- Fuzzing for every decoder of untrusted input (manifests, frames, chunks,
  handshake and rendezvous messages, codes, paths), run nightly in CI.
- CI also tests the newest stable Rust, audits dependencies with
  cargo-deny (RustSec advisories, licenses, sources), and Dependabot
  proposes updates weekly.
- On Linux, the right-click menu entries show the Portal icon.

### Changed
- Faster: scanning before the code appears is about 2.5 times faster
  (several files are read at once, and the whole-file hash runs alongside
  chunking). Sending reads and compresses the next chunk while the current
  one is on the network, keeps files open between chunks, and stops
  compressing files that do not shrink (video, photos, archives). Chunks are
  verified on all cores instead of the disk-writing thread. Larger QUIC
  flow-control windows let a single transfer fill fast long-distance links.

### Security
- tokio 1.54.0 → 1.54.1: the old version was yanked for a use-after-free in
  its timers (RUSTSEC-2026-0384).
- ctutils 0.4.3 → 0.4.5: constant-time helpers used by the cryptography
  could branch on secret values (RUSTSEC-2026-0385).

## [0.3.1] - 2026-10-22

### Added
- The Portal logo: `portal.exe` now has the Portal icon (also shown by its
  right-click menu entry and "Send to" shortcut) and the name "Portal" in
  its file properties, and the README shows the logo.

## [0.3.0] - 2026-10-22

### Added
- `portal send a b c` sends several files and folders as one transfer with
  one code. They arrive side by side; their names must differ.
- Text messages: `portal send --text "..."` (or `--text -` for standard
  input) and `portal send --clipboard`. The receiver prints the text, made
  safe for the terminal, or copies it with `portal receive --copy`.
- Trusted devices: `portal pair` on one device and `portal pair <code>` on
  the other, once. Then `portal send <files> --to <device>` and
  `portal receive --from <device>` need no code. `portal devices` lists them,
  `portal unpair` forgets one.
- `portal doctor` checks the relay, UDP, NAT type, DHT, local discovery,
  the rendezvous server (if set), the download folder and the version, and
  explains what each result means for transfers.
- `--limit <speed>` on `send` and `receive`, e.g. `--limit 5MB/s`.
- `portal update` installs the newest release (checksum-verified);
  `--check` only reports it, `--force` reinstalls. It refuses, with the
  right command, when a package manager installed Portal.
- `portal context-menu` adds "Send with Portal" to Explorer (plus
  "Send to → Portal" for several items), a Finder Quick Action, or Nautilus
  and Dolphin entries; `--remove` takes it away. The Windows installer adds
  it, and the uninstallers remove it.
- Distribution: Scoop bucket and Homebrew tap in this repository, winget
  manifests, `cargo binstall` metadata, build provenance attestations for
  every release archive, and optional Windows (SignPath) and macOS
  (notarization) signing in the release workflow. See docs/RELEASING.md.

### Changed
- Protocol version 2 (still talks to version 1). Single files and folders
  are sent exactly as before, so 0.2 receivers keep working; text and
  several items need the receiver to update, and tell it so.

## [0.2.0] - 2026-10-22

### Added
- M4: resuming. An interrupted transfer keeps what arrived; when the same
  files are sent again, the receiver re-checks the kept chunks and asks only
  for what is missing. Files that already exist with identical content are
  skipped instead of being reported as conflicts.
- `portal receive` asks "Save it into …? [Y/n]" before writing anything when
  run in a terminal; `--yes` / `-y` skips the question. The sender is told
  when the receiver says no.
- `portal dir <folder>` saves a download folder that `portal receive` uses
  when `--output` is not given; `portal dir` shows it, `--reset` forgets it.
- `portal completions <shell>` prints completions for bash, zsh, fish,
  PowerShell and elvish.
- The sender now shows a progress bar too, and both sides end with the time
  taken and average speed.

## [0.1.0] - 2026-10-21

First release.

### Added
- Serverless pairing, now the default: `portal send` picks a random code
  number and announces it with mDNS on the local network and in the
  BitTorrent Mainline DHT; `portal receive` finds it there and runs the code
  handshake directly on the QUIC connection (ALPN `portal-pair/1`), with each
  side checking the other's sealed node ID against the TLS-authenticated one.
  The DHT record holds only the node ID and relay when a relay is known.
- `--rendezvous` / `PORTAL_RENDEZVOUS` is now an opt-in alternative, and a
  global flag.
- Release workflow building Windows, macOS and Linux (x86_64 and ARM64)
  archives on `v*` tags, plus `install.ps1` / `install.sh`.
- M3: short codes and authenticated pairing:
  - `portal send` shows a code like `7-cedar-river`; `portal receive <code>`
    (or prompts for it). Typos are caught locally with a suggestion.
  - SPAKE2 over a rendezvous relay, then one ChaCha20-Poly1305 sealed message
    each way. The sender accepts only the receiver that proved it knows the
    code. One guess per code.
  - `portal-rendezvous` server: single-use nameplates, expiry, size, count
    and connection limits; Dockerfile and docs/SELF_HOSTING.md.
  - `--rendezvous` / `PORTAL_RENDEZVOUS` to choose the server.
- M2: direct transfers over iroh (QUIC, hole punching, relay fallback):
  - `portal send <path>` prints an address; `portal receive --addr <address>`
    downloads into the current folder (or `--output`), with `--overwrite`.
  - `portal-proto`: versioned control messages, length-prefixed frames with
    hard size caps, zstd chunk frames (compressed only when smaller,
    decompression capped).
  - `portal-net`: receiver-driven transfer, 4 parallel data streams, a single
    writer thread on the receiver, generic error reporting to the peer.
  - Progress bar with speed and ETA; `[direct]` / `[relay]` route display.
  - Tests with real endpoints over loopback, a malicious sender (corrupted
    chunk, incompatible version), and two real `portal` processes.
- M1: local transfer core in `portal-core`:
  - `SafeRelPath`: portable, traversal-proof relative paths (rejects `..`,
    absolute paths, drive letters, UNC, alternate data streams, reserved
    Windows names, trailing dots/spaces), with property tests.
  - FastCDC content-defined chunking (64 KiB / 256 KiB / 1 MiB) with BLAKE3
    chunk and whole-file hashes.
  - `Manifest`: postcard-encoded, versioned, strictly validated on decode,
    with size caps; `transfer_id` = BLAKE3 of the encoding.
  - `scan`: builds a manifest from a file or folder; optional `.gitignore`
    support; skips symlinks, special files and unportable or colliding names.
  - `Source` and `Sink`: hash-addressed chunk serving and verified,
    atomic, conflict-aware writing via `.portal-partial`.
- M0: Cargo workspace with `portal-core`, `portal-proto`, `portal-net`,
  `portal-cli` and `portal-rendezvous` crates.
- `portal` CLI skeleton with `send` and `receive` subcommands (not functional yet).
- CI on Windows, macOS and Linux: fmt, clippy (`-D warnings`) and tests.
- Dual MIT / Apache-2.0 license.

### Changed
- Dev builds no longer use incremental compilation, which kept growing
  `target/` past 4 GB.

### Removed
- The placeholder default rendezvous URL.
- `portal receive --addr`: unauthenticated addresses are replaced by codes.
