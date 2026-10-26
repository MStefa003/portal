# Security

Portal moves files between machines, so its security matters more than
any feature. This page says how to report a problem, what Portal protects
against, and how to check that a download is genuine.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately through
[GitHub security advisories](https://github.com/MStefa003/portal/security/advisories/new)
instead. You will get an answer within a week; once a fix is released, the
advisory is published with credit to you, unless you prefer otherwise.

Only the newest release gets security fixes. `portal update` installs it.

## What Portal protects against

- **Someone between the two machines** (the network, a relay, the DHT, a
  rendezvous server) cannot read or change what is sent. Everything travels
  over QUIC with TLS 1.3, and the two sides prove to each other that they
  know the code with SPAKE2 before anything else happens. Relays and
  rendezvous servers never see the code, the keys or the files.
- **Guessing the code.** A code has about 21 bits of secret words, and each
  code allows one guess: a wrong guess ends the transfer for both sides.
  An attacker has a one in 1.7 million chance per transfer they try to
  intercept, and the sender notices the failure.
- **A malicious sender** cannot write outside the folder the receiver
  chose. Paths are checked against `..`, absolute paths, drive letters,
  Windows device names, alternate data streams and case collisions;
  existing files are not overwritten unless asked; symbolic links are never
  followed; every chunk and every file is checked against its BLAKE3 hash
  before it reaches its final name; compressed data cannot expand beyond
  the 1 MiB chunk limit; the transfer is refused up front when the disk
  is too small; and the receiver is asked before anything is saved.
- **Terminal tricks.** Text and error messages from the other side are
  stripped of control characters before they are printed.
- **Tampered updates.** `portal update` only installs archives signed with
  Portal's release key (below).

Out of scope: a compromised machine on either end, and someone who already
has the code (anyone you give the code to can receive the files, which is
how it is meant to work).

The design is described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
the wire format in [docs/PROTOCOL.md](docs/PROTOCOL.md).

## How it is checked

- Every decoder that reads untrusted bytes (manifests, protocol frames,
  chunks, handshake and rendezvous messages, codes, paths) is fuzzed every
  night; see [fuzz/README.md](fuzz/README.md).
- Tests run real transfers against deliberately malicious peers:
  corrupted chunks, wrong codes, a man in the middle, oversized messages.
- `unsafe` code is forbidden in all of Portal's own crates.
- Every push is checked against the RustSec vulnerability database and a
  license and source policy ([deny.toml](deny.toml)); Dependabot proposes
  dependency updates weekly.

## Checking a download

Every release archive comes with three independent proofs:

1. **Signature** (`.minisig`): made with Portal's release key, which never
   leaves the release workflow's secrets. The public key is

   ```
   RWTIdVXMW8Z0RQixMzNSOsaChfzseZPChiIB/jxRwiB7PD9hBKs33lhi
   ```

   Check it with [minisign](https://jedisct1.github.io/minisign/):

   ```console
   minisign -Vm portal-v0.5.0-x86_64-unknown-linux-gnu.tar.gz -P RWTIdVXMW8Z0RQixMzNSOsaChfzseZPChiIB/jxRwiB7PD9hBKs33lhi
   ```

   The trusted comment names the file, so a genuine archive cannot be
   passed off as another version or platform. `portal update` always checks
   this, and so does `install.sh` when minisign is installed.
2. **Build provenance** (GitHub attestation): proves the archive was built
   by this repository's release workflow from the tagged source:
   `gh attestation verify <archive> --repo MStefa003/portal`.
3. **Checksum** (`.sha256`): catches damaged downloads.
