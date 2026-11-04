# Source provenance

Canonical source: [KroxiDev/RAD at `98fb7ce54a65cf38e57a697b4edbddf598d6319e`](https://github.com/KroxiDev/RAD/tree/98fb7ce54a65cf38e57a697b4edbddf598d6319e)
(RAD `beta` after PR #16; source of Audio and OBS 0.4.1).

The SDK and official plugins are maintained alongside the RAD Host so both peers
change atomically. This repository distributes a snapshot of that commit's
`plugins/`, `crates/rad-plugin-contract/`, `crates/rad-wire/`, one contract-test
fixture (`crates/rad-plugin-host/fixture/manifest-v1.json`) and the test-only fault
probe the media tests include (`scripts/performance/fault_probe.rs`). The fixture is
data only, not a Host implementation or a release executable.

[ source-manifest.json ](source-manifest.json) lists each source/destination SHA-256
and the transforms applied. Only the standalone workspace membership, relative
Cargo/vector paths and Markdown links are rebased: links within the snapshot stay
relative, links to the rest of RAD point to the canonical commit, and the README
gains a provenance section. The shared wire and contract implementations are
unchanged. There are no independently maintained SDK protocol enums. The plugin
READMEs are copied in Spanish, as maintained in RAD; the author documentation in
`docs/` belongs to this repository and stays in English.

Reproduce by exporting those paths from the pinned commit, removing the `plugins/`
prefix, rebasing `sdk/Cargo.toml`'s contract dependency one level up, the SDK vector
includes and the media fault-probe include one level down, and including the two shared crates as workspace
members. Verify the hashes before building with the locked dependency versions.

## Product release build

Build product binaries without test-support/all-features. Keep local compiler
paths out of distributable executables with portable path remapping, for example:

    export RUSTFLAGS="--remap-path-prefix=$HOME=/build-home --remap-path-prefix=$PWD=/src/RAD"
    cargo build --offline --locked --release --workspace --bins

Use an already populated cache for offline builds. Verify the resulting product
executables, manifests, embedded --licenses output and package checksums; do not
ship fixture executables. The compatibility reports use explicitly controlled
fixture backends and do not certify personal services or real hardware.

For SDK consumers pin **`sdk-v0.3.0`**, never a moving branch. The plugin-specific
release tags pin the same canonical source snapshot. Documentation describes
contract major 1 / SDK 0.3.0. A source snapshot is not proof that release packages or
compatibility checks have been published; consult the release notes and their
checksums/reports for the actual tested artifacts.
