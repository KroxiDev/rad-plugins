//! The `device` options of both definitions, read from the sound server as it is right now.
mod support;
use rad_plugin_audio::{AudioPlugin, Kind};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::*;

fn values(list: &Value) -> Vec<(String, String)> {
    list["options"]
        .as_array()
        .unwrap_or_else(|| panic!("not an available list: {list}"))
        .iter()
        .map(|o| {
            (
                o["value"].as_str().unwrap().to_owned(),
                o["label"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
fn pair(value: &str, label: &str) -> (String, String) {
    (value.to_owned(), label.to_owned())
}
fn unavailable() -> Value {
    json!({"availability":"unavailable"})
}

#[test]
fn output_offers_the_default_first_and_then_every_sink() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    for scope in ["action", "visual"] {
        let list = host.options(scope, "output", "device");
        assert_eq!(list["availability"], "available");
        assert_eq!(
            values(&list),
            [
                pair("@default", "Default device (follows changes)"),
                pair("sink-a", "Speakers"),
                pair("sink-b", "Headphones"),
            ],
            "scope {scope}"
        );
    }
    host.finish().unwrap();
}

#[test]
fn microphone_offers_sources_without_the_monitors_of_the_sinks() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let list = host.options("action", "microphone", "device");
    assert_eq!(
        values(&list),
        [
            pair("@default", "Default device (follows changes)"),
            pair("mic-a", "Desk microphone"),
            pair("mic-b", "Webcam microphone"),
        ]
    );
    // The fake lists a monitor per sink next to the real sources, as a real server does.
    assert!(server.calls().iter().any(|c| c.ends_with("list sources")));
    host.finish().unwrap();
}

#[test]
fn the_list_follows_devices_that_come_and_go() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    server.add_device(Kind::Output, "sink-c", "HDMI");
    server.remove_device(Kind::Output, "sink-a");
    assert_eq!(
        values(&host.options("action", "output", "device")),
        [
            pair("@default", "Default device (follows changes)"),
            pair("sink-b", "Headphones"),
            pair("sink-c", "HDMI"),
        ]
    );
    // An empty server still has the default entry: a real, non-empty answer.
    server.remove_device(Kind::Output, "sink-b");
    server.remove_device(Kind::Output, "sink-c");
    assert_eq!(
        values(&host.options("action", "output", "device")),
        [pair("@default", "Default device (follows changes)")]
    );
    host.finish().unwrap();
}

#[test]
fn labels_are_sanitised_values_are_usable_and_the_list_is_bounded() {
    let server = FakeServer::new();
    let long = "é".repeat(200); // 400 bytes
    server.add_device(Kind::Output, "long", &long);
    server.add_device(Kind::Output, "control", "Line\none\u{7}\ttab");
    server.add_device(Kind::Output, "blank", "  \n ");
    server.add_device(Kind::Output, "-option-like", "Looks like a flag");
    server.add_device(Kind::Output, &"n".repeat(300), "Name too long to store");
    let mut host = Host::start(&server, quick());
    let list = host.options("action", "output", "device");
    let options = values(&list);
    let label_of = |value: &str| {
        options
            .iter()
            .find(|(v, _)| v == value)
            .map(|(_, l)| l.clone())
    };
    let long_label = label_of("long").unwrap();
    assert!(
        long_label.len() <= 256 && !long_label.is_empty(),
        "{} bytes",
        long_label.len()
    );
    assert!(
        long_label.chars().all(|c| c == 'é'),
        "cut on a character boundary"
    );
    let control = label_of("control").unwrap();
    assert!(!control.chars().any(char::is_control), "{control:?}");
    assert!(control.contains("Line") && control.contains("tab"));
    assert_eq!(
        label_of("blank").as_deref(),
        Some("blank"),
        "no usable description: the name stands in"
    );
    assert_eq!(
        label_of("-option-like"),
        None,
        "a name that could be taken for an option is not offered"
    );
    assert!(
        options.iter().all(|(v, _)| v.len() <= 256),
        "unstorable names are not offered"
    );
    // Many devices: the default plus 63 more, never beyond 64 entries.
    for n in 0..80 {
        server.add_device(Kind::Output, &format!("bulk-{n}"), &format!("Bulk {n}"));
    }
    let many = values(&host.options("action", "output", "device"));
    assert_eq!(many.len(), 64);
    assert_eq!(many[0].0, "@default");
    let mut unique: Vec<_> = many.iter().map(|(v, _)| v.clone()).collect();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 64, "values are unique");
    host.finish().unwrap();
}

#[test]
fn an_unreachable_server_makes_the_options_unavailable() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    server.down();
    assert_eq!(host.options("action", "output", "device"), unavailable());
    assert_eq!(
        host.options("visual", "microphone", "device"),
        unavailable()
    );
    server.up();
    assert_eq!(
        host.options("action", "output", "device")["availability"],
        "available"
    );
    // Output that is not a device listing is the same as no server.
    server.garbage(true);
    assert_eq!(host.options("action", "output", "device"), unavailable());
    host.finish().unwrap();
}

#[test]
fn without_pactl_installed_the_options_are_unavailable() {
    let mut host = Host::with_plugin(AudioPlugin::new(None, quick()));
    assert_eq!(host.options("action", "output", "device"), unavailable());
    host.finish().unwrap();
}

#[test]
fn a_hung_pactl_is_cut_off_before_the_host_gives_up_on_the_query() {
    let server = FakeServer::new();
    server.hang(&["-f json list sinks"]);
    let mut host = Host::start(&server, quick());
    let started = Instant::now();
    assert_eq!(host.options("action", "output", "device"), unavailable());
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(server.hangs_started(), 1, "the listing really hung");
    assert!(
        eventually(Duration::from_secs(2), || server.live_hung() == 0),
        "the hung child was killed"
    );
    host.finish().unwrap();
}

#[test]
fn unknown_definitions_and_fields_are_unavailable() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    assert_eq!(host.options("action", "speakers", "device"), unavailable());
    assert_eq!(host.options("action", "output", "operation"), unavailable());
    host.finish().unwrap();
}

#[test]
fn repeated_names_and_blank_labels_do_not_spoil_the_whole_list() {
    let server = FakeServer::new();
    server.add_device(Kind::Output, "twin", "First twin");
    server.add_device(Kind::Output, "twin", "Second twin");
    server.add_device(Kind::Output, "   ", "   ");
    server.add_device(Kind::Output, "quiet", "");
    let mut host = Host::start(&server, quick());
    let list = host.options("action", "output", "device");
    assert_eq!(list["availability"], "available", "{list}");
    let options = values(&list);
    let label_of = |value: &str| {
        let found: Vec<_> = options.iter().filter(|(v, _)| v == value).collect();
        assert!(
            found.len() <= 1,
            "{value:?} is offered {} times",
            found.len()
        );
        found.first().map(|(_, l)| l.clone())
    };
    assert_eq!(
        label_of("twin").as_deref(),
        Some("First twin"),
        "the first of a repeated name is kept"
    );
    assert_eq!(label_of("quiet").as_deref(), Some("quiet"));
    assert_eq!(
        label_of("   ").as_deref(),
        Some("   "),
        "with nothing readable, the value is the label"
    );
    assert_eq!(options[0].0, "@default");
    // The same holds for microphones.
    server.add_device(Kind::Microphone, "mic-a", "Same name again");
    let list = host.options("visual", "microphone", "device");
    assert_eq!(
        values(&list).iter().filter(|(v, _)| v == "mic-a").count(),
        1
    );
    host.finish().unwrap();
}
