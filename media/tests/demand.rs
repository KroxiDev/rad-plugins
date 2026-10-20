//! Resource lifetime observed on an isolated bus, not through implementation counters.
mod support;
use std::time::{Duration, Instant};
use support::*;

fn clients(bus: &PrivateBus, expected: usize) {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let actual = bus.client_count();
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < end,
            "expected {expected} clients, saw {actual}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn readers_share_the_bus_and_last_consumer_releases_it() {
    let bus = PrivateBus::start();
    let _player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave");
    let mut host = Host::start(Some(bus.address()), quick());
    clients(&bus, 1); // Construction does not connect.
    let first = host.subscribe("brave");
    let second = host.subscribe("brave");
    host.wait_visual(
        first,
        |v| v.availability == "current",
        Duration::from_secs(3),
    )
    .unwrap();
    host.wait_visual(
        second,
        |v| v.availability == "current",
        Duration::from_secs(3),
    )
    .unwrap();
    clients(&bus, 2); // One player and the shared monitor; the reads have finished.
    host.unsubscribe(first);
    assert_eq!(host.invoke("play", "brave")["status"], "success");
    assert!(!host.choices("action", "control").is_empty());
    clients(&bus, 2);
    host.unsubscribe(second);
    clients(&bus, 1);
    assert_eq!(host.invoke("pause", "brave")["status"], "success");
    clients(&bus, 1);
    assert!(!host.choices("action", "control").is_empty());
    clients(&bus, 1);
    host.finish().unwrap();
}

#[test]
fn returning_during_linger_does_not_republish_an_old_current_state() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave");
    player.command("status Playing").sync();
    let mut timing = quick();
    timing.linger = Duration::from_secs(3);
    let mut host = Host::start(Some(bus.address()), timing);
    let first = host.subscribe("brave");
    host.wait_state(first, "current", Some("Playing"), Duration::from_secs(3))
        .unwrap();
    // Let the initial monitor resync finish; the fake then cannot answer a fresh read.
    std::thread::sleep(Duration::from_millis(300));
    player.freeze();
    host.unsubscribe(first);
    let second = host.subscribe("brave");
    let visual = host
        .next_visual(second, Duration::from_secs(3))
        .expect("fresh unavailable state");
    assert_eq!(
        visual.availability, "unavailable",
        "old state escaped: {visual:?}"
    );
    player.thaw();
    host.unsubscribe(second);
    host.finish().unwrap();
}

#[test]
fn an_old_in_flight_read_cannot_publish_into_a_returned_subscription() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave");
    player.command("status Playing").sync();
    let mut timing = quick();
    timing.linger = Duration::from_secs(3);
    timing.refresh_deadline = Duration::from_millis(1200);
    timing.recheck = Duration::from_millis(50);
    let mut host = Host::start(Some(bus.address()), timing);
    let first = host.subscribe("brave");
    host.wait_state(first, "current", Some("Playing"), Duration::from_secs(3))
        .unwrap();
    player.freeze();
    // The periodic reader captures the old shown state and waits for the frozen fake.
    std::thread::sleep(Duration::from_millis(250));
    host.unsubscribe(first);
    let second = host.subscribe("brave");
    let visual = host
        .next_visual(second, Duration::from_secs(4))
        .expect("fresh unavailable state");
    assert_eq!(
        visual.availability, "unavailable",
        "old result escaped: {visual:?}"
    );
    assert_eq!(visual.text, None);
    player.thaw();
    host.unsubscribe(second);
    host.finish().unwrap();
}

#[test]
fn a_timed_out_action_does_not_close_a_concurrent_query_and_repeated_timeouts_release() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave");
    let mut timing = quick();
    timing.action_deadline = Duration::from_millis(250);
    timing.options_deadline = Duration::from_secs(2);
    timing.label_deadline = Duration::from_secs(2);
    player.command("identity Concurrent Player").sync();
    let mut host = Host::start(Some(bus.address()), timing);
    for _ in 0..4 {
        player.freeze();
        let action = host.ask_invoke("play", "brave");
        let query = host.ask_options("action", "control", "player");
        clients(&bus, 2); // Both calls share the same connection.
        let outcome = host.outcome_of(action, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome["code"], "timed_out");
        clients(&bus, 2); // The longer query still owns it.
        player.thaw();
        let answer = host
            .take(
                |f| f["correlation"]["request"] == query,
                Duration::from_secs(3),
            )
            .unwrap();
        assert_eq!(answer["message"]["list"]["availability"], "available");
        assert!(
            answer["message"]["list"]["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["label"] == "Concurrent Player")
        );
        clients(&bus, 1); // Neither a timeout nor the completed query retains resources.
    }
    // Timed-out calls only read the capability: no command was replayed after thaw.
    assert!(player.calls().is_empty());
    host.finish().unwrap();
}

#[test]
fn a_failed_acquisition_can_be_followed_by_fresh_demand_without_leaking() {
    let mut bus = PrivateBus::start_at_fixed_address();
    let mut host = Host::start(Some(bus.address()), quick());
    bus.kill();
    assert_eq!(host.invoke("play", "brave")["code"], "unavailable");
    bus.restart();
    let _player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave");
    assert_eq!(host.invoke("play", "brave")["status"], "success");
    clients(&bus, 1);
    host.finish().unwrap();
}
