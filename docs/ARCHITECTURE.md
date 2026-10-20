# Architecture

This document explains how Portal is put together and why. It grows with each
milestone; sections marked *planned* describe the design we are building toward.

## Crates

```
portal-cli ──► portal-net ──► portal-proto
     │              │
     └──────────────┴──► portal-core
portal-rendezvous ──► portal-proto
```

- **portal-core** knows *what* to transfer: manifests, chunks, hashes, path
  safety, resume state. Its transfer logic is a *sans-IO* state machine:
  it consumes messages and emits actions, and never touches a socket. That
  makes it testable in-process, including dropped connections and corruption.
- **portal-proto** defines every byte on the wire, the protocol version and
  hard size caps for anything read from the network.
- **portal-net** moves bytes: iroh endpoints, the rendezvous client and the
  SPAKE2 handshake.
- **portal-cli** is a thin layer: arguments, prompts, progress bars.
- **portal-rendezvous** is a dumb mailbox. It never sees code words, keys or
  file contents.

## Connection flow (planned, M3)

1. The sender asks the rendezvous server for a channel number and generates
   two random words locally: `<channel>-<word>-<word>`.
2. Both peers join the channel and run SPAKE2 using the full code as password.
   A wrong code yields different keys; decryption of the next message fails,
   both sides abort and the channel is burned (one guess per code).
3. Each side sends its iroh node address encrypted with the derived key.
4. Peers connect over iroh (QUIC, hole punching, relay fallback) and verify the
   remote node ID equals the one received in step 3, which defeats a
   man-in-the-middle at the rendezvous server.

## Transfer flow (planned, M1–M4)

Sender builds a manifest (relative paths, sizes, mtimes, chunk hashes);
receiver diffs it against what it already has and requests only missing
chunks; chunks are verified with BLAKE3, written into `.portal-partial`, and
completed files are atomically moved into place.
