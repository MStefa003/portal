# Fuzzing

Each target feeds random, coverage-guided input to something that reads
bytes Portal does not control, and checks that it never panics and that
its guarantees hold:

| Target | Input | Also checks |
|---|---|---|
| `manifest` | manifests from a sender | accepted manifests are valid, round-trip, and every path stays inside the destination |
| `control` | control-stream frames | decoded messages round-trip; sanitized peer text has no control characters |
| `chunk` | chunk frames on data streams | decompression never exceeds the chunk limit; our own chunks decode to the original |
| `handshake` | code handshake and rendezvous messages | garbage never opens a sealed message |
| `code` | codes people type | parsed codes round-trip |
| `path` | paths from manifests | accepted paths only have plain components and stay inside the destination |

CI builds every target on each push, and `.github/workflows/fuzz.yml` runs
each for 10 minutes every night.

To run one yourself (needs nightly Rust):

```console
cargo install cargo-fuzz
cargo +nightly fuzz run manifest -- -max_total_time=60
```

On Windows, add the AddressSanitizer runtime that ships with Visual Studio
to `PATH` first, e.g.
`C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\<version>\bin\Hostx64\x64`.

A failing input is saved under `fuzz/artifacts/<target>/`; replay it with
`cargo +nightly fuzz run <target> <file>`.
