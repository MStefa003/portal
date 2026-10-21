# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
