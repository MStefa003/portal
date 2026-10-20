# Portal wire protocol

Current protocol version: **1** (`portal_proto::PROTOCOL_VERSION`).

> Status: draft. All three layers are specified below. Until the first
> release the protocol may change without notice.

## Conventions

- All messages are `serde` types encoded with `postcard`.
- Every frame is `u32` little-endian length followed by the encoded message.
- Every length read from the network is checked against a hard cap *before*
  allocating.
- Peers exchange protocol versions first; a mismatch fails with a clear error.

## Layers

1. **Rendezvous** (WebSocket to `portal-rendezvous`): nameplate allocation and
   an opaque relay between two clients.
2. **Handshake** (relayed): SPAKE2, then one sealed message each way carrying
   the peers' iroh identities.
3. **Peer** (iroh QUIC, ALPN `portal/1`): a control stream plus parallel data
   streams carrying chunks.

## Codes

A code is `<nameplate>-<word>-<word>`, for example `7-cedar-river`.

- **Nameplate**: a decimal number from 1 to 9,999,999, handed out by the
  rendezvous server. It is not secret. The server draws it at random from
  `1..=max(99, 4 × waiting senders)`, so codes stay short.
- **Words**: 2 words chosen uniformly by the sender from a 1,295-word list
  (the EFF Short Wordlist #1 without "yo-yo"; see `NOTICE`), using the OS
  random number generator. That gives about 20.7 bits.
- Parsing is case-insensitive and accepts spaces instead of hyphens. A word
  that is not on the list is rejected *before* any network traffic, with a
  suggestion for the closest word, so typos do not use up the code.
- The canonical form (lowercase, single hyphens) is the SPAKE2 password.

20.7 bits is fine because an attacker gets **one online guess per code**:
the first claim of a nameplate consumes it, and a wrong guess makes the
sender abort. The chance of a successful guess is 1 in 1,677,025.

## Rendezvous protocol (version 1)

WebSocket at `<server>/v1` (`ws://` or `wss://`). Each binary WebSocket
message is one postcard `ClientMsg` or `ServerMsg`; text messages are a
protocol error. WebSocket messages are capped at **8 KiB**.

| # | `ClientMsg` | Meaning |
|---|---|---|
| 0 | `Hello { version, min_version }` | first message |
| 1 | `Allocate` | sender: reserve a nameplate |
| 2 | `Claim { nameplate }` | receiver: join that sender |
| 3 | `Relay(bytes)` | forward to the paired peer (at most 4 KiB, at most 8 per client) |

| # | `ServerMsg` | Meaning |
|---|---|---|
| 0 | `Welcome { version, min_version }` | reply to `Hello` |
| 1 | `Allocated { nameplate }` | reply to `Allocate` |
| 2 | `Paired` | both peers present; relaying is open |
| 3 | `Relay(bytes)` | payload from the peer |
| 4 | `PeerLeft` | the peer disconnected |
| 5 | `Error(code)` | `Version`, `UnknownNameplate`, `Busy`, `Expired` or `Protocol`; the server then closes |

```text
sender                      server                      receiver
  │── Hello ──────────────────►│◄────────────────── Hello ──│
  │◄──────────────── Welcome ──│── Welcome ────────────────►│
  │── Allocate ───────────────►│                            │
  │◄────── Allocated { 7 } ────│                            │
  │   (shows 7-cedar-river)    │◄──────── Claim { 7 } ──────│
  │◄───────────────── Paired ──│── Paired ─────────────────►│
  │◄═══════════ Relay × n ════►│◄═══════════ Relay × n ════►│
```

Server rules:
- `Hello` must arrive within 10 s, and so must `Allocate` or `Claim`.
- A nameplate is single-use: `Claim` removes it, so a second claim gets
  `UnknownNameplate`. It is also freed when the sender disconnects.
- An unclaimed nameplate expires after 1 hour (configurable), and a pairing
  after 60 s.
- Limits on waiting senders and on open connections; beyond them, `Busy`.
- The server pings waiting senders every 30 s to keep proxies from dropping
  idle connections.
- The server logs neither nameplates nor payloads.

## Handshake (protocol version 1)

Runs over `Relay` messages once both peers are `Paired`. Each relayed payload
is one postcard `PeerMsg`:

| # | `PeerMsg` | Meaning |
|---|---|---|
| 0 | `Pake { version, min_version, msg }` | peer protocol versions plus the 33-byte SPAKE2 message |
| 1 | `Sealed(bytes)` | ChaCha20-Poly1305 ciphertext |

1. Both sides send `Pake` right away. SPAKE2 runs over Ed25519 in symmetric
   mode, with password = the canonical code and identity =
   `portal/1 code handshake`. `Pake` is variant 0 forever, so a version
   mismatch shows as a version error rather than a wrong code.
2. From the 32-byte SPAKE2 key `K`, each direction gets its own key:
   `blake3::derive_key("portal 2026-10-21 sealed message from sender", K)`
   and `... from receiver`. Because each key seals exactly one message, the
   nonce is fixed at zero. Separate keys also mean a reflected message never
   opens.
3. The sender sends `Sealed(postcard(EndpointAddr))`: its node ID plus relay
   URL and IP addresses. The receiver sends `Sealed(postcard(EndpointId))`.
4. Failure to open a `Sealed` message means the codes differed: the side
   that notices aborts, and the code is gone.
5. Both close the WebSocket. The receiver dials the sender's `EndpointAddr`
   over iroh. iroh's TLS authenticates the node ID, and the receiver checks it
   again. The sender accepts **only** the node ID from step 3 and closes any
   other connection.

The rendezvous server cannot read or alter the identities without the code
words, so it cannot place itself in the middle. The most it can do is refuse
service.

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
