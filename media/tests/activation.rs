//! A player that is not running must never be started by asking after it. The private bus has
//! services that it can start on request; starting one leaves a marker file, so a started player
//! shows.
mod support;
use serde_json::json;
use std::time::{Duration, Instant};
use support::*;

const BRAVE: &str = "org.mpris.MediaPlayer2.brave";
/// A service that has nothing to do with the plugin: it only shows that the bus starts services.
const CANARY: &str = "org.rad.test.Canary";

/// A service started by the bus leaves its marker a moment after the bus ran it.
fn started_within(bus: &PrivateBus, name: &str, within: Duration) -> bool {
    let end = Instant::now() + within;
    while !bus.started(name) {
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

fn failure(code: &str) -> serde_json::Value {
    json!({"status":"failure","code":code})
}

#[test]
fn the_private_bus_starts_the_services_that_are_asked_for() {
    // The control of the tests below: if this bus could not start anything, they would prove
    // nothing.
    let bus = PrivateBus::start_activatable(&[BRAVE, CANARY]);
    bus.activate(CANARY);
    assert!(started_within(&bus, CANARY, Duration::from_secs(5)));
    assert!(!bus.started(BRAVE), "only the one that was asked for");
}

#[test]
fn asking_after_a_player_that_is_not_running_does_not_start_it() {
    let bus = PrivateBus::start_activatable(&[BRAVE]);
    let mut host = Host::start(Some(bus.address()), quick());

    assert_eq!(host.invoke("play", "brave"), failure("unavailable"));
    let choices = host.choices("action", "control");
    assert!(
        choices.contains(&("brave".to_owned(), "Brave (not running)".to_owned())),
        "{choices:?}"
    );
    let subscription = host.subscribe("brave");
    host.wait_state(subscription, "unavailable", None, Duration::from_secs(3))
        .expect("an unavailable visual");

    assert!(
        !started_within(&bus, BRAVE, Duration::from_millis(500)),
        "the player was started"
    );
    host.finish().unwrap();
}

#[test]
fn a_player_that_quits_between_two_calls_is_not_started_to_answer_the_second() {
    let bus = PrivateBus::start_activatable(&[BRAVE]);
    let mut player = FakePlayer::start(&bus, BRAVE);
    // It gives up its name while it answers what the action asks first, so the next call of the
    // action goes to a name that nobody owns but the bus knows how to start.
    player.command("vanish CanPlay").sync();
    let mut host = Host::start(Some(bus.address()), quick());

    assert_eq!(host.invoke("play", "brave"), failure("unavailable"));
    assert_eq!(
        player.calls(),
        Vec::<String>::new(),
        "it left before the call"
    );
    assert!(
        !started_within(&bus, BRAVE, Duration::from_millis(500)),
        "the player was started to answer"
    );
    host.finish().unwrap();
}
