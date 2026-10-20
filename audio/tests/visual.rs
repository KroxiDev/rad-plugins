//! The visual states: they follow the sound server through one shared event monitor, which only
//! exists while something is subscribed.
mod support;
use rad_plugin_audio::{AudioPlugin, Kind};
use std::time::Duration;
use support::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}
/// The integration's latency budget for a source change (not an SDK heartbeat).
const HEARTBEAT: Duration = Duration::from_millis(500);

#[test]
fn a_subscription_shows_muted_or_on_with_a_colour() {
    let server = FakeServer::new();
    server.set_mute(Kind::Microphone, "mic-a", true);
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    let microphone = host.subscribe("microphone", "@default");
    let mut colours = Vec::new();
    for (subscription, text) in [(output, "On"), (microphone, "Muted")] {
        let frame = host
            .take(
                |f| f["correlation"]["request"] == subscription && f["message"]["kind"] == "visual",
                secs(5),
            )
            .expect("initial visual");
        let state = &frame["message"]["state"];
        assert_eq!(state["availability"], "current");
        assert_eq!(state["content"]["text"], text);
        let colour = state["content"]["color"].as_array().expect("colour");
        colours.push([0, 1, 2, 3].map(|i| colour[i].as_u64().unwrap()));
    }
    let [on, muted] = colours.as_slice() else {
        panic!("both colours")
    };
    assert!(on[1] > on[0], "on is greenish: {on:?}");
    assert!(muted[0] > muted[1], "muted is reddish: {muted:?}");
    assert_eq!((on[3], muted[3]), (255, 255));
    host.finish().unwrap();
}

