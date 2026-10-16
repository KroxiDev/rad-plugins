//! Dispatch of invoke/options to the plugin, deferred replies and the concurrency caps.
mod support;
use rad_plugin_sdk::{
    ChoiceList, ChoiceOption, FailureCode, Invoke, OptionsQuery, OptionsScope, Outcome, Plugin,
    Reply, Subscription, SubscriptionId,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::Duration,
};
use support::*;

/// Hands every request and its reply handle to the test, which completes them when it wants.
struct Handoff {
    invokes: Sender<(Invoke, Reply<Outcome>)>,
    queries: Sender<(OptionsQuery, Reply<ChoiceList>)>,
}
struct Probe {
    invokes: Receiver<(Invoke, Reply<Outcome>)>,
    queries: Receiver<(OptionsQuery, Reply<ChoiceList>)>,
}
fn handoff() -> (Handoff, Probe) {
    let (invokes_tx, invokes) = channel();
    let (queries_tx, queries) = channel();
    (
        Handoff {
            invokes: invokes_tx,
            queries: queries_tx,
        },
        Probe { invokes, queries },
    )
}
impl Plugin for Handoff {
    fn invoke(&self, call: Invoke, reply: Reply<Outcome>) {
        self.invokes.send((call, reply)).unwrap();
    }
    fn options(&self, query: OptionsQuery, reply: Reply<ChoiceList>) {
        self.queries.send((query, reply)).unwrap();
    }
    fn subscribe(&self, _: Subscription) {}
    fn unsubscribe(&self, _: SubscriptionId) {}
}

fn invoke(host: &mut Host, request: u64) {
    host.send(
        request,
        json!({"kind":"invoke","action":"go","parameters":parameters(json!({"n":request}))}),
    );
}
fn query(host: &mut Host, request: u64) {
    host.send(
        request,
        json!({"kind":"options","scope":"visual","definition":"d","field":"f",
        "context":parameters(json!({"k":"v"}))}),
    );
}
fn option(n: usize) -> ChoiceOption {
    ChoiceOption {
        value: format!("value-{n}"),
        label: format!("Label {n}"),
    }
}
fn wait() -> Duration {
    Duration::from_secs(5)
}

#[test]
fn an_invoke_reaches_the_plugin_and_is_answered_later_from_another_thread() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    invoke(&mut host, 2);
    let (call, reply) = probe.invokes.recv_timeout(wait()).unwrap();
    assert_eq!(call.action, "go");
    assert_eq!(call.parameters.schema_version, 1);
    assert_eq!(call.parameters.values["n"], 2);

    // While the invoke is pending the loop keeps serving: an options query is answered first.
    query(&mut host, 3);
    let (asked, options_reply) = probe.queries.recv_timeout(wait()).unwrap();
    assert_eq!(
        (asked.scope, asked.definition.as_str(), asked.field.as_str()),
        (OptionsScope::Visual, "d", "f")
    );
    assert_eq!(asked.context.text("k"), Some("v"));
    options_reply.send(ChoiceList::Available {
        options: vec![option(1)],
    });
    let choices = host.recv();
    assert_eq!(correlation(&choices), (GENERATION, 3));

    // The invoke is completed from a different thread, long after it was dispatched.
    std::thread::spawn(move || reply.send(Outcome::Success))
        .join()
        .unwrap();
    let result = host.recv();
    assert_eq!(correlation(&result), (GENERATION, 2));
    assert_eq!(result["protocol"], 1);
    assert_eq!(
        result["message"],
        json!({"kind":"result","outcome":{"status":"success"}})
    );
    host.finish().unwrap();
}

