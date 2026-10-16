# Audio plugin

Mutes, unmutes and toggles the **microphone** and the **audio output**, and shows whether each one
is muted. Plugin id `org.rad.audio`, version 0.1.0, x86_64 Linux, host API 2.

## Requirements

- PipeWire with `pipewire-pulse`, or PulseAudio.
- `pactl` with JSON output (`-f json`) in `/usr/bin`, `/bin` or `/usr/local/bin`. Tested with pactl 17
  on PipeWire's PulseAudio server.

The plugin talks to the sound server only by running `pactl`; it has no other dependencies. If
`pactl` is missing or the server is not running, actions fail with `unavailable` and the state shows
as unavailable until the server is back.

## Actions and states

| Action | Operations | Device |
| --- | --- | --- |
| Audio output | toggle, mute, unmute | an output (sink) |
| Microphone | toggle, mute, unmute | an input (source), monitors excluded |

| State | Shows |
| --- | --- |
| Audio output state | `On` (green) or `Muted` (red) |
| Microphone state | `On` (green) or `Muted` (red) |

The **device** is either *Default device (follows changes)* or one specific device.

- *Default* acts on whatever the default device is when the action runs, and the state follows the
  default when it changes.
- A *specific* device must exist when the action runs. If it does not, the action fails with
  `unavailable` and nothing else is touched: no other device is used in its place. Its state shows as
  unavailable and returns to `On`/`Muted` when the device reappears.

State changes made elsewhere (a volume key, a mixer) show up within a heartbeat. While a state is
displayed the plugin follows the server with one `pactl subscribe` process; with nothing displayed
it runs no background process at all.

The microphone *Default device* is looked up by name before acting. If the server's default input is
a monitor (a recording of what an output plays) rather than a microphone, the action fails with
`unavailable` and the state shows as unavailable.

When the server cannot be reached the plugin tries again after 1 s, then 2 s, 4 s and so on up to
30 s, and starts over at 1 s once it answers. A server that comes back can therefore take up to 30 s
to be noticed.

## Build and package

From the `plugins` directory:

```sh
cargo build --release -p rad-plugin-audio
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    audio/manifest.json target/release/rad-plugin-audio org.rad.audio.radp
```

## Tests

`cargo test -p rad-plugin-audio` never changes the audio of the machine. It runs the plugin against
a fake `pactl` (`examples/fake_pactl.rs`) that keeps its state in a temporary directory and can
simulate a stopped server, a hung command and garbage output. The one test that starts the real
executable points it at an empty private runtime directory, where no sound server can answer.