#[test]
fn an_outside_change_of_mute_reaches_the_host_within_one_heartbeat() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    let microphone = host.subscribe("microphone", "mic-b");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    host.wait_visual(microphone, "current", Some("On"), secs(5))
        .unwrap();
    for (muted, text) in [(true, "Muted"), (false, "On"), (true, "Muted")] {
        host.discard_pending();
        server.set_mute(Kind::Output, "sink-a", muted);
        let took = host
            .wait_visual(output, "current", Some(text), HEARTBEAT)
            .unwrap_or_else(|seen| panic!("never {text}: {seen:?}"));
        assert!(took <= HEARTBEAT, "{took:?}");
    }
    host.discard_pending();
    server.set_mute(Kind::Microphone, "mic-b", true);
    host.wait_visual(microphone, "current", Some("Muted"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn a_chosen_device_follows_only_itself() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let chosen = host.subscribe("output", "sink-b");
    host.wait_visual(chosen, "current", Some("On"), secs(5))
        .unwrap();
    server.set_mute(Kind::Output, "sink-a", true); // the default, not the chosen one
    server.set_default(Kind::Output, "sink-b");
    let after = host.wait_visual(chosen, "current", Some("Muted"), Duration::from_millis(600));
    assert!(
        after.is_err(),
        "sink-b was never muted, so it never shows Muted"
    );
    server.set_mute(Kind::Output, "sink-b", true);
    host.wait_visual(chosen, "current", Some("Muted"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn the_default_is_resolved_again_when_it_changes() {
    let server = FakeServer::new();
    server.set_mute(Kind::Output, "sink-b", true);
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    server.set_default(Kind::Output, "sink-b");
    host.wait_visual(output, "current", Some("Muted"), HEARTBEAT)
        .unwrap();
    server.set_default(Kind::Output, "sink-a");
    host.wait_visual(output, "current", Some("On"), HEARTBEAT)
        .unwrap();
    // No default at all is not a state to show.
    server.set_default(Kind::Output, "");
    host.wait_visual(output, "unavailable", None, HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn a_device_that_disappears_is_unavailable_and_current_again_when_it_returns() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let chosen = host.subscribe("microphone", "mic-b");
    host.wait_visual(chosen, "current", Some("On"), secs(5))
        .unwrap();
    server.remove_device(Kind::Microphone, "mic-b");
    host.wait_visual(chosen, "unavailable", None, HEARTBEAT)
        .unwrap();
    server.add_device(Kind::Microphone, "mic-b", "Webcam microphone");
    server.set_mute(Kind::Microphone, "mic-b", true);
    host.wait_visual(chosen, "current", Some("Muted"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn a_server_that_goes_away_is_unavailable_and_recovery_restores_the_state() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    server.down();
    host.wait_visual(output, "unavailable", None, secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    server.set_mute(Kind::Output, "sink-a", true); // changed while nobody could see
    server.up();
    host.wait_visual(output, "current", Some("Muted"), secs(3))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    assert!(server.subscribers_started() >= 2, "the monitor reconnected");
    host.finish().unwrap();
}

#[test]
fn the_monitor_exists_only_while_something_is_subscribed() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        server.calls().is_empty(),
        "constructing an idle plugin runs no pactl"
    );
    assert_eq!(
        host.options("action", "output", "device")["availability"],
        "available"
    );
    host.invoke("output", "mute", "@default");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        server.subscribers_started(),
        0,
        "actions and options need no monitor"
    );

    // Two subscriptions share one monitor.
    let output = host.subscribe("output", "@default");
    let microphone = host.subscribe("microphone", "@default");
    host.wait_visual(output, "current", Some("Muted"), secs(5))
        .unwrap();
    host.wait_visual(microphone, "current", Some("On"), secs(5))
        .unwrap();
    assert_eq!(server.live_subscribers(), 1);
    host.unsubscribe(output);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(server.live_subscribers(), 1, "one subscription is left");
    host.unsubscribe(microphone);
    assert!(
        eventually(secs(2), || server.live_subscribers() == 0),
        "the monitor child is gone"
    );

    // Resubscribing starts a new monitor and shows the state as it is now, not as it was: two
    // subscriptions in a row, so the second arrives before the new monitor has any picture.
    server.set_mute(Kind::Output, "sink-a", false);
    host.discard_pending();
    let again = host.subscribe("output", "@default");
    let other = host.subscribe("microphone", "@default");
    assert_eq!(
        host.next_visual(again, secs(5)),
        Some(("current".into(), Some("On".into())))
    );
    assert_eq!(
        host.next_visual(other, secs(5)),
        Some(("current".into(), Some("On".into())))
    );
    assert!(eventually(secs(2), || server.live_subscribers() == 1));
    host.finish().unwrap();
    assert!(
        eventually(secs(2), || server.live_subscribers() == 0),
        "ending the session stops it too"
    );
}

#[test]
fn between_observations_there_is_no_blind_renewal_or_process() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let calls = server.calls().len();
    // A live SDK is not evidence that the source was observed again.
    assert!(
        host.take(
            |f| matches!(
                f["message"]["kind"].as_str(),
                Some("visual" | "visual_renew")
            ),
            Duration::from_millis(1400)
        )
        .is_none()
    );
    assert_eq!(
        server.calls().len(),
        calls,
        "{:?}",
        &server.calls()[calls.min(server.calls().len())..]
    );
    host.finish().unwrap();
}

#[test]
fn a_burst_of_events_becomes_a_few_refreshes_not_one_per_event() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before = server.listings();
    // 30 events over about 150 ms, spaced closer than a refresh takes: refreshing per event
    // would mean a dozen; the monitor waits for quiet, at most 80 ms in these timings.
    for n in 0..30 {
        server.emit(&format!("Event 'change' on sink #{n}"));
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(400));
    let refreshes = server.listings() - before;
    assert!(
        (1..=3).contains(&refreshes),
        "30 events caused {refreshes} refreshes"
    );
    host.finish().unwrap();
}

#[test]
fn events_that_cannot_change_a_device_are_ignored() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before = server.listings();
    for line in [
        "Event 'new' on client #5",
        "Event 'change' on sink-input #3",
        "Event 'remove' on source-output #2",
        "Event 'change' on module #1",
        "garbage that is not an event",
        "Event 'change' on sinkish #1",
    ] {
        server.emit(line);
    }
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(server.listings(), before);
    // The kinds that can are: sinks, sources and the server's defaults.
    server.emit("Event 'change' on source #4");
    assert!(eventually(secs(2), || server.listings() > before));
    host.finish().unwrap();
}

#[test]
fn a_subscription_that_cannot_work_is_unavailable_and_starts_no_monitor() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let empty = host.subscribe("output", "");
    let unknown = host.subscribe("speakers", "@default");
    assert_eq!(
        host.next_visual(empty, secs(5)).map(|v| v.0),
        Some("unavailable".into())
    );
    assert_eq!(
        host.next_visual(unknown, secs(5)).map(|v| v.0),
        Some("unavailable".into())
    );
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(server.subscribers_started(), 0);
    host.finish().unwrap();
    // Without pactl everything is unavailable, and the answer is immediate.
    let mut host = Host::with_plugin(AudioPlugin::new(None, quick()));
    let output = host.subscribe("output", "@default");
    assert_eq!(
        host.next_visual(output, secs(5)).map(|v| v.0),
        Some("unavailable".into())
    );
    host.finish().unwrap();
}

#[test]
fn a_failed_refresh_is_tried_again_without_waiting_for_another_event() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    // The event stream stays up, but the listings stop being usable: the refresh fails.
    server.garbage(true);
    server.set_mute(Kind::Output, "sink-a", true);
    host.wait_visual(output, "unavailable", None, secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    // The service is back and nothing announces it: the plugin finds it out by itself.
    server.garbage(false);
    host.wait_visual(output, "current", Some("Muted"), secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    host.finish().unwrap();
}

#[test]
fn an_event_cuts_short_the_wait_before_a_retry() {
    let server = FakeServer::new();
    let slow = quick().retry_wait(secs(5)).retry_max(secs(5));
    let mut host = Host::start(&server, slow);
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    server.garbage(true);
    server.set_mute(Kind::Output, "sink-a", true);
    host.wait_visual(output, "unavailable", None, secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    server.garbage(false);
    server.emit("Event 'change' on sink #1");
    let took = host
        .wait_visual(output, "current", Some("Muted"), secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    assert!(
        took < Duration::from_secs(1),
        "waited for the 5 s retry: {took:?}"
    );
    host.finish().unwrap();
}

#[test]
fn stopping_cuts_short_the_wait_before_a_retry() {
    let server = FakeServer::new();
    let slow = quick().retry_wait(secs(5)).retry_max(secs(5));
    let mut host = Host::start(&server, slow);
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "current", Some("On"), secs(5))
        .unwrap();
    server.garbage(true);
    server.set_mute(Kind::Output, "sink-a", true);
    host.wait_visual(output, "unavailable", None, secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    host.unsubscribe(output);
    assert!(
        eventually(secs(2), || server.live_subscribers() == 0),
        "the monitor kept waiting for its retry after being stopped"
    );
    host.finish().unwrap();
}

/// How many times the plugin tried to reach the event stream.
fn connection_attempts(server: &FakeServer) -> usize {
    server.calls_starting_with("subscribe")
}

#[test]
fn an_unreachable_server_is_retried_with_growing_waits_that_start_over_after_recovery() {
    let server = FakeServer::new();
    server.down();
    let timing = quick()
        .retry_wait(Duration::from_millis(30))
        .retry_max(Duration::from_millis(240));
    let mut host = Host::start(&server, timing);
    let output = host.subscribe("output", "@default");
    host.wait_visual(output, "unavailable", None, secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));

    // 30, 60, 120, 240, 240... ms: about eight tries in 1.5 s. A fixed 30 ms wait makes forty.
    std::thread::sleep(Duration::from_millis(1500));
    let tries = connection_attempts(&server);
    assert!((5..=12).contains(&tries), "{tries} tries in 1.5 s");

    // The longest wait is 240 ms, so recovery shows within that plus a refresh.
    server.up();
    let took = host
        .wait_visual(output, "current", Some("On"), secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    assert!(took < Duration::from_millis(900), "{took:?}");

    // Back to short waits: 30, 60, 120 ms means at least three tries in the next half second,
    // where a wait that stayed at 240 ms would allow two.
    server.down();
    host.wait_visual(output, "unavailable", None, secs(2))
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    let before = connection_attempts(&server);
    std::thread::sleep(Duration::from_millis(500));
    let after = connection_attempts(&server) - before;
    assert!(
        after >= 3,
        "only {after} tries in the half second after recovery"
    );
    host.finish().unwrap();
}

#[test]
fn watching_resumed_while_a_refresh_is_stuck_shows_only_fresh_pictures() {
    let server = FakeServer::new();
    server.hang(&["-f json list"]);
    let timing = quick().refresh_deadline(Duration::from_millis(400));
    let mut host = Host::start(&server, timing);
    let first = host.subscribe("output", "@default");
    assert!(
        eventually(secs(2), || server.hangs_started() >= 1),
        "the first refresh is stuck"
    );
    host.unsubscribe(first);
    // The listing works again, and the output was muted meanwhile.
    server.hang(&[]);
    server.set_mute(Kind::Output, "sink-a", true);
    host.discard_pending();
    let second = host.subscribe("output", "@default");
    // The abandoned refresh ends in a failure; none of that may reach the new subscription.
    assert_eq!(
        host.next_visual(second, secs(5)),
        Some(("current".into(), Some("Muted".into())))
    );
    assert!(eventually(secs(2), || server.live_subscribers() == 1));
    host.finish().unwrap();
    assert!(eventually(secs(2), || server.live_subscribers() == 0));
}

#[test]
fn adding_a_subscriber_during_a_blocked_read_does_not_refresh_cached_visuals() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick().refresh_deadline(secs(3)));
    let first = host.subscribe("output", "@default");
    host.wait_visual(first, "current", Some("On"), secs(5))
        .unwrap();
    host.discard_pending();
    server.hang(&["-f json list"]);
    server.set_mute(Kind::Output, "sink-a", true);
    assert!(eventually(secs(2), || server.live_hung() == 1));
    let calls = server.calls();
    let second = host.subscribe("output", "@default");
    let unexpected = host.take(
        |f| {
            matches!(
                f["message"]["kind"].as_str(),
                Some("visual" | "visual_renew")
            )
        },
        Duration::from_millis(300),
    );
    let calls_after = server.calls();
    // Release future reads; the current hung child still reaches its controlled deadline.
    server.hang(&[]);
    host.wait_visual(first, "current", Some("Muted"), secs(5))
        .unwrap();
    host.wait_visual(second, "current", Some("Muted"), secs(5))
        .unwrap();
    host.finish().unwrap();
    assert_eq!(
        calls_after, calls,
        "the reader stayed blocked during the add"
    );
    assert!(
        unexpected.is_none(),
        "cache asserted fresh without observation: {unexpected:?}"
    );
    assert!(eventually(secs(2), || server.live_subscribers() == 0
        && server.live_hung() == 0));
}
