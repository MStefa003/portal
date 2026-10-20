# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
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
