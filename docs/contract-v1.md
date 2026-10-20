# RAD plugin contract v1

Published contract major **1**, SDK and official plugins **0.2.0**. Target:
`x86_64-unknown-linux-gnu`. This document describes the first published major;
the prerelease linear host APIs 2/3 are not earlier contract majors. They require
an updated package; RAD does not silently rewrite installed executables.

The canonical executable definition is `rad-plugin-contract` in
[KroxiDev/RAD](https://github.com/KroxiDev/RAD/tree/main/crates/rad-plugin-contract).
This distribution includes that same source in [`crates/rad-plugin-contract`](../crates/rad-plugin-contract)
and reference vectors in its `vectors/` directory. The SDK reexports these types;
it does not implement a second protocol. [SOURCE.md](../SOURCE.md) records the
canonical commit and mechanical distribution changes.

## Trust and packaging

A plugin is **native code with the user's privileges**, not a sandboxed extension.
Install only trusted code. A valid package or a successful compatibility check
proves neither authorship nor absence of malicious behavior. Resource declarations
are information for the user, not OS permissions. Version 1 makes no plugin-to-host
requests; that direction's namespace is reserved.

A RADP v1 file contains exactly:

1. ASCII `RADP` (4 bytes).
2. Big-endian u32 container version `1`.
3. Big-endian u32 manifest byte length, 1–65536.
4. Big-endian u64 executable byte length, 1–64 MiB.
5. UTF-8 manifest JSON, followed by the Linux executable bytes.

No filenames, compression or trailing bytes. Container version, manifest
`contract`, envelope `protocol` and negotiated host major are separate fields.
`radp-pack` writes the container; the host additionally validates the manifest,
compatibility and executable. Fixtures must never be published as product packages.

## Manifest

The complete field/type definitions are in `crates/rad-plugin-contract/src/manifest.rs`.
A minimal action-only manifest is:

```json
{
  "contract": 1,
  "id": "org.example.hello",
  "version": {"major": 1, "minor": 0, "patch": 0},
  "host_min": 1,
  "host_max": 1,
  "targets": ["x86_64-unknown-linux-gnu"],
  "capabilities": [],
  "actions": [{
    "id": "hello", "label": "Hello", "gestures": ["press"],
    "schema": {"version": 1, "fields": []},
    "credential_slots": []
  }],
  "visuals": []
}
```

Identifiers are 1–96 ASCII lowercase letters, digits, `.`, `_` or `-`,
starting with a letter or digit.
Labels are nonempty, at most 256 UTF-8 bytes, without control characters.
There are at most 64 actions, 64 visuals, 32 fields per definition and 16 targets.
IDs must be unique within each list. At least one action or visual is required.
Unknown object members, unknown field kinds and unsupported versions are rejected.

Optional `lifecycle` is `on_demand` (default) or `always_loaded`, a developer's
static policy, not a user setting. Optional `resources` is a unique list from
`internet`, `system_programs`, `external_files`, `devices`, `desktop_services`.
Declare the resources actually used; declarations do not enforce isolation.

A schema is `{version, fields}` with positive version. Each field is
`{id, label, required, kind}`. Supported `kind` objects:

| `type` | Other fields |
|---|---|
| `boolean` | none |
| `integer` | inclusive `min`, `max` (signed integers) |
| `text` | `max_bytes` (1–4096) |
| `choice` | `options`: 1–64 unique nonempty labels |
| `dynamic_choice` | `depends_on`: at most four earlier choice/dynamic-choice field IDs |

Parameters are `{schema_version, values}`. Values are booleans, signed integers
or strings, never secrets. The document is at most 16 KiB and 32 values;
individual strings are at most 4096 bytes. Parameters must match the schema;
unknown keys, missing required values and out-of-range values fail validation.
An absent optional value is distinct from a supplied value. Saved dynamic values
are not silently replaced when a current options list no longer contains them.

Optional manifest `settings` is a schema of **non-secret plugin-wide settings**,
using the same field system and requiring the `settings` capability. Settings
belong to the deck document: they travel in copies, participate in undo, and
survive a missing plugin. They are not credentials or machine preferences.
Version 1 has no settings-specific dynamic-options scope; use static fields for
settings requiring choices.

## Handshake and capabilities

stdin/stdout carry frames: big-endian u32 length followed by 1–65536 bytes of
UTF-8 JSON. stdout is exclusively protocol; diagnostics go to stderr. A frame is:

```json
{"protocol":1,"correlation":{"generation":1,"request":1},"message":{"kind":"hello","host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.example.hello","plugin_version":{"major":1,"minor":0,"patch":0},"capabilities":[]}}
```

The host opens with `hello`, request 1. The plugin replies `ready` with the same
correlation, its matching `plugin_id`, `plugin_version`, and negotiated
`capabilities`. Both must agree on identity, target and a supported major in the
manifest's inclusive range. The host supports the current and preceding published
major; major 1 has no preceding published major yet.

Capabilities are unique identifier strings (at most 32). Unknown offered names
are harmless; traffic uses only the **known intersection** of both peers. An
additive field/message may be sent only if its capability was negotiated. Strict
parsing is not relaxed. Do not advertise a feature that is not implemented.

| Capability | Enables |
|---|---|
| `controls` | Generic control identity and press/hold/long-press events |
| `settings` | Immutable settings on Invoke, Options and Subscribe |
| `dynamic_options` | Options/Choices and dynamic-choice declarations |
| `credentials` | Immutable credential revisions and their lifecycle |
| `jpeg` | JPEG visual content |
| `response_timeout` | Per-action response budget declaration |
| `visual_freshness` | Visual subscriptions and declared validity/renewal |

A host request has a positive generation and strictly increasing positive request
ID. Replies echo their request correlation unchanged; subscription updates retain
the original Subscribe correlation. A late reply never becomes a new request's
reply. Do not reuse IDs or send traffic from another generation.

## Messages

`kind` selects the message. The shared `protocol.rs` and vectors are authoritative
for required members, enum tags and strict decoding.

| Direction | Kind | Other fields |
|---|---|---|
| Host → plugin | `hello` | `host_api`, `target`, `plugin_id`, `plugin_version`, `capabilities` |
| Plugin → host | `ready` | `plugin_id`, `plugin_version`, `capabilities` |
| Host → plugin | `invoke` | `action`, `parameters`; optional `control`, `settings`, `credential_revision` |
| Plugin → host | `result` | `outcome`: `{status:"success"}` or `{status:"failure",code:...}` |
| Host → plugin | `options` | `scope`, `definition`, `field`, `context`; optional `settings`, `credential_revision` |
| Plugin → host | `choices` | `list`: `{availability:"available",options:[{value,label}]}` or `{availability:"unavailable"}` |
| Host → plugin | `subscribe` | `source`, `parameters`; optional `settings`, `credential_revision` |
| Plugin → host | `visual` | `valid_for_ms`, `state` |
| Plugin → host | `visual_renew` | `valid_for_ms` only, no state/image payload |
| Host → plugin | `unsubscribe` | `subscription`: original Subscribe correlation |
| Host → plugin | `credentials` | `revision`, `values:[{slot,value}]` |
| Plugin → host | `credentials_applied` | `revision` |
| Host → plugin | `credentials_release` | `revision` |
| Plugin → host | `credentials_released` | `revision` |

Failure codes: `invalid_parameters`, `unavailable`, `failed`, `timed_out`, `busy`,
`incompatible`, `credentials_unavailable`. Failure is not automatic retry.
An uncertain completion is not proof that an external action did not occur.

Options have `scope: "action"` or `"visual"`, a declared definition and a dynamic
field; `context` contains only that field's declared dependencies. Lists have at
most 64 unique values with valid labels. Available-empty differs from unavailable.
Listing options must not invoke actions. The host allows two pending queries per
plugin, including expired queries not yet answered, with a 2000 ms response deadline.

## Controls, snapshots and admission

A definition's `gestures` contains `press`, `hold`, `long_press` (default only
`press`). A nondefault gesture requires `controls`. Invoke `control` is
`{control:{kind,index}, gesture}`: generic kind/index, not a device-specific key
count. Wire gestures are `press`, `hold_start`, `hold_end`, `long_press`.

