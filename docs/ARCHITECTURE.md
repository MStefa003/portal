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
  hard size caps for anything read from the network. It also holds the
  sans-IO parts of the code handshake: code parsing, SPAKE2 and sealing.
- **portal-net** moves bytes: iroh endpoints, peer discovery (mDNS and the
  Mainline DHT), direct pairing, the rendezvous client, and sending and
  receiving transfers.
- **portal-cli** is a thin layer: arguments, prompts, progress bars.
- **portal-rendezvous** is an optional dumb mailbox (axum, WebSockets), for
  networks where discovery does not work. It never sees code words, keys or
  file contents. It is a library plus a binary, so tests can run it
  in-process.

## Connection flow without a server (default)

1. The sender picks a random nameplate and two random words locally:
   `4821937-cedar-river`. It announces "the sender for 4821937 is node X"
   with mDNS on the local network and, once it has reached a relay, in the
   BitTorrent Mainline DHT under a key derived from the number.
2. The receiver types the code. It is checked against the wordlist offline
   first, then the receiver searches mDNS and the DHT at once and dials the
   first address found, with ALPN `portal-pair/1`.
3. On that connection both run SPAKE2 with the full code as password, then
   each seals its own node ID. Each checks the opened node ID against the one
   the connection's TLS authenticated. A wrong code or a relaying
   man-in-the-middle makes both abort, and the code is gone.
4. The transfer runs on the same connection.

Discovery results are untrusted hints. The DHT key is derivable from the
number alone, so anyone who knows (or brute-forces) a number can read or
overwrite its record. That is harmless to confidentiality: whoever answers
still has to pass the handshake, and gets one guess.

## Connection flow with a rendezvous server (opt-in)

1. The sender connects to the rendezvous server, gets a nameplate (`7`) and
   picks two random words locally: `7-cedar-river`.
2. The receiver types the code. It is checked against the wordlist offline
   first, then the receiver claims nameplate 7. The server pairs the two
   WebSockets and consumes the nameplate.
3. Both run SPAKE2 with the full code as password, relayed by the server.
   A wrong code gives different keys, so the sealed message that follows
   fails to open and both sides abort. One guess per code.
4. The sender seals its iroh `EndpointAddr`; the receiver seals its node ID.
5. They hang up on the server and connect over iroh (QUIC, hole punching,
   relay fallback):
   - the receiver dials exactly the sealed address, and iroh's TLS proves it
     reached that node ID;
   - the sender accepts only the sealed node ID.

   That closes the gap a man-in-the-middle at the rendezvous server would need.

### Threat model

| Attacker | Can | Cannot |
|---|---|---|
| Rendezvous server operator (opt-in) | see nameplates, timing and IP addresses; refuse service | learn the words, swap identities, read files |
| Anyone watching the DHT | find active numbers by brute force (10 M keys) and see the sender's node ID and relay; overwrite the record so the receiver dials them instead | learn the words, see the sender's IP address while it has a relay, get past the handshake |
| Someone on the same LAN | see mDNS announcements: number, node ID, local addresses | learn the words or get past the handshake |
| Network observer | see that two hosts talk QUIC (and to which relay) | read anything: TLS 1.3 inside QUIC |
| iroh relay operator | see node IDs and encrypted traffic volume | read or alter traffic |
| Someone guessing codes | take one guess per code (1 in 1.68 M) and make that transfer fail | try again on the same code |
| Malicious sender | send arbitrary bytes | write outside the destination, overwrite without consent, exhaust memory (see below) |

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

In M1 the "bytes" arrows were function calls in one process
(`crates/portal-core/tests/local_transfer.rs`). Since M2 they are QUIC streams
(`crates/portal-net`).

### Concurrency on the wire (M2)

```
sender                                     receiver
──────                                     ────────
control reader task ─► channel ─┐          control: Want batches, then WantDone
                                ▼
          select! { control msg | worker finished }
Want hashes ─► queue ─► worker ×4 ─ data stream ─► reader task per stream
                 read file (blocking pool)          decompress (blocking pool)
                 zstd if smaller                          │ bounded channel (64)
                                                          ▼
                                               writer thread owns the Sink:
                                               verify, write, finalize files
```

- **The receiver drives the transfer.** It decides what it needs (`Want`), so
  dedup and resume need no changes on the sender side.
- **Frame reads are not cancel-safe.** Dropping a half-read frame would leave
  the stream mid-frame. So every stream is read by its own task, and `select!`
  only ever waits on channels and `JoinSet`s.
- **One writer thread.** The `Sink` does blocking file I/O and keeps state, so
  one thread owns it. The bounded channel feeding it applies backpressure all
  the way back to the sender through QUIC flow control.
- **`JoinSet`s abort their tasks on drop.** Whichever way a transfer ends, no
  background task is left running.

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
