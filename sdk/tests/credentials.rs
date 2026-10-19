mod support;
use rad_plugin_sdk::*;
use serde_json::json;
use support::*;
struct Probe;
impl Plugin for Probe {
    fn invoke(&self, _: Invoke, r: Reply<Outcome>) {
        r.send(Outcome::Success);
    }
    fn options(&self, _: OptionsQuery, r: Reply<ChoiceList>) {
        r.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, _: Subscription) {}
    fn unsubscribe(&self, _: SubscriptionId) {}
}
#[test]
fn installation_is_acknowledged_without_authenticating_external_services() {
    let mut host = Host::start(
        config(3..=3).credential_slots(vec!["password".into()]),
        Probe,
    );
    host.handshake(3);
    host.send(
        2,
        json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":"CANARY"}]}),
    );
    let reply = host.recv();
    assert_eq!(
        reply["message"],
        json!({"kind":"credentials_applied","revision":1})
    );
    assert_eq!(correlation(&reply), (GENERATION, 2));
    host.finish().unwrap();
}

use std::sync::mpsc::{Sender, channel};
struct Deferred {
    calls: Sender<(Invoke, Reply<Outcome>)>,
    released: Sender<u64>,
}
impl Plugin for Deferred {
    fn invoke(&self, c: Invoke, r: Reply<Outcome>) {
        self.calls.send((c, r)).unwrap();
    }
    fn options(&self, _: OptionsQuery, r: Reply<ChoiceList>) {
        r.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, _: Subscription) {}
    fn unsubscribe(&self, _: SubscriptionId) {}
    fn credentials_released(&self, revision: u64) {
        self.released.send(revision).unwrap();
    }
}
fn install(host: &mut Host, request: u64, revision: u64, value: Option<&str>) {
    let values: Vec<_> = value
        .into_iter()
        .map(|value| json!({"slot":"password","value":value}))
        .collect();
    host.send(
        request,
        json!({"kind":"credentials","revision":revision,"values":values}),
    );
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"credentials_applied","revision":revision})
    );
}
fn invoke_revision(host: &mut Host, request: u64, revision: u64) {
    host.send(request,json!({"kind":"invoke","action":"run","parameters":parameters(json!({})),"credential_revision":revision}));
}
#[test]
fn deferred_operations_keep_immutable_snapshots_and_release_runs_cleanup() {
    let (tx, rx) = channel();
    let (rtx, rrx) = channel();
    let mut host = Host::start(
        config(3..=3).credential_slots(vec!["password".into()]),
        Deferred {
            calls: tx,
            released: rtx,
        },
    );
    host.handshake(3);
    install(&mut host, 2, 1, Some("old"));
    invoke_revision(&mut host, 3, 1);
    let (old, reply) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    install(&mut host, 4, 2, None);
    invoke_revision(&mut host, 5, 2);
    let (new, new_reply) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(
        old.credentials.as_ref().unwrap().get("password"),
        Some("old")
    );
    assert_eq!(new.credentials.as_ref().unwrap().get("password"), None);
    assert!(!format!("{old:?}").contains("old"));
    drop(old);
    reply.send(Outcome::Success);
    host.recv();
    host.send(6, json!({"kind":"credentials_release","revision":1}));
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"credentials_released","revision":1})
    );
    assert_eq!(
        rrx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        1
    );
    drop(new);
    new_reply.send(Outcome::Success);
    host.recv();
    host.finish().unwrap();
}
#[test]
fn release_is_rejected_while_an_operation_handle_is_alive() {
    let (tx, rx) = channel();
    let (rtx, _) = channel();
    let mut host = Host::start(
        config(3..=3).credential_slots(vec!["password".into()]),
        Deferred {
            calls: tx,
            released: rtx,
        },
    );
    host.handshake(3);
    install(&mut host, 2, 1, Some("secret"));
    invoke_revision(&mut host, 3, 1);
    let held = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    host.send(4, json!({"kind":"credentials_release","revision":1}));
    assert!(host.outcome().is_err());
    drop(held);
}
#[test]
fn unknown_wrong_direction_duplicate_and_oversized_credentials_fail_closed() {
    for message in [
        json!({"kind":"credentials","revision":1,"values":[{"slot":"other","value":"CANARY"}]}),
        json!({"kind":"credentials","revision":0,"values":[]}),
        json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":"x".repeat(4097)}]}),
        json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":""},{"slot":"password","value":""}]}),
        json!({"kind":"credentials","revision":1,"values":[],"CANARY":"CANARY"}),
        json!({"kind":"credentials_applied","revision":1}),
        json!({"kind":"invoke","action":"run","parameters":parameters(json!({}))}),
        json!({"kind":"invoke","action":"run","parameters":parameters(json!({})),"credential_revision":1}),
    ] {
        let mut host = Host::start(
            config(3..=3).credential_slots(vec!["password".into()]),
            Probe,
        );
        host.handshake(3);
        host.send(2, message);
        let error = host.outcome().unwrap_err();
        assert!(!format!("{error:?} {error}").contains("CANARY"));
    }
}
#[test]
fn revision_storage_is_bounded_and_released_maps_do_not_accumulate() {
    let mut host = Host::start(
        config(3..=3).credential_slots(vec!["password".into()]),
        Probe,
    );
    host.handshake(3);
    let mut request = 2;
    for revision in 1..=200 {
        install(&mut host, request, revision, Some("canary"));
        request += 1;
        host.send(
            request,
            json!({"kind":"credentials_release","revision":revision}),
        );
        request += 1;
        assert_eq!(host.recv()["message"]["kind"], "credentials_released");
    }
    for revision in 201..=267 {
        install(&mut host, request, revision, None);
        request += 1;
    }
    host.send(
        request,
        json!({"kind":"credentials","revision":268,"values":[]}),
    );
    assert!(host.outcome().is_err());
}
#[test]
fn parser_rejects_oversized_decoded_strings_and_duplicate_parameter_keys() {
    for raw in [
        serde_json::to_vec(&json!({"protocol":1,"correlation":{"generation":GENERATION,"request":2},"message":{"kind":"invoke","action":"run","parameters":{"schema_version":1,"values":{"x":"a".repeat(16385)}}}})).unwrap(),
        format!(r#"{{"protocol":1,"correlation":{{"generation":{GENERATION},"request":2}},"message":{{"kind":"invoke","action":"run","parameters":{{"schema_version":1,"values":{{"x":"a","x":"b"}}}}}}}}"#).into_bytes(),
    ] {
        let mut host=Host::start(config(3..=3),Probe);
        host.handshake(3);host.send_raw(&raw);
        assert!(host.finish().is_err());
    }
}

struct VisualOptions {
    options: Sender<(OptionsQuery, Reply<ChoiceList>)>,
    visuals: Sender<Subscription>,
}
impl Plugin for VisualOptions {
    fn invoke(&self, _: Invoke, reply: Reply<Outcome>) {
        reply.send(Outcome::Success);
    }
    fn options(&self, query: OptionsQuery, reply: Reply<ChoiceList>) {
        self.options.send((query, reply)).unwrap();
    }
    fn subscribe(&self, subscription: Subscription) {
        self.visuals.send(subscription).unwrap();
    }
    fn unsubscribe(&self, _: SubscriptionId) {}
}
#[test]
fn options_and_visuals_pin_their_own_revision_with_api_three_operation_caps() {
    let (otx, orx) = channel();
    let (vtx, vrx) = channel();
    let mut host = Host::start(
        config(3..=3).credential_slots(vec!["password".into()]),
        VisualOptions {
            options: otx,
            visuals: vtx,
        },
    );
    host.handshake(3);
    install(&mut host, 2, 1, Some("first"));
    let mut options = Vec::new();
    for request in 3..=4 {
        host.send(request,json!({"kind":"options","scope":"action","definition":"run","field":"scene","context":parameters(json!({})),"credential_revision":1}));
        options.push(orx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
    }
    host.send(5,json!({"kind":"options","scope":"action","definition":"run","field":"scene","context":parameters(json!({})),"credential_revision":1}));
    assert_eq!(
        host.recv()["message"]["list"]["availability"],
        "unavailable"
    );
    assert!(orx.try_recv().is_err());
    let mut visuals = Vec::new();
    for request in 6..38 {
        host.send(request,json!({"kind":"subscribe","source":"status","parameters":parameters(json!({})),"credential_revision":1}));
        visuals.push(vrx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
    }
    host.send(38,json!({"kind":"subscribe","source":"status","parameters":parameters(json!({})),"credential_revision":1}));
    assert_eq!(
        host.recv()["message"]["state"]["availability"],
        "unavailable"
    );
    assert!(vrx.try_recv().is_err());
    install(&mut host, 39, 2, Some("second"));
    for (query, reply) in options {
        assert_eq!(
            query.credentials.as_ref().unwrap().get("password"),
            Some("first")
        );
        drop(query);
        reply.send(ChoiceList::Unavailable);
        host.recv();
    }
    for (i, visual) in visuals.into_iter().enumerate() {
        assert_eq!(visual.credentials().unwrap().get("password"), Some("first"));
        host.send(40+i as u64,json!({"kind":"unsubscribe","subscription":{"generation":GENERATION,"request":visual.id().request}}));
        drop(visual);
    }
    host.send(72, json!({"kind":"credentials_release","revision":1}));
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"credentials_released","revision":1})
    );
    host.finish().unwrap();
}