The user selects one gesture for a button's action plan. Hold is available only
when all its actions support it. End is delivered only to the actions actually
started, with their original control, parameters, settings, credential revision
and process generation. It must release the original operation, never repeat
navigation or launch an application. Disconnect/layout changes also end a hold.

The Host reserves End capacity **before** dispatching Start (32 holds per plugin,
64 globally); End does not compete for the ordinary invocation quota. The SDK
also reserves 32 independent End slots. A duplicate, unmatched or mismatched End
is invalid. Start's result does not consume End's reservation. Process death
invalidates that generation; there is no replay against its replacement.

Settings and credentials are captured before work waits. Changing either affects
new work, not an already admitted invocation. Visual settings changes replace
the old binding rather than mutating an existing subscription. No settings setter
message or ACK race exists in v1.

Ordinary actions keep a 5000 ms response budget. With `response_timeout`, an action
may name an optional integer field with exact bounds 1–120 and a default in that
range: `{field,default_seconds}`. The Host captures this budget at admission and
starts it at dispatch. It is not a cancellation or retry protocol.

## Visual freshness and bounded output

`state` is `{availability:"current",content:...}`, `stale` with content, or
`{availability:"unavailable"}`. Content has optional `text`, RGBA `color`, and
one of `png_hex` / `jpeg_hex`, lowercase hex at most 16384 characters (8 KiB).
The host validates/normalizes images independently; JPEG requires its capability.

