# RAD plugins

Plugins for RAD Action Deck and the small SDK they are built on. This is a standalone Cargo
workspace: it does not depend on any RAD crate, it implements the documented plugin protocol
(length-prefixed JSON frames over stdin and stdout) on its own.

| Directory | Crate | What it is |
| --- | --- | --- |
| [`sdk`](sdk) | `rad-plugin-sdk` | Framing, protocol types, the handshake and dispatch loop with deferred replies and visual heartbeats, and the `radp-pack` packer. |
| [`audio`](audio) | `rad-plugin-audio` | Mute, unmute and toggle the microphone and the audio output through `pactl`. |
| [`media`](media) | `rad-plugin-media` | Control a media player (play, pause, stop, previous, next) and show its cover through MPRIS on the session D-Bus. |

## Build, test, package

```sh
cargo build --release
cargo test                       # also builds the helper examples the tests use
cargo clippy --all-targets -- -D warnings
```

`cargo test` runs without touching real audio: the tests drive a fake `pactl`. The media plugin's
tests run against a private `dbus-daemon` (it must be installed) with fake MPRIS players, never the
session bus. If you run a single test target (`cargo test --test process`), build the helpers first
with `cargo build --examples`.

A plugin package (`.radp`) is the manifest and the executable in one file:

```sh
cargo build --release -p rad-plugin-audio
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    audio/manifest.json target/release/rad-plugin-audio org.rad.audio.radp
```

`radp-pack <manifest.json> <executable> <output.radp>` writes the RADP v1 container: `RADP`, the
format version, the manifest and executable lengths, then the two blobs. The manifest must be
1 to 65536 bytes and the executable 1 byte to 64 MiB; anything else is refused before it is read.

## Writing a plugin with the SDK

Implement `rad_plugin_sdk::Plugin` and hand it to `run_stdio`:

```rust
use rad_plugin_sdk::*;

struct Hello;
impl Plugin for Hello {
    fn invoke(&self, _call: Invoke, reply: Reply<Outcome>) {
        // Slow work goes on another thread; the reply can be sent from anywhere.
        std::thread::spawn(move || reply.send(Outcome::Success));
    }
    fn options(&self, _query: OptionsQuery, reply: Reply<ChoiceList>) {
        reply.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, subscription: Subscription) {
        subscription.update(VisualState::Current {
            content: VisualContent { text: Some("Hello".into()), ..Default::default() },
        });
    }
    fn unsubscribe(&self, _subscription: SubscriptionId) {}
}

fn main() {
    let config = Config::new("org.example.hello", Version { major: 1, minor: 0, patch: 0 }, 2..=2);
    let _ = run_stdio(config, Hello);
}
```

What the SDK does for you:

- **Handshake.** Answers the host's hello with the plugin's own id and version, and refuses (without
  answering) a host API outside the range in `Config`.
- **Deferred replies.** `invoke` and `options` get a `Reply` that can be completed later, from any
  thread. A reply dropped without an answer still answers (`failed` / `unavailable`).
- **Bounded concurrency.** At most 16 actions run at once (the 17th is answered `busy`) and at most 4
  option queries (extra ones are answered `unavailable`); nothing is queued. A handler that panics
  fails only its own request.
- **Visual heartbeat.** The host treats a visual source as stale one second after its last message.
  Every `Subscription` has its last state repeated every 400 ms (never more than 500 ms).
- **Safe output.** Frames from any thread never interleave. Replies and visual states the host would
  reject (bad labels, duplicate option values, oversize frames) are replaced by `unavailable`
  instead of getting the plugin disconnected.

## Standalone protocol reference (host API 2)

The manifest JSON in each plugin directory is the package's public declaration:
`contract: 1`, plugin `id`, semantic `version` object, inclusive `host_min`/`host_max`,
`targets`, `actions` and `visuals`. Each definition has `id`, `label`, versioned
`schema.fields` and `credential_slots`. Fields have `id`, `label`, `required` and
`kind` (`boolean`, bounded `integer`, bounded `text`, fixed `choice`, or
`dynamic_choice` with `depends_on`). Dependencies name earlier choice fields;
API 2 is required for dynamic choices. Parameters are `{schema_version, values}`.
The audio/media manifests are complete examples; the host validates before dispatch.

