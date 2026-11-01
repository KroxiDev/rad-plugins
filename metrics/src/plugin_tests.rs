use super::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
    time::Duration,
};

fn send(s: &mut UnixStream, request: u64, message: Value) {
    let bytes = serde_json::to_vec(
        &json!({"protocol":1,"correlation":{"generation":7,"request":request},"message":message}),
    )
    .unwrap();
    s.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
    s.write_all(&bytes).unwrap();
}
fn receive(s: &mut UnixStream, request: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut size = [0; 4];
        s.read_exact(&mut size).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
        s.read_exact(&mut bytes).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if value["correlation"]["request"] == request {
            return value;
        }
        assert!(Instant::now() < deadline);
    }
}
fn subscribe(s: &mut UnixStream, request: u64) {
    send(
        s,
        request,
        json!({"kind":"subscribe","source":"cpu","parameters":{"schema_version":1,"values":{}}}),
    );
}
fn barrier(s: &mut UnixStream, request: u64) {
    send(
        s,
        request,
        json!({"kind":"options","scope":"visual","definition":"cpu","field":"none","context":{"schema_version":1,"values":{}}}),
    );
    receive(s, request);
}

#[test]
fn two_consumers_share_demand_and_last_release_reaches_reader_before_fresh_resubscribe() {
    let (requests_tx, requests_rx) = mpsc::channel();
    let poller = Poller::with_demand_factory(move || {
        move |demand, sample| {
            requests_tx.send((demand, sample)).unwrap();
            Sample {
                cpu: Some(37),
                ..Default::default()
            }
        }
    })
    .unwrap();
    let plugin = MetricsPlugin::with_poller(poller).unwrap();
    let shared = plugin.shared.clone();
    assert!(
        requests_rx.try_recv().is_err(),
        "construction does not poll"
    );
    let (mut host, wire) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = wire.try_clone().unwrap();
    // This unit-test peer uses the plugin's no-op options handler as a dispatch
    // barrier. Negotiate that test-only path explicitly; the production metrics
    // manifest remains visual-only and does not claim dynamic options.
    let peer = config().capabilities(vec!["dynamic_options".into(), "visual_freshness".into()]);
    let run = std::thread::spawn(move || rad_plugin_sdk::run(peer, plugin, input, wire));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["dynamic_options","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.metrics","plugin_version":{"major":0,"minor":3,"patch":0}}),
    );
    assert_eq!(receive(&mut host, 1)["message"]["kind"], "ready");
    subscribe(&mut host, 2);
    assert_eq!(
        receive(&mut host, 2)["message"]["state"]["availability"],
        "unavailable"
    );
    let (first, sample) = requests_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(first.sources, [true, false, false, false]);
    assert!(sample);
    loop {
        if receive(&mut host, 2)["message"]["state"]["availability"] == "current" {
            break;
        }
    }
    subscribe(&mut host, 3);
    assert_eq!(
        receive(&mut host, 3)["message"]["state"]["availability"],
        "current"
    );
    assert_eq!(
        shared.0.lock().unwrap().demand,
        first,
        "second consumer shares demand epoch"
    );
    send(
        &mut host,
        4,
        json!({"kind":"unsubscribe","subscription":{"generation":7,"request":2}}),
    );
    barrier(&mut host, 5);
    assert_eq!(
        shared.0.lock().unwrap().demand,
        first,
        "first release retains source"
    );
    send(
        &mut host,
        6,
        json!({"kind":"unsubscribe","subscription":{"generation":7,"request":3}}),
    );
    barrier(&mut host, 7);
    let deadline = Instant::now() + Duration::from_secs(3);
    let empty = loop {
        let (demand, sample) = requests_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        if !demand.active() {
            assert!(!sample, "release performs no read");
            break demand;
        }
        assert!(Instant::now() < deadline);
    };
    assert_eq!(empty.cpu_epoch, first.cpu_epoch + 1);
    subscribe(&mut host, 8);
    assert_eq!(
        receive(&mut host, 8)["message"]["state"]["availability"],
        "unavailable",
        "resubscribe must not replay cached Current"
    );
    let resumed = shared.0.lock().unwrap().demand;
    assert!(resumed.epoch > empty.epoch);
    assert_eq!(resumed.cpu_epoch, empty.cpu_epoch);
    loop {
        if receive(&mut host, 8)["message"]["state"]["availability"] == "current" {
            break;
        }
    }
    host.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(run.join().unwrap().is_ok());
}