`valid_for_ms` is **1000–60000**, measured by the Host's clock. Expiry makes the
last content stale, not current. `visual_renew` is valid only after a first state.
Publish or renew only after observing the real source's health. A healthy
unchanged observation may renew without retransmitting the image. Cached state
alone, a timer or a blocked reader is not evidence of health. The SDK has **no
independent heartbeat**. Stop producing after unsubscribe/EOF and release the
last consumer's resources.

The SDK's `run_stdio` makes native stdout nonblocking, budgets a whole frame to
50 ms, and exits terminally with code **74** on output failure or expiry. Never
write another frame after a partial failed frame. Custom `run` writers must
provide their own bounded-write behavior. Concurrent frames do not interleave.
Host ordinary invocation limits are 16 per plugin/64 globally; SDK ordinary limit
is 16, queries 2 and subscriptions 32. Extra work is rejected, not queued forever.
These are bounds, not a CPU/RSS performance certification.

## Credentials and diagnostics

Secrets use declared `credential_slots`, never Parameters, settings, options,
visuals, stdout diagnostics or stderr. There are at most 32 slots, 4096 bytes per
value, 8192 bytes per map and 67 retained revisions. Positive revisions are
immutable. Empty-present differs from omitted. Applied/Released ACKs have a
2000 ms deadline. Work waits for Applied; the Host releases only after all owners,
including holds and residual work, are gone. Timeout does not prove cleanup.

SDK credential handles expose borrowed values and revision IDs. Retire external
resources for a revision synchronously in `Plugin::credentials_released` before
returning. Do not keep idle credential handles forever or log their values.

Host diagnostics are tagged by plugin, retained as at most 32 lines, 1024 bytes
per line and eight accepted lines per second, with bounded reads per turn.
Known delivered secrets are redacted; that is defense in depth, not permission to
log secrets. Runtime journald and Advanced → Log expose the bounded diagnostics.

## Compatibility check

Use `rad-plugin-check package.radp scenario.json` from the matching RAD release.
See [the author guide](author-guide.md) and
[scenario format](https://github.com/KroxiDev/RAD/blob/main/docs/plugin-check.md).
A check covers declared actions/gestures, options, settings, healthy freshness,
unsubscribe, pressure, credential reflection and EOF cleanup with controlled
services. Incomplete or unobservable coverage is **not PASS**. It does not certify
real hardware, every distribution, security, or behavior outside its scenarios.
EOF must stop the plugin's work and descendants; checker cleanup after a failure
is not evidence that the plugin honored that requirement.
