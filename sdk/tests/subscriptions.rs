//! Visual subscriptions: handles, validation, the heartbeat and the subscription cap.
mod support;
use rad_plugin_sdk::{
    ChoiceList, Config, Error, Invoke, OptionsQuery, Outcome, Plugin, Reply, Subscription,
    SubscriptionId, VisualContent, VisualState,
};
use serde_json::{Value, json};
use std::{
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};
use support::*;

struct Hub {
    handles: Sender<Subscription>,
    unsubscribed: Sender<SubscriptionId>,
}
struct HubProbe {
    handles: Receiver<Subscription>,
    unsubscribed: Receiver<SubscriptionId>,
}
fn hub() -> (Hub, HubProbe) {
    let (handles_tx, handles) = channel();
    let (unsubscribed_tx, unsubscribed) = channel();
    (
        Hub {
            handles: handles_tx,
            unsubscribed: unsubscribed_tx,
        },
        HubProbe {
            handles,
            unsubscribed,
        },
    )
}
impl Plugin for Hub {
    fn invoke(&self, _: Invoke, reply: Reply<Outcome>) {
        reply.send(Outcome::Success);
    }
    fn options(&self, _: OptionsQuery, reply: Reply<ChoiceList>) {
        reply.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, subscription: Subscription) {
        self.handles.send(subscription).unwrap();
    }
    fn unsubscribe(&self, subscription: SubscriptionId) {
        self.unsubscribed.send(subscription).unwrap();
    }
}

fn subscribe(host: &mut Host, request: u64) {
    host.send(
        request,
        json!({"kind":"subscribe","source":"state","parameters":parameters(json!({"device":"d"}))}),
    );
}
fn text(text: &str) -> VisualState {
    VisualState::Current {
        content: VisualContent {
            text: Some(text.into()),
            ..Default::default()
        },
    }
}
fn shown(frame: &Value) -> Option<String> {
    frame["message"]["state"]["content"]["text"]
        .as_str()
        .map(str::to_owned)
}
fn wait() -> Duration {
    Duration::from_secs(5)
}

#[test]
fn a_subscription_handle_updates_on_the_subscribe_correlation() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    let sub = probe.handles.recv_timeout(wait()).unwrap();
    assert_eq!((sub.id().generation, sub.id().request), (GENERATION, 2));
    assert_eq!(sub.source(), "state");
    assert_eq!(sub.parameters().text("device"), Some("d"));
    assert!(sub.is_active());

    sub.update(VisualState::Current {
        content: VisualContent {
            text: Some("On".into()),
            color: Some([0, 200, 0, 255]),
            ..Default::default()
        },
    });
    let frame = host.recv();
    assert_eq!(correlation(&frame), (GENERATION, 2));
    assert_eq!(
        frame["message"],
        json!({"kind":"visual","state":{"availability":"current",
            "content":{"text":"On","png_hex":null,"color":[0,200,0,255]}}})
    );
    sub.update(VisualState::Stale {
        content: VisualContent {
            jpeg_hex: Some("ffd8ffd9".into()),
            ..Default::default()
        },
    });
    assert_eq!(
        host.recv()["message"]["state"],
        json!({"availability":"stale","content":{"text":null,"png_hex":null,"jpeg_hex":"ffd8ffd9","color":null}})
    );
    sub.update(VisualState::Unavailable);
    assert_eq!(
        host.recv()["message"]["state"],
        json!({"availability":"unavailable"})
    );
    host.finish().unwrap();
}

