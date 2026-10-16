//! The `control` action: which method reaches which player, and what is reported when it cannot.
mod support;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::*;

const BRAVE: &str = "org.mpris.MediaPlayer2.brave.instance4242";
const STRAWBERRY: &str = "org.mpris.MediaPlayer2.strawberry";

fn success() -> Value {
    json!({"status":"success"})
}
fn failure(code: &str) -> Value {
    json!({"status":"failure","code":code})
}

#[test]
fn each_operation_calls_its_mpris_method_on_the_chosen_player() {
    let bus = PrivateBus::start();
    let mut brave = FakePlayer::start(&bus, BRAVE);
    let mut strawberry = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    for (operation, method) in [
        ("play_pause", "PlayPause"),
        ("play", "Play"),
        ("pause", "Pause"),
        ("stop", "Stop"),
        ("previous", "Previous"),
        ("next", "Next"),
    ] {
        assert_eq!(
            host.invoke(operation, "strawberry"),
            success(),
            "{operation}"
        );
        assert_eq!(strawberry.calls().last().map(String::as_str), Some(method));
    }
    assert_eq!(
        strawberry.calls(),
        ["PlayPause", "Play", "Pause", "Stop", "Previous", "Next"]
    );
    assert_eq!(
        brave.calls(),
        Vec::<String>::new(),
        "the other player is left alone"
    );
    // And the other way round: the choice, not the order of starting, picks the player.
    assert_eq!(host.invoke("next", "brave"), success());
    assert_eq!(brave.calls(), ["Next"]);
    assert_eq!(strawberry.calls().len(), 6);
    host.finish().unwrap();
}

#[test]
fn an_application_that_is_not_running_is_unavailable_and_no_other_player_is_used() {
    // The player that was chosen is missing: the one that is running is not used instead.
    let bus = PrivateBus::start();
    let mut strawberry = FakePlayer::start(&bus, STRAWBERRY);
    let mut lookalike = FakePlayer::start_owning(
        &bus,
        &[
            "org.mpris.MediaPlayer2.bravest",
            "org.mpris.MediaPlayer2.brave2",
            "org.mpris.MediaPlayer2.Brave",
        ],
    );
    let mut host = Host::start(Some(bus.address()), quick());
    for operation in ["play_pause", "play", "pause", "stop", "previous", "next"] {
        assert_eq!(
            host.invoke(operation, "brave"),
            failure("unavailable"),
            "{operation}"
        );
    }
    assert_eq!(strawberry.calls(), Vec::<String>::new());
    assert_eq!(lookalike.calls(), Vec::<String>::new());
    host.finish().unwrap();
}

#[test]
fn with_several_instances_only_the_first_one_found_is_called() {
    // Two matching players: no error and no choice to make; the first by name is used.
    let bus = PrivateBus::start();
    let mut second = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave.instance9");
    let mut first = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave.instance10");
    let mut host = Host::start(Some(bus.address()), quick());
    assert_eq!(host.invoke("next", "brave"), success());
    assert_eq!(host.invoke("play_pause", "brave"), success());
    assert_eq!(first.calls(), ["Next", "PlayPause"]);
    assert_eq!(second.calls(), Vec::<String>::new());

    // Once the first one is gone, the next instance is the one found.
    first.quit();
    assert_eq!(host.invoke("stop", "brave"), success());
    assert_eq!(second.calls(), ["Stop"]);
    host.finish().unwrap();
}

#[test]
fn a_player_that_cannot_do_it_is_incompatible_and_is_not_asked_to() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    // (operation, the ability it needs, the method it would call)
    let table = [
        ("play", "play", "Play"),
        ("pause", "pause", "Pause"),
        ("play_pause", "pause", "PlayPause"),
        ("next", "next", "Next"),
        ("previous", "previous", "Previous"),
        ("stop", "control", "Stop"),
    ];
    for (operation, ability, _) in table {
        player.command(&format!("can {ability} false")).sync();
        let calls_before = player.calls().len();
        assert_eq!(
            host.invoke(operation, "strawberry"),
            failure("incompatible"),
            "{operation} without {ability}"
        );
        assert_eq!(player.calls().len(), calls_before, "{operation} was called");
        // Only its own ability matters: every other operation still works.
        for (other, other_ability, other_method) in table {
            if other_ability != ability {
                assert_eq!(host.invoke(other, "strawberry"), success(), "{other}");
                assert_eq!(
                    player.calls().last().map(String::as_str),
                    Some(other_method)
                );
            }
        }
        player.command(&format!("can {ability} true")).sync();
        assert_eq!(
            host.invoke(operation, "strawberry"),
            success(),
            "{operation} again"
        );
    }
    host.finish().unwrap();
}

