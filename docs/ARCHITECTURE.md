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

## Transfer flow

```
 sender                                   receiver
 ──────                                   ────────
 scan(root) ──► Manifest ──── bytes ────► Manifest::from_bytes (validates)
                                          Sink::new  (checks conflicts)
                                 ◄─────── sink.missing_chunks()
 Source::read_chunk(hash) ── chunk ─────► sink.write_chunk(hash, data)
                                          sink.finish()
```

In M1 the "bytes" arrows are function calls in one process
(`crates/portal-core/tests/local_transfer.rs`); from M2 they are QUIC streams.

### Chunking and deduplication

`chunk.rs` cuts files with FastCDC: a rolling hash over the content decides
where chunks end, so the boundaries follow the content and not the byte
offsets. Insert a line near the top of a file and only the chunk around the
insertion changes; all later chunks keep their boundaries and their BLAKE3
hashes. Chunks are addressed by hash, so the receiver asks for each distinct
hash once, even when it occurs in several files.

### Why the receiver is safe against a malicious sender

- **Paths.** A `SafeRelPath` can only be created through validation,
  including when it is deserialized. Holding one means the path is relative,
  has no `..`, and is valid on Windows, macOS and Linux (see PROTOCOL.md).
  `SafeRelPath::to_path(dest)` joins it component by component.
- **Existing files.** `Sink::new` checks the destination before anything is
  written. Existing files are a conflict, refused unless overwriting was
  allowed. Symbolic links and file-vs-folder mismatches are always refused,
  so a pre-existing link cannot redirect writes outside the destination.
  The check is repeated immediately before each file is moved into place.
- **Content.** Each chunk is hashed before it is written. Each file is
  assembled in `<dest>/.portal-partial/<id>/`, hashed again as a whole once
  complete, and only then renamed to its final path. A corrupted or
  incomplete transfer never leaves a file at a final path.
- **Sizes.** The manifest is capped at 256 MiB encoded and 1,000,000 entries,
  and chunks at 1 MiB. Collection lengths in the encoding are
  attacker-controlled, but serde caps preallocation, so a lying length
  fails without allocating.

### Known limitation

There is a window between the last check and `rename` in which a local
process could swap a directory for a symlink. Closing it fully needs
handle-relative filesystem APIs (`openat`-style), which would mean either
`unsafe` code or an extra dependency such as `cap-std`; deferred for now.
