# Portal wire protocol

Current protocol version: **1** (`portal_proto::PROTOCOL_VERSION`).

> Status: draft. Message types are specified as they are implemented
> (M1–M3). Until the first release the protocol may change without notice.

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