#[test]
fn a_player_that_never_answers_times_out_and_does_not_hold_up_the_others() {
    let bus = PrivateBus::start();
    let mut stuck = FakePlayer::start(&bus, BRAVE);
    let mut fine = FakePlayer::start(&bus, STRAWBERRY);
    stuck.command("hang").sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let started = Instant::now();
    let slow = host.ask_invoke("play", "brave");
    // Meanwhile another action goes through at once.
    let quick_start = Instant::now();
    assert_eq!(host.invoke("play", "strawberry"), success());
    assert!(
        quick_start.elapsed() < Duration::from_millis(400),
        "{:?}",
        quick_start.elapsed()
    );
    assert_eq!(fine.calls(), ["Play"]);

    let outcome = host
        .outcome_of(slow, Duration::from_secs(3))
        .expect("an outcome");
    assert_eq!(outcome, failure("timed_out"));
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(800), "gave up after {took:?}");
    assert!(
        took < Duration::from_millis(2000),
        "{took:?}, the host gives up at 5 s"
    );
    assert_eq!(stuck.calls(), ["Play"], "the call did reach it");
    host.finish().unwrap();
}

#[test]
fn a_frozen_player_times_out_whatever_is_asked_of_it() {
    let bus = PrivateBus::start();
    let frozen = FakePlayer::start(&bus, BRAVE);
    frozen.freeze();
    let mut host = Host::start(Some(bus.address()), quick());
    let started = Instant::now();
    assert_eq!(host.invoke("next", "brave"), failure("timed_out"));
    assert!(
        started.elapsed() < Duration::from_millis(2000),
        "{:?}",
        started.elapsed()
    );
    frozen.thaw();
    host.finish().unwrap();
}

#[test]
fn a_player_that_leaves_during_the_call_is_unavailable() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    // From now on it takes method calls and never answers them.
    player.command("hang").sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let request = host.ask_invoke("play", "strawberry");
    // Wait until the call is with the player, so that it is the one the player leaves unanswered.
    let started = Instant::now();
    while player.calls().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "no call arrived"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    player.quit();
    let outcome = host.outcome_of(request, Duration::from_secs(3));
    assert_eq!(outcome, Some(failure("unavailable")));
    host.finish().unwrap();
}

#[test]
fn parameters_that_are_not_a_known_operation_and_a_player_are_invalid() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    let cases = [
        (
            "control",
            json!({"operation":"rewind","player":"strawberry"}),
        ),
        ("control", json!({"operation":"PLAY","player":"strawberry"})),
        ("control", json!({"player":"strawberry"})),
        ("control", json!({"operation":"play"})),
        ("control", json!({"operation":"play","player":""})),
        ("control", json!({"operation":"play","player":7})),
        (
            "control",
            json!({"operation":"play","player":"straw\nberry"}),
        ),
        ("control", json!({"operation":5,"player":"strawberry"})),
        ("other", json!({"operation":"play","player":"strawberry"})),
    ];
    for (action, values) in cases {
        let request = host.ask_invoke_values(action, values.clone());
        let outcome = host.outcome_of(request, Duration::from_secs(5)).unwrap();
        assert_eq!(outcome, failure("invalid_parameters"), "{action} {values}");
    }
    assert_eq!(player.calls(), Vec::<String>::new());
    host.finish().unwrap();
}

#[test]
fn without_a_bus_the_action_is_unavailable() {
    let mut dead = PrivateBus::start();
    let address = dead.address().to_owned();
    dead.kill();
    for address in [Some(address.as_str()), None] {
        let mut host = Host::start(address, quick());
        assert_eq!(
            host.invoke("play", "brave"),
            failure("unavailable"),
            "{address:?}"
        );
        host.finish().unwrap();
    }
}

#[test]
fn losing_the_bus_is_unavailable_and_a_new_bus_at_the_same_address_is_used_again() {
    let mut bus = PrivateBus::start_at_fixed_address();
    let mut host = Host::start(Some(bus.address()), quick());
    let player = FakePlayer::start(&bus, STRAWBERRY);
    assert_eq!(host.invoke("play", "strawberry"), success());
    drop(player);
    bus.kill();
    assert_eq!(host.invoke("play", "strawberry"), failure("unavailable"));

    bus.restart();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    assert_eq!(host.invoke("pause", "strawberry"), success());
    assert_eq!(player.calls(), ["Pause"]);
    host.finish().unwrap();
}
