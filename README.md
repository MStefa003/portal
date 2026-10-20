# Portal

Send files and folders directly between two machines: peer-to-peer,
end-to-end encrypted, resumable, and simple to use.

```console
# Machine A
$ portal send ./project
Code: 7-cedar-river
Waiting for receiver...

# Machine B
$ portal receive 7-cedar-river
Receiving "project" (1,284 files, 2.3 GB) from 192.168.1.20 [direct]
[████████████░░░░░░░░] 61%  48 MB/s  ETA 0:19
```

> **Status:** early development (milestone M3). Transfers work end to end
> with short codes. There is no public rendezvous server yet, so you run your
> own; it takes one command.

## Try it (M3)

```console
# Anywhere both machines can reach (see docs/SELF_HOSTING.md)
$ portal-rendezvous --listen 0.0.0.0:8080

# Machine A
$ portal send --rendezvous ws://192.168.1.10:8080/v1 ./project
Sending "project" (2 files, 5.00 MB)
Code: 22-baker-hazy

On the other machine, run:

  portal receive --rendezvous ws://192.168.1.10:8080/v1 22-baker-hazy

# Machine B
$ portal receive --rendezvous ws://192.168.1.10:8080/v1 22-baker-hazy
Receiving "project" (2 files, 5.00 MB) from 192.168.1.20 [direct]
✓ Received 2 files, 5.00 MB into C:\Users\you\Downloads
```

Set `PORTAL_RENDEZVOUS` once to skip the flag. Each code works once: a wrong
guess cancels the transfer, so nobody gets a second try.

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

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/PROTOCOL.md](docs/PROTOCOL.md)
and [docs/SELF_HOSTING.md](docs/SELF_HOSTING.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.

Code words come from the [EFF Short Wordlist #1](https://www.eff.org/dice)
by the Electronic Frontier Foundation, licensed under
[CC BY 3.0 US](https://creativecommons.org/licenses/by/3.0/us/) (one word
removed). See [NOTICE](NOTICE).