#[test]
fn outcomes_and_choice_lists_have_the_documented_json_shape() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    invoke(&mut host, 2);
    probe
        .invokes
        .recv_timeout(wait())
        .unwrap()
        .1
        .send(Outcome::Failure {
            code: FailureCode::TimedOut,
        });
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"result","outcome":{"status":"failure","code":"timed_out"}})
    );
    query(&mut host, 3);
    probe
        .queries
        .recv_timeout(wait())
        .unwrap()
        .1
        .send(ChoiceList::Available {
            options: vec![ChoiceOption {
                value: "a".into(),
                label: "A".into(),
            }],
        });
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"choices","list":{"availability":"available","options":[{"value":"a","label":"A"}]}})
    );
    query(&mut host, 4);
    probe
        .queries
        .recv_timeout(wait())
        .unwrap()
        .1
        .send(ChoiceList::Unavailable);
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"choices","list":{"availability":"unavailable"}})
    );
    query(&mut host, 5);
    probe
        .queries
        .recv_timeout(wait())
        .unwrap()
        .1
        .send(ChoiceList::Available { options: vec![] });
    assert_eq!(
        host.recv()["message"],
        json!({"kind":"choices","list":{"availability":"available","options":[]}})
    );
    host.finish().unwrap();
}

#[test]
fn a_choice_list_the_host_would_reject_is_replaced_by_unavailable() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    let invalid: Vec<Vec<ChoiceOption>> = vec![
        (0..65).map(option).collect(),
        vec![option(1), option(1)],
        vec![ChoiceOption {
            value: "v".into(),
            label: "".into(),
        }],
        vec![ChoiceOption {
            value: "v".into(),
            label: "tab\there".into(),
        }],
        vec![ChoiceOption {
            value: "x".repeat(257),
            label: "l".into(),
        }],
    ];
    for (n, options) in invalid.into_iter().enumerate() {
        query(&mut host, 2 + n as u64);
        probe
            .queries
            .recv_timeout(wait())
            .unwrap()
            .1
            .send(ChoiceList::Available { options });
        assert_eq!(
            host.recv()["message"]["list"],
            json!({"availability":"unavailable"}),
            "case {n}"
        );
    }
    host.finish().unwrap();
}

/// Valid by the option rules, but JSON escaping makes the frame larger than 64 KiB.
#[test]
fn a_choice_list_too_big_for_one_frame_is_answered_unavailable_not_lost() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    query(&mut host, 2);
    let options: Vec<ChoiceOption> = (0..64)
        .map(|n| ChoiceOption {
            value: format!("{}{n:03}", "\"".repeat(250)),
            label: "\"".repeat(256),
        })
        .collect();
    let (_, reply) = probe.queries.recv_timeout(wait()).unwrap();
    reply.send(ChoiceList::Available { options });
    let answer = host.recv();
    assert_eq!(correlation(&answer), (GENERATION, 2));
    assert_eq!(
        answer["message"]["list"],
        json!({"availability":"unavailable"})
    );
    host.finish().unwrap();
}

#[test]
fn the_seventeenth_concurrent_action_is_busy_and_is_never_queued() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    let mut held = Vec::new();
    for request in 2..18 {
        invoke(&mut host, request);
        held.push(probe.invokes.recv_timeout(wait()).unwrap().1);
    }
    assert_eq!(held.len(), 16);
    invoke(&mut host, 18);
    let busy = host.recv();
    assert_eq!(correlation(&busy), (GENERATION, 18));
    assert_eq!(
        busy["message"],
        json!({"kind":"result","outcome":{"status":"failure","code":"busy"}})
    );
    assert!(
        probe.invokes.try_recv().is_err(),
        "a rejected action must not reach the plugin"
    );

    // Answering one frees exactly one slot, and the refused action was not remembered.
    held.pop().unwrap().send(Outcome::Success);
    assert_eq!(correlation(&host.recv()), (GENERATION, 17));
    invoke(&mut host, 19);
    let (call, reply) = probe.invokes.recv_timeout(wait()).unwrap();
    assert_eq!(call.parameters.values["n"], 19);
    reply.send(Outcome::Success);
    assert_eq!(correlation(&host.recv()), (GENERATION, 19));
    assert!(
        host.recv_within(Duration::from_millis(100)).is_none(),
        "no queued action ran"
    );
    drop(held);
    host.finish().unwrap();
}

