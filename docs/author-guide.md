# Writing a RAD plugin

For **contract major 1 / SDK 0.2.0**, Linux x86_64. Read the
[contract](contract-v1.md) first. Plugins are trusted native code, not sandboxed.

## Obtain the SDK

Pin a published tag, not `main`:

```toml
[dependencies]
rad-plugin-sdk = { git = "https://github.com/KroxiDev/rad-plugins", tag = "sdk-v0.2.0" }
```

The tag includes the shared contract and framing crates; Cargo resolves their
relative paths. [SOURCE.md](../SOURCE.md) identifies their canonical RAD commit.
Do not copy a second set of wire enums into your plugin.

## Minimal executable

```rust
use rad_plugin_sdk::*;

struct Hello;
impl Plugin for Hello {
    fn invoke(&self, call: Invoke, reply: Reply<Outcome>) {
        let result = if call.action == "hello" {
            Outcome::Success
        } else {
            Outcome::Failure { code: FailureCode::InvalidParameters }
        };
        drop(call); // Release captured credentials before completing the reply.
        reply.send(result);
    }
    fn options(&self, _query: OptionsQuery, reply: Reply<ChoiceList>) {
        reply.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, subscription: Subscription) {
        subscription.update(VisualState::Unavailable);
    }
    fn unsubscribe(&self, _subscription: SubscriptionId) {}
}

fn main() -> std::process::ExitCode {
    if let Some(code) = licenses_exit() { return code; }
    let config = Config::new(
        "org.example.hello",
        Version { major: 1, minor: 0, patch: 0 },
        1..=1,
    );
    match run_stdio(config, Hello) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
```

Pair it with the minimal manifest in the contract. IDs, version, supported major,
capabilities and credential slots must match both configurations. Add capabilities
with `.capabilities(vec!["visual_freshness".into()])` only when implemented; use
`.credential_slots(...)` for the manifest's declared union of slots.

Validate parameters against your manifest schema before side effects. Handlers
run on the dispatch thread and must return promptly. Move slow I/O to bounded
workers and finish a `Reply` later; dropping it unanswered returns failure or
unavailable. Do not create unbounded threads/queues. Share readers among compatible
consumers; stop them when their final consumer ends. Lazy acquisition is preferred
to prewarming every external integration at startup.

## Visuals, settings and gestures

For a visual, negotiate `visual_freshness`, subscribe to its real source and call
`subscription.publish(state, validity_ms)` **after a completed observation**.
An equal state produces a payload-free renewal; a changed state sends content.
`update(state)` uses the SDK's default validity. No timer renews it for you. A
blocked/stopped reader must eventually let the Host's freshness expire. Keep image
conversion bounded and do not renew while conversion/source access is hung.
Check `is_active()` and release resources on unsubscribe and stdin EOF.

Read `Invoke.settings`, `OptionsQuery.settings` or `Subscription::settings()` as
an immutable snapshot. Missing optional settings need documented defaults. For
example OBS's port is non-secret settings; its password is a credential slot.
Do not introduce global mutable "latest settings" that changes an old operation.

Declare only supported gestures. With `controls`, inspect `call.control`:
HoldStart starts and HoldEnd releases the same original action. Match by control
and immutable context, not by a guessed current selection. Both receive replies;
Start success is not a reason to drop the release state. Do not replay an action
on End. Ordinary invocations and reserved End capacity are separate.

For dynamic choices, list current choices without invoking the action. Preserve a
saved missing value, distinguish unavailable from an empty list, and respect the
query's dependency context. Settings have no dynamic-options query scope in v1.

## Credentials and diagnostics

Use `call.credentials`, `query.credentials` or `subscription.credentials()`;
never pass credentials in settings or action parameters. Keep external sessions
isolated by captured revision/settings. Drop active handles when work really ends,
then retire revision-owned external resources before returning from
`credentials_released`. Never emit a password to a log, visual or error message.
Host redaction is only defense in depth.

Use stderr for short diagnostics, not stdout. The Host retains/rate-limits them;
do not depend on every log line being delivered. Never bypass the SDK writer for
protocol output. `run_stdio` enforces the output deadline; on terminal output
failure the process exits 74 rather than continuing after a truncated frame.

## Build, package and check

From this distribution workspace:

```sh
cargo build --locked --release --workspace --all-features --bins --examples
cargo test --locked --release --workspace --all-features
cargo clippy --locked --release --workspace --all-features --all-targets -- -D warnings
# Build product binaries separately, without test-support/all-features.
export RUSTFLAGS="--remap-path-prefix=$HOME=/build-home --remap-path-prefix=$PWD=/src/RAD"
cargo build --locked --release --workspace --bins
target/release/radp-pack \
  audio/manifest.json target/release/rad-plugin-audio org.rad.audio.radp
```

Add `--offline` when using an already populated cache. Tests require their documented
system tools; they must fail closed rather than connect to personal services.
Preserve licenses and [third-party notices](../notices/README.md). Product binaries
must handle `--licenses` before opening external services or reading host input.

Create a checker scenario covering every declared action/gesture and visual, every
dynamic field, and explicit settings. Use private fake audio/D-Bus/OBS/metrics
services and synthetic credentials. Mark only a safe-to-repeat fixture operation
for pressure; **never repeatedly invoke a real destructive action**. Run:

```sh
rad-plugin-check package.radp scenario.json
```

The [scenario reference](https://github.com/KroxiDev/RAD/blob/main/docs/plugin-check.md)
defines all fields and bounds. Keep the JSON report and specialized semantic
regression results. PASS means those compatibility observations succeeded, not
that code is safe or hardware has been certified. Fix or explicitly report
incomplete/unobservable checks; do not publish them as successful checks.

On a capability/shape incompatibility update the package; do not weaken strict
parsing or restore the prerelease `api >= 2`/`api >= 3` branches. On uncertain
external results, report uncertainty rather than automatically replaying effects.
