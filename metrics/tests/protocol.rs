use rad_plugin_metrics::{MetricsPlugin, config, polling::Sample};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
    time::{Duration, Instant},
};
fn send(s: &mut UnixStream, request: u64, message: Value) {
    let bytes = serde_json::to_vec(
        &json!({"protocol":1,"correlation":{"generation":7,"request":request},"message":message}),
    )
    .unwrap();
    s.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
    s.write_all(&bytes).unwrap();
}
fn recv(s: &mut UnixStream) -> Value {
    let mut n = [0; 4];
    s.read_exact(&mut n).unwrap();
    let mut b = vec![0; u32::from_be_bytes(n) as usize];
    s.read_exact(&mut b).unwrap();
    serde_json::from_slice(&b).unwrap()
}
#[test]
fn no_blind_renewal_revives_stale_metrics_and_eof_does_not_join_driver() {
    let (release_tx, release_rx) = mpsc::channel();
    let mut calls = 0;
    let plugin = MetricsPlugin::with_reader(move || {
        calls += 1;
        if calls > 1 {
            release_rx.recv().unwrap();
        }
        Sample {
            cpu: Some(37),
            ..Default::default()
        }
    })
    .unwrap();
    let (mut host, wire) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    let input = wire.try_clone().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        done_tx
            .send(rad_plugin_sdk::run(config(), plugin, input, wire))
            .unwrap();
    });
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.metrics","plugin_version":{"major":0,"minor":3,"patch":0}}),
    );
    let ready = recv(&mut host);
    assert_eq!(ready["message"]["kind"], "ready");
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
    assert_eq!(ready["message"]["plugin_version"], manifest["version"]);
    send(
        &mut host,
        2,
        json!({"kind":"subscribe","source":"cpu","parameters":{"schema_version":1,"values":{}}}),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let v = recv(&mut host);
        if v["message"]["state"]["availability"] == "current" {
            break;
        }
        assert!(Instant::now() < deadline, "no current metric");
    }
    loop {
        let v = recv(&mut host);
        if v["message"]["state"]["availability"] == "stale" {
            break;
        }
        assert!(Instant::now() < deadline, "never expired");
    }
    host.set_read_timeout(Some(Duration::from_millis(900)))
        .unwrap();
    let mut header = [0; 4];
    let error = host
        .read_exact(&mut header)
        .expect_err("no blind renewal of stale source");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    host.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("EOF must not join blocked reader")
            .is_ok()
    );
    release_tx.send(()).unwrap();
}