#[test]
fn option_queries_have_their_own_cap_and_are_answered_unavailable_beyond_it() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    let mut held = Vec::new();
    for request in 2..6 {
        query(&mut host, request);
        held.push(probe.queries.recv_timeout(wait()).unwrap().1);
    }
    query(&mut host, 6);
    let refused = host.recv();
    assert_eq!(correlation(&refused), (GENERATION, 6));
    assert_eq!(
        refused["message"]["list"],
        json!({"availability":"unavailable"})
    );
    assert!(probe.queries.try_recv().is_err());
    // Actions are unaffected by pending queries.
    invoke(&mut host, 7);
    probe
        .invokes
        .recv_timeout(wait())
        .unwrap()
        .1
        .send(Outcome::Success);
    assert_eq!(correlation(&host.recv()), (GENERATION, 7));
    drop(held);
    host.finish().unwrap();
}

#[test]
fn a_reply_dropped_without_an_answer_still_answers_and_frees_its_slot() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    for round in 0..20u64 {
        // 20 rounds exceed the cap of 16, so a leaked slot would turn into `busy`.
        invoke(&mut host, 2 + round);
        drop(probe.invokes.recv_timeout(wait()).unwrap().1);
        let result = host.recv();
        assert_eq!(correlation(&result), (GENERATION, 2 + round));
        assert_eq!(
            result["message"]["outcome"],
            json!({"status":"failure","code":"failed"}),
            "round {round}"
        );
    }
    query(&mut host, 100);
    drop(probe.queries.recv_timeout(wait()).unwrap().1);
    assert_eq!(
        host.recv()["message"]["list"],
        json!({"availability":"unavailable"})
    );
    host.finish().unwrap();
}

struct Panicky {
    calls: AtomicUsize,
}
impl Plugin for Panicky {
    fn invoke(&self, _: Invoke, reply: Reply<Outcome>) {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let _held = reply;
            panic!("plugin bug");
        }
        reply.send(Outcome::Success);
    }
    fn options(&self, _: OptionsQuery, reply: Reply<ChoiceList>) {
        reply.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, _: Subscription) {}
    fn unsubscribe(&self, _: SubscriptionId) {}
}

#[test]
fn a_panicking_handler_fails_its_own_request_and_the_loop_keeps_serving() {
    let mut host = Host::start(
        config(2..=2),
        Panicky {
            calls: AtomicUsize::new(0),
        },
    );
    host.handshake(2);
    invoke(&mut host, 2);
    assert_eq!(
        host.recv()["message"]["outcome"],
        json!({"status":"failure","code":"failed"})
    );
    invoke(&mut host, 3);
    let ok = host.recv();
    assert_eq!(correlation(&ok), (GENERATION, 3));
    assert_eq!(ok["message"]["outcome"], json!({"status":"success"}));
    host.finish().unwrap();
}

/// Four threads answer at once with lists of ~30 KB; every frame must arrive intact.
#[test]
fn frames_written_from_several_threads_never_interleave() {
    let (plugin, probe) = handoff();
    let mut host = Host::start(config(2..=2), plugin);
    host.handshake(2);
    let big: Vec<ChoiceOption> = (0..64)
        .map(|n| ChoiceOption {
            value: format!("{n:03}{}", "v".repeat(200)),
            label: "l".repeat(250),
        })
        .collect();
    let expected: Value = serde_json::to_value(ChoiceList::Available {
        options: big.clone(),
    })
    .unwrap();
    let counter = Mutex::new(0u64);
    for round in 0..5u64 {
        let mut threads = Vec::new();
        for n in 0..4u64 {
            query(&mut host, 2 + round * 4 + n);
            let (_, reply) = probe.queries.recv_timeout(wait()).unwrap();
            let list = ChoiceList::Available {
                options: big.clone(),
            };
            threads.push(std::thread::spawn(move || reply.send(list)));
        }
        for _ in 0..4 {
            let frame = host.recv();
            assert_eq!(frame["message"]["list"], expected);
            *counter.lock().unwrap() += 1;
        }
        for t in threads {
            t.join().unwrap();
        }
    }
    assert_eq!(*counter.lock().unwrap(), 20);
    host.finish().unwrap();
}
