# RAD OBS plugin

Standalone official plugin `org.rad.obs` (`rad-plugin-obs`), host API 3.
Connects only to **127.0.0.1:4455** using obs-websocket 5.x, RPC 1,
JSON/text WebSocket. Configure that port in OBS. No remote hosts, TLS,
Studio/preview control, object creation, or multiple OBS instances.

Set the optional `password` credential slot to the OBS WebSocket password.
A challenged connection without a password is unavailable; a server without
an authentication challenge does not receive the password. Credentials are
isolated by revision; saving a new password does not reauthenticate old jobs.

## Controls and visuals

| Control | Operations | Visual text |
|---|---|---|
| Program scene | Select existing scene | Program / Inactive (selected scene) |
| Filter | On / Off / Toggle | Enabled / Disabled |
| Scene item | Show / Hide | Visible / Hidden |
| Recording | Start / Stop / Pause / Resume | Recording / Paused / Stopped |
| Streaming | Start / Stop | Streaming / Stopped |
| Wait for connection | Optional `timeout_seconds`: integer 1–120, default 30 | None |

Other actions have a five-second host timeout. Visuals also observe changes
made outside RAD. Disconnected, incompatible, missing, or stale selections
are unavailable rather than guessed. An action response is confirmed by
reading actual OBS state; acceptance of a command alone is not success.
An ordinary action may make one initial connection attempt (up to two seconds
within its timeout), but fails during retry backoff and never waits for a later
attempt. Commands are never replayed after reconnect. Reconnect refreshes observations.

Dynamic choices list existing sources and filters, scenes and scene items,
including group children. Item identity includes its container and numeric ID,
not merely the source name. Stored choices also include collection and names;
renaming/replacing objects may require selecting them again. There is no
cross-collection retargeting. OBS frames/messages over 64 KiB are rejected
without truncation (a defensive compatibility limit independent of SDK framing). Lists exceeding 64 choices or identities/labels
exceeding 256 UTF-8 bytes are unavailable, never truncated.

Requests stop while OBS changes scene collection. Compatibility is checked
against the requests actually advertised by OBS; unsupported controls fail
closed. This does not claim compatibility with every OBS build or distribution.

## Isolated validation

`cargo test --offline --locked --release -p rad-plugin-obs` from the plugins
workspace runs critical contracts with fake peers. Network tests bind only
injected ephemeral loopback endpoints, never production port 4455.
The `test-support` examples `obs-fixture` and `obs-conformance-server` are
isolated test helpers, not public plugin endpoint configuration.
`obs-fixture` accepts a test port argument, or (without arguments) reads a
private regular `0600` file `$XDG_RUNTIME_DIR/rad-obs-fixture.port`. It rejects
port 4455 and never falls back to production.

Checked by hand against a local OBS Studio 32.2.2 on Linux; streaming was not
exercised. That does not certify every OBS version, build or distribution.
`--licenses` prints embedded notices.
