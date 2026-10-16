# Media plugin

Controls a media player (play, pause, play/pause, stop, previous, next) and shows what it is
playing: the cover when the player offers one, otherwise its state. It talks to players through
[MPRIS](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html) on the session
D-Bus. Plugin id `org.rad.media`, version 0.1.0, x86_64 Linux, host API 2.

## Requirements

- A session bus, and players that implement MPRIS (their bus name starts with
  `org.mpris.MediaPlayer2.`). Browsers and most desktop players do. The bus is the one in
  `DBUS_SESSION_BUS_ADDRESS`; when that is not set, the standard socket `$XDG_RUNTIME_DIR/bus` if it
  exists. The plugin never starts a bus.
- GLib and GIO (`libglib-2.0`, `libgio-2.0`); the plugin uses GIO for D-Bus.

The plugin needs nothing else from its environment. If there is no session bus, actions fail with
`unavailable` and the state shows as unavailable until the bus is back.

## Actions and states

| Action | Operations | Player |
| --- | --- | --- |
| Media control | play/pause, play, pause, stop, previous, next | an application |

| State | Shows |
| --- | --- |
| Now playing | the cover; without one `Playing` (green), `Paused` (amber) or `Stopped` (grey) |

The **player** is an application, not one process of it: the application is the part of the player's
bus name after `org.mpris.MediaPlayer2.` without a trailing `.instance<number>`. Browsers add that
suffix, so `...brave.instance4242` is `brave`, and still is after the browser restarts. The choices
are the applications running now, plus Brave and Strawberry marked *(not running)* when they are
not, so a button can be set up before the player is started.

- A player that is **not running** is never replaced by another: the action fails with
  `unavailable` and nothing else is touched, and the state shows as unavailable.
- With **several instances** of one application, the first one found is used: the one whose bus
  name sorts first as text (`brave.instance10` comes before `brave.instance9`). There is no
  selector and no error for it.
- A player that says it cannot do something (`CanPlay`, `CanPause`, `CanGoNext`, `CanGoPrevious`;
  `CanControl` for stop) is not asked to: the action fails with `incompatible`.
- A player that does not answer within 3 seconds makes the action fail with `timed_out`, and so
  does a bus that accepts the connection and never speaks: the 3 seconds are for everything the
  action does, connecting included. Other actions are not held up.
- A player that quits while the action is with it makes the action fail with `unavailable`.
- Asking after a player never starts it: a player that is not running stays not running.

The state follows the player: the plugin listens to the bus (a player starting or quitting, a
player announcing a change) and reads the player again then. Besides that it reads each displayed
player once every 10 seconds, so that a player that froze and can no longer announce anything is
shown as stale instead of as what it last said. Nothing else is polled. With no state displayed the
plugin runs no listening thread at all (a second after the last one went away).

## Resources

Memory and threads grow with the number of applications displayed, not with the number of
buttons: each displayed application has one reader thread, and all of them share one bus monitor.
Measured on one machine, release build, the plugin alone, with a small cover on every player:

| Displayed | Memory (RSS) | Threads |
| --- | --- | --- |
| 3 applications | about 8 MiB | 9 |
| 8 applications | about 9 MiB | 14 |
| 64 applications (the most the host allows) | about 28 MiB | 70 |

At rest the CPU use is below measurement resolution for a few applications, and about 0.2 % of one
core with 64. Each further application costs about 0.3 MiB. Decoding a cover of 2048 x 2048 pixels
(the largest accepted) adds about 24 MiB for a few milliseconds. There are never more than 64
reader threads, and a reader never waits for a player for more than 1.5 seconds.

## Known limits

- An application is a bus name without a trailing `.instance<digits>`. A player that numbers its
  instances in another way, such as Firefox with `...firefox.instance_1_82`, is one application
  per instance.
- A cover is read when the player announces it. A player that announces a cover before it has
  finished writing the file, and does not announce anything afterwards, keeps showing the state
  until its next announcement.
- Any announcement from any player makes every displayed application be read again, so a busy
  player makes the others get read a little more often.

## Covers

The cover is the `mpris:artUrl` the player offers, when it is a `file://` URL. Nothing is downloaded:
`http(s)` and other URLs are ignored and the state shows as text. The file is read only if it is a
regular file of at most 8 MiB, and decoded only if it is a PNG or JPEG of at most 2048 x 2048
pixels (the file type is taken from its content, not its name). It is scaled to fit 96 x 96
without stretching, over a black background, and published as a JPEG of at most 8 KiB.

## Build and package

From the `plugins` directory:

```sh
cargo build --release -p rad-plugin-media
cargo run --release -p rad-plugin-sdk --bin radp-pack -- \
    media/manifest.json target/release/rad-plugin-media org.rad.media.radp
```

## Tests

`cargo test -p rad-plugin-media` never touches a real player or the session bus. It starts a private
`dbus-daemon` (which must be installed, and so must `touch`) from a minimal configuration of its
own, and fake players (`examples/fake_player.rs`) on it. The only services that bus can start are
the ones one test writes for itself, to show that the plugin never starts a player. The fake records
the calls it gets, answers the MPRIS properties, emits `PropertiesChanged`, and can be told to stop
answering or to quit at a given moment. If you run a single test target
(`cargo test --test actions`), build the helper first with `cargo build --examples`.

**Do not run `fake_player` yourself against your own environment.** It registers a fake MPRIS player
on the bus of `DBUS_SESSION_BUS_ADDRESS`, which would put a player that does not exist on your real
session bus. For that reason it refuses to start unless `FAKE_PLAYER_PRIVATE_BUS=1` is set, which the
tests do and which should only be set together with the address of a bus of your own making.
