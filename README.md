# RAD plugins

The official plugins and SDK live in this workspace so the host and SDK can change
atomically. Host and SDK use the same [`rad-plugin-contract`](crates/rad-plugin-contract)
crate; there is no separate SDK definition of the wire types.

The authoritative documentation for authors is in **[KroxiDev/rad-plugins](https://github.com/KroxiDev/rad-plugins)**:

- [Plugin contract v1](https://github.com/KroxiDev/rad-plugins/blob/main/docs/contract-v1.md)
- [Author guide](https://github.com/KroxiDev/rad-plugins/blob/main/docs/author-guide.md)
- [Published packages](https://github.com/KroxiDev/rad-plugins/releases)

Contract v1 uses a major version and negotiated capabilities, immutable settings
snapshots, source-observed visual renewal, and bounded nonblocking SDK output.
The old linear host APIs 2/3 are not contract v1. Use matching host and plugin
releases; installing a new RAD does not silently update installed packages.
Previously installed prerelease packages remain read-only inventory until updated:
their original metadata and previous version are preserved, but their old protocol
is never executed as contract v1. Package administration remains available.

| Directory | Crate | Integration |
| --- | --- | --- |
| [`sdk`](sdk) | `rad-plugin-sdk` | Shared contract, handshake, dispatch, bounded output and `radp-pack`. |
| [`audio`](audio) | `rad-plugin-audio` | Microphone and output audio through `pactl`. |
| [`media`](media) | `rad-plugin-media` | MPRIS playback and cover art through the session D-Bus. |
| [`obs`](obs) | `rad-plugin-obs` | OBS actions, dynamic choices and visual state. |
| [`metrics`](metrics) | `rad-plugin-metrics` | CPU/RAM and NVIDIA GPU/VRAM visual state. |

Fixtures are test instruments, never release artifacts. Tests do not certify real
hardware, every distribution, or the safety of an arbitrary native plugin.

## Build, test, package

```sh
cargo build --locked --release --workspace --all-features --bins --examples
cargo test --locked --release --workspace --all-features
cargo clippy --locked --release --workspace --all-features --all-targets -- -D warnings
```

Audio unit and protocol tests drive a fake `pactl`. The production no-server test
runs the real `pactl` inside a `bwrap` sandbox with private HOME, runtime, temporary,
device, network and PID namespaces and read-only system executables/libraries.
It cannot reach the host audio server. The isolation guard fails closed if the
required sandbox cannot run. `bwrap`, `pactl` and Linux namespace support must be
available beforehand; the tests do not install dependencies. The media plugin's
tests run against a private `dbus-daemon` (it must be installed) with fake MPRIS players, never the
session bus. OBS tests use synthetic credentials and fake loopback servers on ephemeral ports,
never an installed OBS instance or its default port. Metrics tests use controlled proc
files and a fake NVML library, never system sensors or the real driver. Its
`metrics-fixture` example (feature `test-support`) runs the same plugin/SDK with fixed
readings and the official manifest; it supports `--licenses`. `test-support` enables fixture constructors and examples;
it does not change the production OBS endpoint. If you run a single test target (`cargo test --test process`),
build the helper examples first. With an already populated cache, add `--offline` to these commands.

A plugin package (`.radp`) is the manifest and the executable in one file:

```sh
cargo build --release -p rad-plugin-audio
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    audio/manifest.json target/release/rad-plugin-audio org.rad.audio.radp
```

`radp-pack <manifest.json> <executable> <output.radp>` writes the RADP v1 container: `RADP`, the
format version, the manifest and executable lengths, then the two blobs. The manifest must be
1 to 65536 bytes and the executable 1 byte to 64 MiB; anything else is refused before it is read.

## Licenses and distribution

Project code is [MIT licensed](LICENSE). This does **not** relicense dependencies;
[third-party notices](notices/README.md) preserve their original terms and texts,
including conservatively retained build/target-specific and Rust library notices.
The source tree includes those copies. All plugin executables embed all retained
texts and print them with `--licenses` as their first argument, then exit without
reading host input or connecting to audio/D-Bus/OBS or loading NVML. The notices therefore travel inside
the executable even in the two-blob RADP container. Output failure returns a nonzero
exit code. Keep the include list in `sdk/src/licenses.rs` synchronized when dependencies
or retained notices change, and verify it against original dependency/toolchain files.

GLib/GObject/GIO and the C runtime are dynamically linked system dependencies, not
bundled copies. `pactl` is a separately installed program, not bundled. Their licenses
remain applicable; consult the distribution's packages for their source and notices.
This source documentation is self-contained; using the plugin does not require access
to a private application repository. A source tree or package prepared locally is not
proof that a public repository or release has been published.

## Source provenance

This is a mechanical distribution snapshot, not a second protocol definition.
See [SOURCE.md](SOURCE.md) for the canonical RAD commit, source hashes and the
small path-only rebases needed by this standalone workspace. Pin `sdk-v0.2.0`.