#[test]
fn a_visual_the_host_would_reject_is_replaced_by_unavailable() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    let sub = probe.handles.recv_timeout(wait()).unwrap();
    let invalid = [
        VisualContent::default(),
        VisualContent {
            text: Some("".into()),
            ..Default::default()
        },
        VisualContent {
            text: Some("bell\u{7}".into()),
            ..Default::default()
        },
        VisualContent {
            text: Some("x".repeat(257)),
            ..Default::default()
        },
        VisualContent {
            png_hex: Some("00".into()),
            jpeg_hex: Some("00".into()),
            ..Default::default()
        },
        VisualContent {
            png_hex: Some("0g".into()),
            ..Default::default()
        },
        VisualContent {
            png_hex: Some("ABCD".into()),
            ..Default::default()
        },
        VisualContent {
            jpeg_hex: Some("abc".into()),
            ..Default::default()
        },
        VisualContent {
            jpeg_hex: Some("ab".repeat(8193)),
            ..Default::default()
        },
    ];
    for (n, content) in invalid.into_iter().enumerate() {
        sub.update(VisualState::Current { content });
        assert_eq!(
            host.recv()["message"]["state"],
            json!({"availability":"unavailable"}),
            "case {n}"
        );
    }
    // The largest legal image and a colour-only state pass.
    sub.update(VisualState::Current {
        content: VisualContent {
            png_hex: Some("ab".repeat(8192)),
            ..Default::default()
        },
    });
    assert!(host.recv()["message"]["state"]["content"]["png_hex"].is_string());
    sub.update(VisualState::Current {
        content: VisualContent {
            color: Some([1, 2, 3, 4]),
            ..Default::default()
        },
    });
    assert_eq!(host.recv()["message"]["state"]["availability"], "current");
    host.finish().unwrap();
}

/// Largest gap between consecutive frames of one subscription, in a window of about 1.3 s.
fn largest_gap(config: Config, subscriptions: u64) -> (Duration, Vec<usize>) {
    let (plugin, probe) = hub();
    let mut host = Host::start(config, plugin);
    host.handshake(2);
    for request in 2..2 + subscriptions {
        subscribe(&mut host, request);
        probe
            .handles
            .recv_timeout(wait())
            .unwrap()
            .update(text(&format!("s{request}")));
    }
    let window = Instant::now();
    let mut last_seen: Vec<Option<Instant>> = vec![None; subscriptions as usize];
    let mut counts = vec![0usize; subscriptions as usize];
    let mut gap = Duration::ZERO;
    while window.elapsed() < Duration::from_millis(1300) {
        let Some(frame) = host.recv_within(Duration::from_millis(100)) else {
            continue;
        };
        let index = (correlation(&frame).1 - 2) as usize;
        assert_eq!(
            shown(&frame),
            Some(format!("s{}", index + 2)),
            "heartbeat repeats the last state"
        );
        let now = Instant::now();
        if let Some(previous) = last_seen[index] {
            gap = gap.max(now - previous);
        }
        last_seen[index] = Some(now);
        counts[index] += 1;
    }
    host.finish().unwrap();
    (gap, counts)
}

#[test]
fn the_heartbeat_repeats_every_subscription_well_inside_the_one_second_ttl() {
    let (gap, counts) = largest_gap(config(2..=2), 3);
    assert!(
        counts.iter().all(|&n| n >= 3),
        "frames per subscription: {counts:?}"
    );
    // Heartbeat is 400 ms by default; allow scheduling slack on top of the 500 ms bound.
    assert!(gap <= Duration::from_millis(600), "largest gap {gap:?}");
}

#[test]
fn a_heartbeat_longer_than_the_ttl_is_clamped_to_500_ms() {
    let (gap, counts) = largest_gap(config(2..=2).heartbeat(Duration::from_secs(10)), 1);
    assert!(counts[0] >= 3, "frames: {counts:?}");
    assert!(gap <= Duration::from_millis(600), "largest gap {gap:?}");
}

#[test]
fn a_subscription_that_was_never_updated_sends_nothing() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2).heartbeat(Duration::from_millis(20)), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    let _sub = probe.handles.recv_timeout(wait()).unwrap();
    assert!(host.recv_within(Duration::from_millis(200)).is_none());
    host.finish().unwrap();
}

