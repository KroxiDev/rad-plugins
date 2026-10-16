//! A bus that accepts connections and never speaks: every answer the plugin owes the host must
//! still arrive within the plugin's own deadline, and one hung connection must not hold back the
//! calls made after it.
mod support;
use rad_plugin_media::Timing;
use serde_json::json;
use std::time::{Duration, Instant};
use support::*;

/// What the plugin may take beyond its own deadline: scheduling, not waiting for the bus.
const MARGIN: Duration = Duration::from_millis(600);

fn failure(code: &str) -> serde_json::Value {
    json!({"status":"failure","code":code})
}

#[test]
fn options_from_a_bus_that_never_speaks_are_unavailable_within_their_deadline() {
    let bus = MuteBus::start();
    let timing = quick();
    let mut host = Host::start(Some(&bus.address()), timing.clone());
    let started = Instant::now();
    let list = host.options("action", "control", "player");
    assert_eq!(list, json!({"availability":"unavailable"}));
    assert!(bus.connections() >= 1, "the plugin never tried the bus");
    assert!(
        started.elapsed() < timing.options_deadline + MARGIN,
        "{:?}",
        started.elapsed()
    );
    host.finish().unwrap();
}

#[test]
fn an_action_on_a_bus_that_never_speaks_times_out_within_its_deadline() {
    let bus = MuteBus::start();
    let timing = quick();
    let mut host = Host::start(Some(&bus.address()), timing.clone());
    let started = Instant::now();
    assert_eq!(host.invoke("play", "brave"), failure("timed_out"));
    assert!(bus.connections() >= 1, "the plugin never tried the bus");
    assert!(
        started.elapsed() < timing.action_deadline + MARGIN,
        "{:?}",
        started.elapsed()
    );
    host.finish().unwrap();
}

#[test]
fn a_hung_connection_does_not_hold_back_the_calls_made_after_it() {
    let bus = MuteBus::start();
    // The options wait for the connection longer than the action may.
    let timing = Timing {
        action_deadline: Duration::from_millis(300),
        options_deadline: Duration::from_millis(1500),
        ..quick()
    };
    let mut host = Host::start(Some(&bus.address()), timing.clone());
    let options = host.ask_options("action", "control", "player");
    // Let the options query reach the bus first.
    let waiting_since = Instant::now();
    while bus.connections() == 0 {
        assert!(
            waiting_since.elapsed() < Duration::from_secs(3),
            "no connection"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    assert_eq!(host.invoke("play", "brave"), failure("timed_out"));
    assert!(
        started.elapsed() < timing.action_deadline + MARGIN,
        "the action queued behind the connection: {:?}",
        started.elapsed()
    );
    // The action waited for the connection that was being made; it did not make one of its own.
    assert_eq!(bus.connections(), 1);
    // And the options still end by their own deadline.
    let answer = host
        .take(
            |f| f["correlation"]["request"] == options,
            timing.options_deadline + MARGIN,
        )
        .expect("the options were answered");
    assert_eq!(
        answer["message"]["list"],
        json!({"availability":"unavailable"})
    );
    host.finish().unwrap();
}

#[test]
fn a_visual_on_a_bus_that_never_speaks_is_unavailable() {
    let bus = MuteBus::start();
    let mut host = Host::start(Some(&bus.address()), quick());
    let subscription = host.subscribe("brave");
    host.wait_state(subscription, "unavailable", None, Duration::from_secs(3))
        .expect("an unavailable visual");
    host.finish().unwrap();
}
