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
fn handshake(host: &mut UnixStream) {
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
    send(
        host,
        1,
        json!({"kind":"hello","capabilities":["visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.metrics","plugin_version":manifest["version"]}),
    );
    let ready = recv(host);
    assert_eq!(ready["message"]["kind"], "ready");
    assert_eq!(ready["message"]["plugin_version"], manifest["version"]);
}

fn subscribe(host: &mut UnixStream, id: u64, source: &str, values: Value) {
    send(
        host,
        id,
        json!({"kind":"subscribe","source":source,"parameters":{"schema_version":1,"values":values}}),
    );
}

fn recv_before(host: &mut UnixStream, deadline: Instant) -> Value {
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "timed out waiting for a visual");
    host.set_read_timeout(Some(remaining)).unwrap();
    recv(host)
}

fn expect_unavailable(host: &mut UnixStream, id: u64) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let frame = recv_before(host, deadline);
        if frame["correlation"]["request"] == id {
            assert_eq!(frame["message"]["state"]["availability"], "unavailable");
            return;
        }
    }
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
    handshake(&mut host);
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

#[test]
fn simultaneous_vram_styles_follow_their_own_values_and_reject_invalid_parameters() {
    use rad_plugin_metrics::readings::Memory;
    let (samples, reader) = mpsc::channel();
    let plugin = MetricsPlugin::with_reader(move || reader.recv().unwrap_or_default()).unwrap();
    let (mut host, wire) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    let input = wire.try_clone().unwrap();
    let running = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, wire));
    handshake(&mut host);

    subscribe(&mut host, 2, "vram", json!({}));
    expect_unavailable(&mut host, 2);
    subscribe(
        &mut host,
        3,
        "vram",
        json!({"style":"bar","value":"percent","label":"hidden"}),
    );
    expect_unavailable(&mut host, 3);
    subscribe(&mut host, 4, "cpu", json!({}));
    expect_unavailable(&mut host, 4);
    // An in-flight read from the first demand epoch may still need to finish.
    let gib = 1 << 30;
    let sample = Sample {
        cpu: Some(24),
        vram: Some(Memory {
            used: 2 * gib,
            available: 6 * gib,
            total: 8 * gib,
        }),
        ..Default::default()
    };
    samples.send(sample.clone()).unwrap();
    samples.send(sample).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut seen = [false; 3];
    while !seen.iter().all(|&current| current) {
        let frame = recv_before(&mut host, deadline);
        let id = frame["correlation"]["request"].as_u64().unwrap();
        if frame["message"]["kind"] == "visual_renew" {
            assert!(seen[(id - 2) as usize], "renewal before Current");
            continue;
        }
        let state = &frame["message"]["state"];
        assert_eq!(state["availability"], "current");
        let content = &state["content"];
        match id {
            2 => {
                assert_eq!(content["text"], "VRAM 2.0/8.0 GiB");
                assert!(content["png_hex"].is_null());
            }
            3 => {
                assert_eq!(content["text"], "25%");
                assert!(content["png_hex"].as_str().unwrap().starts_with("89504e47"));
            }
            4 => assert_eq!(content["text"], "CPU 24%"),
            _ => panic!("unexpected subscription {id}"),
        }
        seen[(id - 2) as usize] = true;
    }
    // Both sources are already current: accepting either bad choice would return Current.
    for (id, source, values) in [
        (5, "vram", json!({"style":"x"})),
        (6, "cpu", json!({"value":"used"})),
    ] {
        subscribe(&mut host, id, source, values);
        expect_unavailable(&mut host, id);
    }
    host.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(running.join().unwrap().is_ok());
}