#[test]
fn unsubscribe_stops_the_heartbeat_and_silences_the_handle() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2).heartbeat(Duration::from_millis(20)), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    subscribe(&mut host, 3);
    let first = probe.handles.recv_timeout(wait()).unwrap();
    let second = probe.handles.recv_timeout(wait()).unwrap();
    first.update(text("first"));
    second.update(text("second"));
    host.send(
        4,
        json!({"kind":"unsubscribe","subscription":{"generation":GENERATION,"request":2}}),
    );
    assert_eq!(probe.unsubscribed.recv_timeout(wait()).unwrap().request, 2);
    assert!(!first.is_active());
    assert!(second.is_active());
    // Everything written before this options answer was already in flight when the plugin
    // dropped the subscription; nothing of the first one may follow it.
    host.send(
        6,
        json!({"kind":"options","scope":"visual","definition":"d","field":"f",
               "context":parameters(json!({}))}),
    );
    while correlation(&host.recv()) != (GENERATION, 6) {}
    first.update(text("ignored"));
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut seen = 0;
    while Instant::now() < deadline {
        if let Some(frame) = host.recv_within(Duration::from_millis(50)) {
            assert_eq!(
                correlation(&frame),
                (GENERATION, 3),
                "only the live subscription repeats"
            );
            seen += 1;
        }
    }
    assert!(
        seen >= 3,
        "the live subscription kept its heartbeat ({seen})"
    );
    // Unknown subscriptions are ignored.
    host.send(
        5,
        json!({"kind":"unsubscribe","subscription":{"generation":GENERATION,"request":99}}),
    );
    assert!(
        probe
            .unsubscribed
            .recv_timeout(Duration::from_millis(150))
            .is_err()
    );
    host.finish().unwrap();
}

#[test]
fn ending_the_session_unsubscribes_what_is_left() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    subscribe(&mut host, 3);
    let handles = [
        probe.handles.recv_timeout(wait()).unwrap(),
        probe.handles.recv_timeout(wait()).unwrap(),
    ];
    host.finish().unwrap();
    let mut ended: Vec<u64> = (0..2)
        .map(|_| probe.unsubscribed.recv_timeout(wait()).unwrap().request)
        .collect();
    ended.sort_unstable();
    assert_eq!(ended, [2, 3]);
    assert!(handles.iter().all(|h| !h.is_active()));
}

#[test]
fn at_most_64_subscriptions_are_tracked_and_the_extra_one_is_answered_unavailable() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    for request in 2..66 {
        subscribe(&mut host, request);
        probe.handles.recv_timeout(wait()).unwrap();
    }
    subscribe(&mut host, 66);
    let refused = host.recv();
    assert_eq!(correlation(&refused), (GENERATION, 66));
    assert_eq!(
        refused["message"]["state"],
        json!({"availability":"unavailable"})
    );
    assert!(
        probe.handles.try_recv().is_err(),
        "the extra subscription is not handed to the plugin"
    );
    // Freeing one makes room again.
    host.send(
        67,
        json!({"kind":"unsubscribe","subscription":{"generation":GENERATION,"request":2}}),
    );
    probe.unsubscribed.recv_timeout(wait()).unwrap();
    subscribe(&mut host, 68);
    probe.handles.recv_timeout(wait()).unwrap();
    host.finish().unwrap();
}

#[test]
fn reusing_a_subscription_correlation_is_a_protocol_violation() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    probe.handles.recv_timeout(wait()).unwrap();
    subscribe(&mut host, 2);
    let result = host.outcome();
    assert!(matches!(result, Err(Error::Protocol(_))), "{result:?}");
}

/// Updates from a worker thread race with a 2 ms heartbeat; the host must never see an older
/// state after a newer one.
#[test]
fn the_heartbeat_never_overtakes_a_newer_update() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(2..=2).heartbeat(Duration::from_millis(2)), plugin);
    host.handshake(2);
    subscribe(&mut host, 2);
    let sub = probe.handles.recv_timeout(wait()).unwrap();
    let last = 300u32;
    let worker = std::thread::spawn(move || {
        for n in 0..=last {
            sub.update(text(&n.to_string()));
            std::thread::sleep(Duration::from_micros(300));
        }
    });
    let mut highest = 0u32;
    while highest < last {
        let frame = host.recv();
        let n: u32 = shown(&frame).unwrap().parse().unwrap();
        assert!(n >= highest, "state {n} arrived after {highest}");
        highest = n;
    }
    worker.join().unwrap();
    host.finish().unwrap();
}
