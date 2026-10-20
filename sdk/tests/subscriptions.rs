//! Visual subscriptions: handles, validation, source renewal and the subscription cap.
mod support;
use rad_plugin_sdk::{
    ChoiceList, Error, Invoke, OptionsQuery, Outcome, Plugin, Reply, Subscription, SubscriptionId,
    VisualContent, VisualState,
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
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
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
        json!({"kind":"visual","valid_for_ms":25000,"state":{"availability":"current",
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
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
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
        sub.update(text("valid reset"));
        assert_eq!(shown(&host.recv()).as_deref(), Some("valid reset"));
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

#[test]
fn observing_unchanged_sources_renews_each_subscription_without_repeating_content() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
    let mut subscriptions = Vec::new();
    for request in 2..5 {
        subscribe(&mut host, request);
        let sub = probe.handles.recv_timeout(wait()).unwrap();
        sub.publish(text(&format!("s{request}")), 1000);
        let initial = host.recv();
        assert_eq!(correlation(&initial), (GENERATION, request));
        assert_eq!(shown(&initial), Some(format!("s{request}")));
        subscriptions.push(sub);
    }
    assert!(
        host.recv_within(Duration::from_millis(1100)).is_none(),
        "cached states are never renewed automatically"
    );
    for _ in 0..3 {
        for sub in &subscriptions {
            sub.publish(text(&format!("s{}", sub.id().request)), 1000);
            let renewal = host.recv();
            assert_eq!(correlation(&renewal), (GENERATION, sub.id().request));
            assert_eq!(
                renewal["message"],
                json!({"kind":"visual_renew","valid_for_ms":1000})
            );
        }
    }
    host.finish().unwrap();
}

#[test]
fn declared_validity_is_clamped_to_the_contract_bounds() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
    subscribe(&mut host, 2);
    let sub = probe.handles.recv_timeout(wait()).unwrap();
    for (requested, expected) in [
        (0, 1000),
        (999, 1000),
        (1000, 1000),
        (60000, 60000),
        (60001, 60000),
    ] {
        sub.publish(text("observed"), requested);
        assert_eq!(host.recv()["message"]["valid_for_ms"], expected);
    }
    host.finish().unwrap();
}

#[test]
fn a_subscription_that_was_never_updated_sends_nothing() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
    subscribe(&mut host, 2);
    let _sub = probe.handles.recv_timeout(wait()).unwrap();
    assert!(host.recv_within(Duration::from_millis(200)).is_none());
    host.finish().unwrap();
}

#[test]
fn unsubscribe_stops_renewals_and_silences_the_handle() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
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
    first.publish(text("first"), 1000);
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut seen = 0;
    while Instant::now() < deadline {
        second.publish(text("second"), 1000);
        if let Some(frame) = host.recv_within(Duration::from_millis(50)) {
            assert_eq!(
                correlation(&frame),
                (GENERATION, 3),
                "only the live subscription renews"
            );
            seen += 1;
        }
    }
    assert!(
        seen >= 3,
        "the live subscription kept its renewals ({seen})"
    );
    // Unknown subscriptions are ignored.
    host.send(
        7,
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
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
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
fn at_most_32_subscriptions_are_tracked_and_the_extra_one_is_answered_unavailable() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
    for request in 2..34 {
        subscribe(&mut host, request);
        probe.handles.recv_timeout(wait()).unwrap();
    }
    subscribe(&mut host, 34);
    let refused = host.recv();
    assert_eq!(correlation(&refused), (GENERATION, 34));
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
        35,
        json!({"kind":"unsubscribe","subscription":{"generation":GENERATION,"request":2}}),
    );
    probe.unsubscribed.recv_timeout(wait()).unwrap();
    subscribe(&mut host, 36);
    probe.handles.recv_timeout(wait()).unwrap();
    host.finish().unwrap();
}

#[test]
fn reusing_a_subscription_correlation_is_a_protocol_violation() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
    subscribe(&mut host, 2);
    probe.handles.recv_timeout(wait()).unwrap();
    subscribe(&mut host, 2);
    let result = host.outcome();
    assert!(matches!(result, Err(Error::Protocol(_))), "{result:?}");
}

/// Source renewals interleave with updates; the host must never see an older state.
#[test]
fn source_renewals_never_retransmit_an_older_update() {
    let (plugin, probe) = hub();
    let mut host = Host::start(config(1..=1), plugin);
    host.handshake(1);
    subscribe(&mut host, 2);
    let sub = probe.handles.recv_timeout(wait()).unwrap();
    let last = 300u32;
    let worker = std::thread::spawn(move || {
        for n in 0..=last {
            sub.update(text(&n.to_string()));
            sub.update(text(&n.to_string()));
            std::thread::sleep(Duration::from_micros(300));
        }
    });
    let mut highest = 0u32;
    while highest < last {
        let frame = host.recv();
        if frame["message"]["kind"] == "visual_renew" {
            assert!(frame["message"].get("state").is_none());
            continue;
        }
        let n: u32 = shown(&frame).unwrap().parse().unwrap();
        assert!(n >= highest, "state {n} arrived after {highest}");
        highest = n;
    }
    worker.join().unwrap();
    host.finish().unwrap();
}
