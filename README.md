# Portal

Send files and folders directly between two machines: peer-to-peer,
end-to-end encrypted, resumable, and simple to use.

```console
# Machine A
$ portal send ./project
Code: 7-sunset-river
Waiting for receiver...

# Machine B
$ portal receive 7-sunset-river
Receiving "project" (1,284 files, 2.3 GB) from 192.168.1.20 [direct]
[████████████░░░░░░░░] 61%  48 MB/s  ETA 0:19
```

> **Status:** early development (milestone M2). Transfers work between two
> machines using a long address; short codes like `7-sunset-river` come in M3.

## Try it (M2)

```console
# Machine A
$ portal send ./project
Sending "project" (2 files, 2.00 MB)

On the other machine, run:

  portal receive --addr cd564b70a3…

# Machine B
$ portal receive --addr cd564b70a3…
Receiving "project" (2 files, 2.00 MB) from 192.168.2.9 [direct]
✓ Received 2 files, 2.00 MB into C:\Users\you\Downloads
```

Until M3 adds the authenticated short-code handshake, anyone who has the
address can connect, so only share it over a channel you trust.

## Goals

- One command on each side. No accounts, no config, no port forwarding.
- End-to-end encrypted: relays and the rendezvous server never see contents or keys.
- Direct peer-to-peer when possible (QUIC + hole punching), encrypted relay as fallback.
- Resumable and verified: every byte is BLAKE3-checked.
- Content-defined chunking, so re-sending a changed folder only sends what changed.
- Windows, macOS and Linux.

## Building

```console
cargo build --release
```

On Windows you need the MSVC toolchain ("Build Tools for Visual Studio" with
the *Desktop development with C++* workload). If you build from Git Bash and
see `link: extra operand`, Git's coreutils `link` is shadowing MSVC's
`link.exe`: build from PowerShell or a Developer Command Prompt instead.

## Workspace

| Crate | Responsibility |
|---|---|
| `portal-core` | Chunking, manifests, hashing, transfer state machine, resume |
| `portal-proto` | Wire protocol: message types, versioning, serialization |
| `portal-net` | iroh/QUIC connections, PAKE handshake, rendezvous client |
| `portal-cli` | The `portal` binary |
| `portal-rendezvous` | Self-hostable server that pairs peers by short code |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/PROTOCOL.md](docs/PROTOCOL.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
