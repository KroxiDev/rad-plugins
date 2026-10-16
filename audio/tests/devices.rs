//! Parsing of `pactl -f json` output. The documents are synthetic but have the shape the real
//! command prints (pactl 17 on PipeWire's PulseAudio server): extra fields everywhere, `mute` a
//! JSON boolean, and sources that are monitors naming the sink they follow in `monitor_source`.
use rad_plugin_audio::{Kind, Selector, Snapshot, devices};

const SINKS: &str = r#"[
  {"index": 5, "state": "SUSPENDED", "name": "output.one", "description": "Output One",
   "mute": false, "monitor_source": "output.one.monitor", "volume": {"front-left": {"value": 1}},
   "properties": {"device.description": "Output One"}, "ports": [{"name": "p"}], "formats": []},
  {"index": 6, "state": "RUNNING", "name": "output.two", "description": "Output Two",
   "mute": true, "monitor_source": "output.two.monitor"}
]"#;

const SOURCES: &str = r#"[
  {"index": 1, "name": "output.one.monitor", "description": "Monitor of Output One", "mute": false,
   "monitor_source": "output.one"},
  {"index": 2, "name": "input.one", "description": "Input One", "mute": true, "monitor_source": ""},
  {"index": 3, "name": "renamed-monitor", "description": "Odd monitor", "mute": false,
   "monitor_of_sink": 6},
  {"index": 4, "name": "input.two", "description": "Input Two", "mute": false,
   "monitor_of_sink": null, "monitor_of_sink_name": null},
  {"index": 5, "name": "output.two.monitor", "description": "Monitor of Output Two", "mute": false}
]"#;

#[test]
fn sinks_keep_their_names_descriptions_and_mute_state() {
    let sinks = devices::parse_devices(SINKS.as_bytes(), Kind::Output).unwrap();
    let summary: Vec<_> = sinks
        .iter()
        .map(|d| {
            (
                d.name.as_str(),
                d.description.as_str(),
                d.muted,
                d.is_monitor,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("output.one", "Output One", false, false),
            ("output.two", "Output Two", true, false)
        ]
    );
}

#[test]
fn monitors_are_recognised_however_the_client_marks_them() {
    let sources = devices::parse_devices(SOURCES.as_bytes(), Kind::Microphone).unwrap();
    let monitors: Vec<_> = sources
        .iter()
        .map(|d| (d.name.as_str(), d.is_monitor))
        .collect();
    assert_eq!(
        monitors,
        [
            ("output.one.monitor", true), // names its sink in monitor_source (and ends in .monitor)
            ("input.one", false),         // empty monitor_source
            ("renamed-monitor", true),    // only monitor_of_sink says so
            ("input.two", false),         // null markers
            ("output.two.monitor", true), // only the name says so
        ]
    );
    let snapshot = Snapshot {
        sources,
        ..Snapshot::default()
    };
    let offered: Vec<_> = snapshot
        .devices(Kind::Microphone)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(offered, ["input.one", "input.two"]);
}

#[test]
fn the_defaults_come_from_info_and_an_empty_name_is_no_default() {
    let info = br#"{"server_name": "x", "default_sink_name": "output.two", "default_source_name": "input.one", "cookie": "c"}"#;
    assert_eq!(
        devices::parse_defaults(info).unwrap(),
        (Some("output.two".into()), Some("input.one".into()))
    );
    let none = br#"{"default_sink_name": "", "default_source_name": ""}"#;
    assert_eq!(devices::parse_defaults(none).unwrap(), (None, None));
}

#[test]
fn text_that_is_not_the_expected_json_is_an_error() {
    for bad in [
        "",
        "not json",
        "{}",
        r#"[{"description": "no name"}]"#,
        r#"[{"name": 3}]"#,
    ] {
        assert!(
            devices::parse_devices(bad.as_bytes(), Kind::Output).is_err(),
            "{bad:?}"
        );
    }
    assert!(devices::parse_defaults(b"[]").is_err());
}

#[test]
fn a_selector_is_the_default_alias_a_name_or_nothing() {
    assert_eq!(Selector::from_value("@default"), Some(Selector::Default));
    assert_eq!(
        Selector::from_value("output.one"),
        Some(Selector::Named("output.one".into()))
    );
    assert_eq!(Selector::from_value(""), None);
}

#[test]
fn the_default_is_resolved_among_the_devices_a_definition_can_pick() {
    let mut snapshot = Snapshot {
        sources: devices::parse_devices(SOURCES.as_bytes(), Kind::Microphone).unwrap(),
        default_source: Some("input.one".into()),
        ..Snapshot::default()
    };
    assert_eq!(
        snapshot
            .resolve(Kind::Microphone, &Selector::Default)
            .map(|d| d.muted),
        Some(true)
    );
    // A default that is a monitor is not a microphone.
    snapshot.default_source = Some("output.one.monitor".into());
    assert!(
        snapshot
            .resolve(Kind::Microphone, &Selector::Default)
            .is_none()
    );
    assert!(
        snapshot
            .resolve(Kind::Microphone, &Selector::Named("renamed-monitor".into()))
            .is_none()
    );
    assert!(
        snapshot
            .resolve(Kind::Microphone, &Selector::Named("input.two".into()))
            .is_some()
    );
}
