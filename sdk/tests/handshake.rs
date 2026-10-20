//! Handshake and protocol discipline of the SDK loop.
mod support;
use rad_plugin_sdk::{
    ChoiceList, Error, Invoke, OptionsQuery, Outcome, Plugin, Reply, Subscription, SubscriptionId,
};
use serde_json::json;
use support::*;

struct Inert;
impl Plugin for Inert {
    fn invoke(&self, _: Invoke, reply: Reply<Outcome>) {
        reply.send(Outcome::Success);
    }
    fn options(&self, _: OptionsQuery, reply: Reply<ChoiceList>) {
        reply.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, _: Subscription) {}
    fn unsubscribe(&self, _: SubscriptionId) {}
}

#[test]
fn answers_hello_with_its_own_identity_on_the_same_correlation() {
    let mut host = Host::start(config(1..=1), Inert);
    host.hello(1);
    let ready = host.recv();
    assert_eq!(correlation(&ready), (GENERATION, 1));
    assert_eq!(ready["protocol"], 1);
    assert_eq!(
        ready["message"],
        json!({"kind":"ready","capabilities":["credentials","dynamic_options","jpeg","visual_freshness"],"plugin_id":"org.example.probe",
               "plugin_version":{"major":0,"minor":3,"patch":1}})
    );
    host.finish().unwrap();
}

#[test]
fn accepts_major_one_inside_each_declared_range() {
    for range in [1..=1, 1..=2, 1..=3] {
        let mut host = Host::start(config(range), Inert);
        host.handshake(1);
        host.finish().unwrap();
    }
}

#[test]
fn refuses_a_host_api_outside_its_range_and_never_answers() {
    for api in [2, 3, 99] {
        let mut host = Host::start(config(1..=1), Inert);
        host.hello(api);
        host.expect_closed();
        let result = host.outcome();
        assert!(
            matches!(result, Err(Error::IncompatibleHost { requested, min: 1, max: 1 }) if requested == api),
            "api {api}: {result:?}"
        );
    }
}

#[test]
fn a_host_that_closes_the_pipe_ends_the_loop_cleanly() {
    let host = Host::start(config(1..=1), Inert);
    assert!(
        host.finish().is_ok(),
        "closing before hello is not an error"
    );
    let mut host = Host::start(config(1..=1), Inert);
    host.handshake(1);
    assert!(host.finish().is_ok());
}

#[test]
fn protocol_violations_stop_the_loop_with_an_error() {
    type Violation = Box<dyn Fn(&mut Host)>;
    let violations: Vec<(&str, Violation)> = vec![
        (
            "first message is not hello",
            Box::new(|h| {
                h.send(
                    1,
                    json!({"kind":"unsubscribe","subscription":{"generation":7,"request":2}}),
                )
            }),
        ),
        ("not json", Box::new(|h| h.send_raw(b"not json"))),
        (
            "unknown kind",
            Box::new(|h| h.send(1, json!({"kind":"explode"}))),
        ),
        (
            "wrong protocol",
            Box::new(|h| {
                h.send_raw(
            br#"{"protocol":2,"correlation":{"generation":7,"request":1},"message":{"kind":"hello","capabilities":[],"host_api":1,"target":"t","plugin_id":"org.example.probe","plugin_version":{"major":0,"minor":3,"patch":1}}}"#)
            }),
        ),
        (
            "zero request",
            Box::new(|h| {
                h.send_raw(
            br#"{"protocol":1,"correlation":{"generation":7,"request":0},"message":{"kind":"hello","capabilities":[],"host_api":1,"target":"t","plugin_id":"org.example.probe","plugin_version":{"major":0,"minor":3,"patch":1}}}"#)
            }),
        ),
        (
            "oversize frame",
            Box::new(|h| {
                use std::io::Write;
                h.stream.write_all(&[0, 1, 0, 1]).unwrap();
            }),
        ),
    ];
    for (name, violation) in violations {
        let mut host = Host::start(config(1..=1), Inert);
        violation(&mut host);
        let result = host.outcome();
        assert!(result.is_err(), "{name}: {result:?}");
    }
}

#[test]
fn messages_only_a_plugin_may_send_are_violations_after_the_handshake() {
    let offenders = [
        json!({"kind":"ready","capabilities":[],"plugin_id":"x","plugin_version":{"major":1,"minor":0,"patch":0}}),
        json!({"kind":"result","outcome":{"status":"success"}}),
        json!({"kind":"visual","valid_for_ms":1000,"state":{"availability":"unavailable"}}),
        json!({"kind":"choices","list":{"availability":"unavailable"}}),
    ];
    for message in offenders {
        let mut host = Host::start(config(1..=1), Inert);
        host.handshake(1);
        host.send(2, message.clone());
        assert!(host.outcome().is_err(), "{message}");
    }
    // A second hello and a request from another generation are violations too.
    let mut host = Host::start(config(1..=1), Inert);
    host.handshake(1);
    host.hello(1);
    assert!(host.outcome().is_err(), "second hello");
    let mut host = Host::start(config(1..=1), Inert);
    host.handshake(1);
    host.send_raw(br#"{"protocol":1,"correlation":{"generation":8,"request":2},"message":{"kind":"unsubscribe","subscription":{"generation":8,"request":1}}}"#);
    assert!(host.outcome().is_err(), "other generation");
}