A RADP v1 file has a 20-byte header: ASCII `RADP`, big-endian u32 version `1`,
big-endian u32 manifest byte count, big-endian u64 executable byte count; then exactly
the manifest JSON and executable bytes, with no paths, compression or trailing data.
Manifest: 1–65536 bytes; executable: 1 byte–64 MiB. `radp-pack` checks these bounds;
it is a container writer, not a manifest semantic validator or code sandbox.

The executable reads stdin and writes stdout as framed UTF-8 JSON: a big-endian u32
length followed by 1–65536 bytes. No logs on stdout. Each envelope is
`{"protocol":1,"correlation":{"generation":1,"request":1},"message":{...}}`.
The host assigns correlations; every reply echoes the request correlation unchanged.
The definitive serde message/type schema is [sdk/src/protocol.rs](sdk/src/protocol.rs).

| Direction | Message (`kind`) | Other fields |
| --- | --- | --- |
| Host → plugin | `hello` (request 1) | `host_api`, `target`, `plugin_id`, `plugin_version` |
| Plugin → host | `ready` | `plugin_id`, `plugin_version` matching the manifest |
| Host → plugin | `options` | `scope` (`action` or `visual`), `definition`, `field`, `context` (Parameters containing only dependency values) |
| Plugin → host | `choices` | `list`: `{availability:"available", options:[{value,label}]}` or `{availability:"unavailable"}` |
| Host → plugin | `invoke` | `action`, `parameters` |
| Plugin → host | `result` | `outcome`: `{status:"success"}` or `{status:"failure",code:...}` |
| Host → plugin | `subscribe` | `source`, `parameters` |
| Plugin → host | `visual` | `state`: `{availability:"current" or "stale",content:...}` or `{availability:"unavailable"}`; correlation of subscribe |
| Host → plugin | `unsubscribe` | `subscription`: correlation of subscribe |

Failure codes: `invalid_parameters`, `unavailable`, `failed`, `timed_out`, `busy`,
`incompatible`, `credentials_unavailable`. Options have at most 64 unique values;
values/labels are nonempty, at most 256 UTF-8 bytes, with no control characters.
An available empty list differs from unavailable. A saved missing value is not
silently replaced. Queries do not invoke actions. The host allows 2 outstanding
queries per plugin, including expired unanswered ones, with a 2000 ms timeout;
late replies must retain their original correlation.

Visual content contains text, RGBA color, and/or one lowercase-hex PNG/JPEG image
(`png_hex` or API-2 `jpeg_hex`, never both), at most 16384 hex characters (8 KiB).
The host validates/normalizes images independently. The SDK repeats current visual
state every 400 ms by default; the host marks it stale after 1000 ms without an
update. Dynamic choices and JPEG need negotiated API 2; the envelope protocol and
manifest contract remain version 1. No credentials or network administration are
part of these plugins. See the SDK source for validation and resource constants.

## Licenses and distribution

Project code is [MIT licensed](LICENSE). This does **not** relicense dependencies;
[third-party notices](notices/README.md) preserve their original terms and texts,
including conservatively retained build/target-specific and Rust library notices.
The source tree includes those copies. Both plugin executables embed all retained
texts and print them with `--licenses` as their first argument, then exit without
reading host input or connecting to audio/D-Bus. The notices therefore travel inside
the executable even in the two-blob RADP container. Output failure returns a nonzero
exit code. Keep the include list in `sdk/src/licenses.rs` synchronized when dependencies
or retained notices change, and verify it against original dependency/toolchain files.

GLib/GObject/GIO and the C runtime are dynamically linked system dependencies, not
bundled copies. `pactl` is a separately installed program, not bundled. Their licenses
remain applicable; consult the distribution's packages for their source and notices.
This source documentation is self-contained; using the plugin does not require access
to a private application repository. A source tree or package prepared locally is not
proof that a public repository or release has been published.
