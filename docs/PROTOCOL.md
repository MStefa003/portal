# Portal wire protocol

Current protocol version: **2** (`portal_proto::PROTOCOL_VERSION`); the
oldest one still spoken is 1 (`MIN_PROTOCOL_VERSION`). Version 2 adds text
messages and manifests with several top-level items; a sender only uses
them when the negotiated version is 2 and otherwise tells the receiver to
update (see [Peer protocol](#peer-protocol-version-2)).

> Status: draft. All three layers are specified below. Until the first
> release the protocol may change without notice.

## Conventions

- All messages are `serde` types encoded with `postcard`.
- Every frame is `u32` little-endian length followed by the encoded message.
- Every length read from the network is checked against a hard cap *before*
  allocating.
- Peers exchange protocol versions first; a mismatch fails with a clear error.

## Layers

1. **Finding the peer**, one of:
   - **Discovery** (the default, no server): the sender announces its
     nameplate on the local network (mDNS) and in the BitTorrent Mainline
     DHT; the receiver looks it up and dials the sender directly.
   - **Rendezvous** (WebSocket to `portal-rendezvous`, opt-in): nameplate
     allocation and an opaque relay between two clients.
2. **Handshake**: SPAKE2, then one sealed message each way carrying the
   peers' iroh identities. Runs on the direct connection (ALPN
   `portal-pair/1`) or through the rendezvous relay.
3. **Peer** (iroh QUIC): a control stream plus parallel data streams carrying
   chunks. With discovery it follows the handshake on the same connection;
   with rendezvous it is a new connection with ALPN `portal/1`.

Paired ("trusted") devices skip layers 1 and 2 after pairing once; see
[Device pairing](#device-pairing).

## Codes

A code is `<nameplate>-<word>-<word>`, for example `7-cedar-river`.

- **Nameplate**: a decimal number from 1 to 9,999,999. It is not secret.
  With discovery the sender draws it uniformly from the whole range, so
  unrelated senders around the world rarely pick the same one. A rendezvous
  server instead draws it from `1..=max(99, 4 × waiting senders)`, so codes
  stay short.
- **Words**: 2 words chosen uniformly by the sender from a 1,295-word list
  (the EFF Short Wordlist #1 without "yo-yo"; see `NOTICE`), using the OS
  random number generator. That gives about 20.7 bits.
- Parsing is case-insensitive and accepts spaces instead of hyphens. A word
  that is not on the list is rejected *before* any network traffic, with a
  suggestion for the closest word, so typos do not use up the code.
- The canonical form (lowercase, single hyphens) is the SPAKE2 password.

20.7 bits is fine because an attacker gets **one online guess per code**:
a wrong guess makes the sender abort, and (with a rendezvous server) the
first claim of a nameplate consumes it. The chance of a successful guess is
1 in 1,677,025.

## Discovery (default)

The sender announces its nameplate `N` in two places and the receiver
searches both at once, using whichever answers first. Neither is trusted:
the result is only an address to dial, and the handshake decides.

**Mainline DHT** (BEP 44 mutable item, no salt):
- Signing key: Ed25519 with seed
  `blake3::derive_key("portal 2026-10-21 dht key for nameplate", N as u32 big-endian)`.
  Anyone who knows `N` can derive it, which is what lets the receiver find
  the item (and what lets others overwrite it; see the threat model).
- Value: postcard `DhtRecord { version: 1, addr: EndpointAddr }`, at most
  1000 bytes. When the sender has a home relay, `addr` holds only its node
  ID and relay URL, so IP addresses stay out of the public DHT; the direct
  path is found by hole punching. Without a relay it holds the direct
  addresses instead.
- `seq` is the Unix time in seconds. The receiver takes the most recent item
  and ignores it if it is older than 2 hours (a leftover from an earlier
  transfer with the same number). The sender republishes every 30 minutes.
- The receiver retries the lookup every 2 s until its timeout (60 s in the
  CLI), since the sender may still be publishing.

**mDNS** on the local network:
- Service `_portal._udp.local.`, instance `portal-<N>`, port = the sender's
  IPv4 iroh port, A/AAAA records for its interface addresses.
- TXT `id` = the sender's node ID (hex), `port6` = its IPv6 port, if any.
- IPv6 link-local addresses are skipped (they need a scope ID).

## Direct pairing (default)

The receiver dials the discovered address with ALPN `portal-pair/1` and
opens a bidirectional stream. On it, both sides run the handshake below
with length-prefixed `PeerMsg` frames (at most 4 KiB), and each side seals
**its own** node ID. Each side then checks that the node ID it opened equals
the node ID that the QUIC connection's TLS authenticated. A machine that
relays the handshake between the real peers knows neither key nor code: the
peers derive the same key, but each finds the other's sealed node ID differs
from the node it is connected to, and aborts.

The sender handles one pairing connection at a time:
- a connection that fails before its first `Pake` message arrives (within
  15 s) did not try the code; the sender drops it and keeps waiting;
- any failure after that counts as a guess: the sender aborts for good;
- connections with ALPN `portal/1` are refused.

After the handshake, both finish the pairing stream, and the receiver opens
the control stream of the peer protocol on the same connection.

## Device pairing

`portal pair` makes two devices recognize each other later without a code.
Each device has a long-term iroh secret key (its node ID is the public key),
created on first use and kept in its settings folder.

Pairing works like [direct pairing](#direct-pairing-default) with ALPN
`portal-trust/1`: the device that shows the code announces its nameplate
and accepts; the other finds it and dials. After the handshake has proven
both node IDs, the dialing device opens a second bidirectional stream and
sends a `Device { name: String }` frame (postcard, at most 1 KiB); the other
answers with its own and finishes. Names are untrusted: control characters
are escaped and they are cut to 64 bytes. Each side stores the other's node
ID with its name.

To send to a paired device, the sender binds with its long-term key and
waits for a connection with ALPN `portal/1` **from that device's node ID
only**; anything else is closed. The receiver binds with its own key and
dials the sender's node ID: found on the local network through mDNS
(service `_portal._udp.local.`, instance `portal-device-<short node ID>`,
TXT `id` checked against the expected ID) and across the internet through
iroh's address lookup (n0's pkarr/DNS service publishes the node's relay).
TLS authenticates both node IDs, so no code is involved. The peer protocol
then runs on that connection as usual, including the receiver's choice to
accept or decline.

## Rendezvous protocol (version 1, opt-in)

Used when `--rendezvous <url>` or `PORTAL_RENDEZVOUS` is set.

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

Runs on the pairing stream of a direct connection (see above), or over
`Relay` messages once both peers are `Paired` at a rendezvous server. Each
message is one postcard `PeerMsg`:

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
3. Direct: each side sends `Sealed(postcard(EndpointId))` with its own node
   ID. Rendezvous: the sender sends `Sealed(postcard(EndpointAddr))`, its
   node ID plus relay URL and IP addresses, and the receiver sends
   `Sealed(postcard(EndpointId))`.
4. Failure to open a `Sealed` message means the codes differed: the side
   that notices aborts, and the code is gone. Before hanging up it waits
   (up to 3 s) until its own `Sealed` message has been delivered, so the
   other side sees a wrong code rather than a dropped connection.
5. Rendezvous only: both close the WebSocket. The receiver dials the sender's `EndpointAddr`
   over iroh. iroh's TLS authenticates the node ID, and the receiver checks it
   again. The sender accepts **only** the node ID from step 3 and closes any
   other connection.

The rendezvous server cannot read or alter the identities without the code
words, so it cannot place itself in the middle. The most it can do is refuse
service.

## Manifest (format versions 1 and 2)

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
attempt: the partial files under `.portal-partial/<first 16 bytes of the id
in hex>/`. On a new attempt it re-hashes every chunk found there and only
`Want`s the ones that are missing or damaged, and it does not `Want` files
that already exist at their target with the manifest's hash. The sender
needs nothing for this: it serves whatever is asked for.

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
| `version` | 1 or 2 |
| Entry count | 1 to 1,000,000 |
| Roots | the first entry has a one-component path; version 1 has no other such entry, version 2 may have more |
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

**Several items.** Sending several files or folders at once produces one
manifest with one top-level entry per item (format version 2). Their names
must differ, ignoring case. A manifest with a single root keeps version 1,
so older receivers can still read it. The `Offer.name` of a version 2
manifest is "*first name* and *n* more".

## Peer protocol (version 2)

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
| 9 | `Text(string)` | S→R | instead of `Offer` (version 2): a text message, at most 256 KiB; answered by `Done` or `Decline` |

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

A text message replaces everything after `Hello` with one `Text`, which the
receiver answers with `Done`. Receivers escape control characters before
showing it in a terminal.

If the negotiated version is 1 and the sender has text or several items to
send, it sends `Error` with a message asking the receiver to update, and
stops.

Either side may pace its data streams to a speed limit (`--limit`); the
receiver does so by reading more slowly, which QUIC flow control passes on
to the sender. Nothing about this is on the wire.

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
