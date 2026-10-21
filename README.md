# Portal

Send files and folders directly between two machines: peer-to-peer,
end-to-end encrypted, resumable, and simple to use.

```console
# Machine A
$ portal send ./project
Code: 4821937-cedar-river
Waiting for receiver...

# Machine B
$ portal receive 4821937-cedar-river
Receiving "project" (1,284 files, 2.3 GB) from 192.168.1.20 [direct]
[████████████░░░░░░░░] 61%  48 MB/s  ETA 0:19
```

> **Status:** early development (milestone M3). Transfers work end to end
> with short codes, with no server to set up.

## Try it

```console
# Machine A
$ portal send ./project
Sending "project" (2 files, 5.00 MB)
Code: 4821937-baker-hazy

On the other machine, run:

  portal receive 4821937-baker-hazy

# Machine B
$ portal receive 4821937-baker-hazy
Receiving "project" (2 files, 5.00 MB) from 192.168.1.20 [direct]
✓ Received 2 files, 5.00 MB into C:\Users\you\Downloads
```

The receiver finds the sender by the number at the start of the code: on
the same network through mDNS (instant, works offline), anywhere else
through the public BitTorrent DHT (a few seconds). Neither ever sees the
words or the files. Each code works once: a wrong guess cancels the
transfer, so nobody gets a second try.

If your network blocks this (some corporate networks block outgoing UDP),
you can pair through a [rendezvous server](docs/SELF_HOSTING.md) you run
yourself instead, with `--rendezvous wss://…/v1` or `PORTAL_RENDEZVOUS`.

## Goals

- One command on each side. No accounts, no config, no port forwarding, no server to run.
- End-to-end encrypted: relays and the rendezvous server never see contents or keys.
- Direct peer-to-peer when possible (QUIC + hole punching), encrypted relay as fallback.
- Resumable and verified: every byte is BLAKE3-checked.
- Content-defined chunking, so re-sending a changed folder only sends what changed.
- Windows, macOS and Linux.

## Installing

Windows (PowerShell):

```console
irm https://raw.githubusercontent.com/MStefa003/portal/main/install.ps1 | iex
```

macOS and Linux:

```console
curl -fsSL https://raw.githubusercontent.com/MStefa003/portal/main/install.sh | sh
```

Or download an archive from the [Releases](https://github.com/MStefa003/portal/releases)
page. Both scripts check the download's SHA-256 before installing.

## Building

```console
cargo build --release
```

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed.

On Windows you need the MSVC toolchain ("Build Tools for Visual Studio" with
the *Desktop development with C++* workload). If you build from Git Bash and
see `link: extra operand`, Git's coreutils `link` is shadowing MSVC's
`link.exe`: build from PowerShell or a Developer Command Prompt instead.

## Workspace

| Crate | Responsibility |
|---|---|
| `portal-core` | Chunking, manifests, hashing, transfer state machine, resume |
| `portal-proto` | Wire protocol: message types, versioning, serialization |
| `portal-net` | iroh/QUIC connections, PAKE handshake, mDNS/DHT discovery, rendezvous client |
| `portal-cli` | The `portal` binary |
| `portal-rendezvous` | Optional self-hostable server that pairs peers by short code |

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
