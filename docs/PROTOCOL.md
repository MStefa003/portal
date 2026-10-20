# Portal wire protocol

Current protocol version: **1** (`portal_proto::PROTOCOL_VERSION`).

> Status: draft. The manifest (M1) and the peer protocol (M2) are specified
> below; the rendezvous and handshake layers follow in M3. Until the first
> release the protocol may change without notice.

## Conventions

- All messages are `serde` types encoded with `postcard`.
- Every frame is `u32` little-endian length followed by the encoded message.
- Every length read from the network is checked against a hard cap *before*
  allocating.
- Peers exchange protocol versions first; a mismatch fails with a clear error.

## Layers

1. **Rendezvous** (WebSocket to `portal-rendezvous`): channel allocation and an
   opaque mailbox.
2. **Handshake** (inside the mailbox): SPAKE2 messages, then node addresses
   encrypted with the derived key.
3. **Peer** (iroh QUIC, ALPN `portal/1`): a control stream plus parallel data
   streams carrying chunks.

## Manifest (format version 1)

The manifest is sent once, from sender to receiver, and describes everything
being transferred. It is a postcard-encoded `portal_core::Manifest`:

```text
Manifest  { version: u16, entries: Vec<Entry> }
Entry     { path: String, kind: EntryKind }
EntryKind = Dir | File(FileMeta)
FileMeta  { size: u64, mtime: Option<Timestamp>, executable: bool,
            hash: [u8; 32], chunks: Vec<ChunkRef> }
ChunkRef  { hash: [u8; 32], len: u32 }
Timestamp { secs: i64, nanos: u32 }      // relative to the Unix epoch
```

postcard encodes integers as varints, `Vec`/`String` as a varint length
followed by the items, `Option` as a 0/1 tag, and enums as a varint variant
index (`Dir` = 0, `File` = 1).

**Transfer id.** `BLAKE3(encoded manifest)`. Identical input produces an
identical id, which is how a receiver finds the resume state of an earlier
attempt (M4).

**Hashes.** All hashes are BLAKE3-256. `FileMeta.hash` covers the whole file;
`ChunkRef.hash` covers one chunk.

**Chunking.** FastCDC (2020 variant) with min 64 KiB, average 256 KiB and
max 1 MiB. Chunk boundaries are a sender-side choice: the receiver never
re-chunks, it only checks that each chunk matches its hash and length.

### Validation (receiver side, mandatory)

A receiver rejects the whole manifest unless all of the following hold:

| Rule | Limit / check |
|---|---|
| Encoded size | at most 256 MiB, checked before decoding |
| `version` | equal to 1 |
| Entry count | 1 to 1,000,000 |
| Root | exactly one entry with a one-component path, and it comes first |
| Order | each entry's parent is a `Dir` listed earlier |
| Uniqueness | no two paths equal after Unicode lowercasing |
| Chunks | every `len` in 1..=1 MiB, and the lens sum to `size` |
| `mtime.nanos` | below 1,000,000,000 |
| Paths | every path passes the rules below |

### Path rules

A path is `/`-separated UTF-8, at most 4096 bytes and 128 components. Each
component is at most 255 bytes, and:

- is not empty, `.` or `..`;
- contains no control character (Unicode `Cc`) and none of `< > : " / \ | ? *`;
- does not end with a space or `.`;
- is not a Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`,
  `CONOUT$`, `COM0`-`COM9`, `LPT0`-`LPT9`, `COM¹²³`, `LPT¹²³`), compared
  case-insensitively on the part before the first `.`, with trailing spaces
  ignored;
- for the first component only: is not `.portal-partial`.

These rules apply on every OS, so a manifest valid anywhere can be written
anywhere. Senders skip (and report) entries that do not satisfy them.

## Peer protocol (version 1)

One QUIC connection over iroh, ALPN `portal/1`. The receiver dials the
sender. iroh authenticates both ends by their node keys (TLS with raw
public keys), so the receiver knows it reached the node ID it dialed.

### Streams

- **Control stream**: one bidirectional stream, opened by the receiver.
  Frames are `u32 LE length` + postcard `Control`, at most **1 MiB** each.
- **Data streams**: unidirectional, opened by the sender (currently 4, lazily).
  Each carries a sequence of chunks:
  `u32 LE header length` + postcard `ChunkHeader { hash, encoding, raw_len, wire_len }`
  + `wire_len` payload bytes. Header at most 64 bytes; `raw_len` and
  `wire_len` at most 1 MiB. A clean end of stream ends the sequence.

### Control messages

| # | Message | Direction | Meaning |
|---|---|---|---|
| 0 | `Hello { version, min_version }` | both, first | supported versions |
| 1 | `Offer { name, files, bytes, manifest_len, transfer_id }` | S→R | what is coming |
| 2 | `ManifestPart(bytes)` | S→R | next piece of the manifest (512 KiB pieces) |
| 3 | `Accept` | R→S | go ahead |
| 4 | `Decline(reason)` | R→S | refused: `Rejected`, `Conflicts`, `Destination` |
| 5 | `Want([hash])` | R→S | send these chunks (up to 8,192 per message) |
| 6 | `WantDone` | R→S | no more `Want`s |
| 7 | `Done` | R→S | everything received and verified |
| 8 | `Error { code, message }` | both | failure; `message` is untrusted text |

The numbers are postcard variant indices. **`Hello` stays variant 0
forever**, so any two versions can read each other's `Hello` and fail with a
clear version message. New variants are only appended.

### Sequence

```text
receiver                                  sender
   │── open control stream, Hello ───────►│
   │◄──────────────────────────── Hello ──│   both: negotiate() or fail
   │◄──────────────────────────── Offer ──│
   │◄──────────────── ManifestPart × n ───│
   │── Accept | Decline ─────────────────►│
   │── Want × n ─────────────────────────►│
   │── WantDone ─────────────────────────►│
   │◄════════════ chunks on data streams ═│
   │── Done ─────────────────────────────►│
   │◄─────────────────────── close(0) ────│
```

**Version negotiation.** Each side sends `Hello` and reads the peer's. A peer
is compatible if `peer.version >= our min` and `peer.min_version <= our
version`; both then use `min(our version, peer.version)`.

**Receiver checks.** It refuses the manifest unless all of these hold:
`manifest_len` is at most 256 MiB, every part is non-empty, the parts add up
to exactly `manifest_len`, `BLAKE3(manifest) == transfer_id`, the manifest
passes validation, and the `name`, `files` and `bytes` in the offer match it.
Each chunk is decompressed with an output cap of `raw_len` (at most 1 MiB),
then checked against its hash before it is written.

**Compression.** `Zstd` is used only when it makes the chunk smaller;
otherwise `Raw`. Level 3.

**Errors and closing.** When a side detects an error, it sends `Error` with a
generic message. Local paths and details are never sent. It then waits up to
3 s for the peer to close the connection. The side that receives an `Error`
or `Decline` closes right away. This order matters because closing a QUIC
connection discards data still in flight. Close codes: `0` success, `1` error.
