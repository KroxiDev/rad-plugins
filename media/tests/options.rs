//! The options of the `player` field: which applications can be picked.
mod support;
use serde_json::json;
use std::time::{Duration, Instant};
use support::*;

const BRAVE: &str = "org.mpris.MediaPlayer2.brave.instance4242";
const STRAWBERRY: &str = "org.mpris.MediaPlayer2.strawberry";

fn pair(value: &str, label: &str) -> (String, String) {
    (value.to_owned(), label.to_owned())
}

/// A fake that says it is called `identity`.
fn player(bus: &PrivateBus, bus_name: &str, identity: &str) -> FakePlayer {
    let mut player = FakePlayer::start(bus, bus_name);
    player.command(&format!("identity {identity}")).sync();
    player
}

#[test]
fn running_applications_are_offered_by_the_name_they_give_themselves() {
    let bus = PrivateBus::start();
    let _brave = player(&bus, BRAVE, "Brave");
    let _strawberry = player(&bus, STRAWBERRY, "Strawberry Music Player");
    let mut host = Host::start(Some(bus.address()), quick());
    // The same list for the action and for the visual; the value is the application, not the
    // instance, so a restarted browser is still the same choice.
    for (scope, definition) in [("action", "control"), ("visual", "now_playing")] {
        let list = host.options(scope, definition, "player");
        assert_eq!(list["availability"], "available", "{list}");
        assert_eq!(
            list["options"],
            json!([
                {"value":"brave","label":"Brave"},
                {"value":"strawberry","label":"Strawberry Music Player"},
            ]),
            "{definition}"
        );
    }
    host.finish().unwrap();
}

#[test]
fn several_instances_of_one_application_are_one_option_labelled_by_the_first() {
    let bus = PrivateBus::start();
    let _later = player(
        &bus,
        "org.mpris.MediaPlayer2.brave.instance9",
        "Brave later",
    );
    let _first = player(
        &bus,
        "org.mpris.MediaPlayer2.brave.instance10",
        "Brave first",
    );
    let mut host = Host::start(Some(bus.address()), quick());
    assert_eq!(
        host.choices("action", "control"),
        [
            pair("brave", "Brave first"),
            pair("strawberry", "Strawberry (not running)"),
        ]
    );
    host.finish().unwrap();
}

#[test]
fn the_initial_applications_can_be_chosen_while_closed() {
    let bus = PrivateBus::start();
    let mut host = Host::start(Some(bus.address()), quick());
    assert_eq!(
        host.choices("action", "control"),
        [
            pair("brave", "Brave (not running)"),
            pair("strawberry", "Strawberry (not running)"),
        ],
        "no player at all is still a real answer"
    );

    // Applications that run come first, by name; the ones that do not run follow.
    let _vlc = player(&bus, "org.mpris.MediaPlayer2.vlc", "VLC media player");
    assert_eq!(
        host.choices("visual", "now_playing"),
        [
            pair("vlc", "VLC media player"),
            pair("brave", "Brave (not running)"),
            pair("strawberry", "Strawberry (not running)"),
        ]
    );
    let _strawberry = player(&bus, STRAWBERRY, "Strawberry");
    assert_eq!(
        host.choices("visual", "now_playing"),
        [
            pair("strawberry", "Strawberry"),
            pair("vlc", "VLC media player"),
            pair("brave", "Brave (not running)"),
        ]
    );
    host.finish().unwrap();
}

#[test]
fn a_label_the_player_cannot_give_is_replaced_by_its_application_name() {
    let bus = PrivateBus::start();
    let _blank = player(&bus, "org.mpris.MediaPlayer2.blank", "");
    let _messy = player(
        &bus,
        "org.mpris.MediaPlayer2.messy",
        "  Two\twords \u{7} here  ",
    );
    let mut host = Host::start(Some(bus.address()), quick());
    let choices = host.choices("action", "control");
    assert_eq!(choices[0], pair("blank", "blank"));
    assert_eq!(
        choices[1],
        pair("messy", "Two words here"),
        "controls become spaces, runs of spaces collapse"
    );
    host.finish().unwrap();
}

#[test]
fn a_player_that_does_not_answer_is_listed_by_application_name_without_stalling_the_rest() {
    let bus = PrivateBus::start();
    let _good = player(&bus, "org.mpris.MediaPlayer2.aaa", "Good one");
    let stuck = player(&bus, "org.mpris.MediaPlayer2.bbb", "Stuck one");
    let _also_good = player(&bus, "org.mpris.MediaPlayer2.ccc", "Also good");
    stuck.freeze();
    let mut host = Host::start(Some(bus.address()), quick());
    let started = Instant::now();
    let choices = host.choices("action", "control");
    let took = started.elapsed();
    assert_eq!(
        &choices[..3],
        [
            pair("aaa", "Good one"),
            pair("bbb", "bbb"),
            pair("ccc", "Also good")
        ]
    );
    assert!(took < Duration::from_millis(900), "{took:?}");
    stuck.thaw();
    host.finish().unwrap();
}

#[test]
fn a_list_never_exceeds_the_limit_and_keeps_the_initial_applications() {
    let bus = PrivateBus::start();
    let names: Vec<String> = (0..70)
        .map(|n| format!("org.mpris.MediaPlayer2.app{n:02}"))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let _many = FakePlayer::start_owning(&bus, &names);
    let mut host = Host::start(Some(bus.address()), quick());
    let choices = host.choices("action", "control");
    assert_eq!(choices.len(), 64);
    assert_eq!(choices[0].0, "app00");
    assert_eq!(
        &choices[62..],
        [
            pair("brave", "Brave (not running)"),
            pair("strawberry", "Strawberry (not running)")
        ]
    );
    let mut values: Vec<_> = choices.iter().map(|(v, _)| v.clone()).collect();
    values.sort();
    values.dedup();
    assert_eq!(values.len(), 64, "values are unique");
    host.finish().unwrap();
}

#[test]
fn without_a_session_bus_everything_is_unavailable() {
    let mut dead = PrivateBus::start();
    let address = dead.address().to_owned();
    dead.kill();
    for address in [Some(address.as_str()), None, Some("not an address")] {
        let mut host = Host::start(address, quick());
        for (scope, definition) in [("action", "control"), ("visual", "now_playing")] {
            assert_eq!(
                host.options(scope, definition, "player"),
                json!({"availability":"unavailable"}),
                "{address:?}"
            );
        }
        host.finish().unwrap();
    }
}

#[test]
fn losing_the_bus_makes_the_options_unavailable() {
    let mut bus = PrivateBus::start();
    let mut host = Host::start(Some(bus.address()), quick());
    assert_eq!(host.choices("action", "control").len(), 2);
    bus.kill();
    assert_eq!(
        host.options("action", "control", "player"),
        json!({"availability":"unavailable"})
    );
    host.finish().unwrap();
}

#[test]
fn other_definitions_and_fields_have_no_options() {
    let bus = PrivateBus::start();
    let mut host = Host::start(Some(bus.address()), quick());
    for (definition, field) in [
        ("control", "operation"),
        ("control", "device"),
        ("unknown", "player"),
    ] {
        assert_eq!(
            host.options("action", definition, field),
            json!({"availability":"unavailable"}),
            "{definition}.{field}"
        );
    }
    host.finish().unwrap();
}
