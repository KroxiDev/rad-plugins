use super::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    time::Duration,
};
fn send(s: &mut UnixStream, n: u64, message: Value) {
    let b = serde_json::to_vec(
        &json!({"protocol":1,"correlation":{"generation":1,"request":n},"message":message}),
    )
    .unwrap();
    s.write_all(&(b.len() as u32).to_be_bytes()).unwrap();
    s.write_all(&b).unwrap();
}
fn recv(s: &mut UnixStream) -> Value {
    let mut h = [0; 4];
    s.read_exact(&mut h).unwrap();
    let mut b = vec![0; u32::from_be_bytes(h) as usize];
    s.read_exact(&mut b).unwrap();
    serde_json::from_slice(&b).unwrap()
}
#[test]
fn collection_barrier_stops_readiness_until_changed() {
    let now = Instant::now();
    let mut s = Session {
        transport: None,
        phase: 3,
        attempt: now,
        retry: now,
        waits: vec![],
        queries: vec![],
        watches: vec![],
        serial: 0,
        blocked: false,
        capabilities: vec![],
        discarded: None,
        pending_text: None,
        backoff: Duration::from_millis(250),
    };
    s.event("CurrentSceneCollectionChanging", now);
    assert!(!s.ready());
    s.event("CurrentSceneCollectionChanged", now);
    assert!(s.ready());
}
fn scripted(
    calls: Vec<(&'static str, Value)>,
) -> (
    UnixStream,
    std::thread::JoinHandle<Result<(), rad_plugin_sdk::Error>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let server = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut ws = tungstenite::accept(s).unwrap();
        ws.send(tungstenite::Message::Text(
            json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
        ))
        .unwrap();
        ws.read().unwrap();
        ws.send(tungstenite::Message::Text(
            json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                .to_string()
                .into(),
        ))
        .unwrap();
        for (expected, data) in
            std::iter::once(("GetVersion", json!({"availableRequests":REQUIRED}))).chain(calls)
        {
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(req["d"]["requestType"], expected);
            ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestId":req["d"]["requestId"],"requestType":expected,"requestStatus":{"result":true,"code":100},"responseData":data}}).to_string().into())).unwrap();
        }
        let _ = ws.read();
    });
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = peer.try_clone().unwrap();
    let p = ObsPlugin::for_test(endpoint).unwrap();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), p, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    (host, run, server)
}
#[test]
fn sdk_options_return_live_scene_identity_not_labels_as_ids() {
    let (mut host, run, server) = scripted(vec![
        (
            "GetSceneCollectionList",
            json!({"currentSceneCollectionName":"C"}),
        ),
        ("GetSceneList", json!({"scenes":[{"sceneName":"Live"}]})),
    ]);
    send(
        &mut host,
        3,
        json!({"kind":"options","credential_revision":1,"scope":"action","definition":"scene","field":"scene","context":{"schema_version":1,"values":{}}}),
    );
    let reply = recv(&mut host);
    assert_eq!(reply["message"]["list"]["availability"], "available");
    let value = reply["message"]["list"]["options"][0]["value"]
        .as_str()
        .unwrap();
    assert_eq!(crate::workflow::target(value).unwrap().collection, "C");
    assert_server_finished(&server);
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
}
#[test]
fn visual_observes_output_without_control_and_unsubscribe_releases_handle() {
    let (mut host, run, server) = scripted(vec![(
        "GetRecordStatus",
        json!({"outputActive":true,"outputPaused":false}),
    )]);
    send(
        &mut host,
        3,
        json!({"kind":"subscribe","credential_revision":1,"source":"record","parameters":{"schema_version":1,"values":{}}}),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < deadline, "current visual missing");
        let reply = recv(&mut host);
        if reply["message"]["state"]["availability"] == "current" {
            assert_eq!(reply["message"]["state"]["content"]["text"], "Recording");
            break;
        }
    }
    send(
        &mut host,
        4,
        json!({"kind":"unsubscribe","subscription":{"generation":1,"request":3}}),
    );
    send(
        &mut host,
        5,
        json!({"kind":"credentials_release","revision":1}),
    );
    loop {
        if recv(&mut host)["message"]["kind"] == "credentials_released" {
            break;
        }
    }
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
}
#[test]
fn disconnect_after_effect_never_replays_on_next_wait() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let server = std::thread::spawn(move || {
        for cycle in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
            ))
            .unwrap();
            ws.read().unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(req["d"]["requestType"], "GetVersion");
            ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestType":"GetVersion","requestId":req["d"]["requestId"],"requestStatus":{"result":true,"code":100},"responseData":{"availableRequests":REQUIRED}}}).to_string().into())).unwrap();
            if cycle == 0 {
                let req: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(req["d"]["requestType"], "StartRecord");
            } else {
                assert!(ws.read().is_err(), "old command replayed");
            }
        }
    });
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = peer.try_clone().unwrap();
    let p = ObsPlugin::for_test(endpoint).unwrap();
    let shared = p.shared.clone();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), p, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    send(
        &mut host,
        3,
        json!({"kind":"invoke","credential_revision":1,"action":"record","parameters":{"schema_version":1,"values":{"operation":"start"}}}),
    );
    assert_eq!(recv(&mut host)["message"]["outcome"]["status"], "failure");
    assert!(
        lock(&shared.inner).sessions.is_empty(),
        "failed action retained a session"
    );
    send(
        &mut host,
        4,
        json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":2}}}),
    );
    assert_eq!(recv(&mut host)["message"]["outcome"]["status"], "success");
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
}
#[test]
fn record_effect_is_sent_once_and_confirmed_through_sdk() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let server = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut ws = tungstenite::accept(s).unwrap();
        ws.send(tungstenite::Message::Text(
            json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
        ))
        .unwrap();
        ws.read().unwrap();
        ws.send(tungstenite::Message::Text(
            json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                .to_string()
                .into(),
        ))
        .unwrap();
        for (expected, data) in [
            ("GetVersion", json!({"availableRequests":REQUIRED})),
            ("StartRecord", json!({})),
            (
                "GetRecordStatus",
                json!({"outputActive":true,"outputPaused":false}),
            ),
        ] {
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(req["d"]["requestType"], expected);
            ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestId":req["d"]["requestId"],"requestType":expected,"requestStatus":{"result":true,"code":100},"responseData":data}}).to_string().into())).unwrap();
        }
        let _ = ws.read();
    });
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = peer.try_clone().unwrap();
    let p = ObsPlugin::for_test(endpoint).unwrap();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), p, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    send(
        &mut host,
        3,
        json!({"kind":"invoke","credential_revision":1,"action":"record","parameters":{"schema_version":1,"values":{"operation":"start"}}}),
    );
    assert_eq!(recv(&mut host)["message"]["outcome"]["status"], "success");
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
}
#[test]
fn challenged_and_unchallenged_wait_release_socket_before_ack() {
    for challenged in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V4(v) => v,
            _ => unreachable!(),
        };
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text(
            if challenged { json!({"op":0,"d":{"rpcVersion":1,"authentication":{"salt":"lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=","challenge":"+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="}}}) } else {json!({"op":0,"d":{"rpcVersion":1}})}.to_string().into(),
        ))
        .unwrap();
            let identify: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(identify["op"], 1);
            if challenged {
                assert_eq!(
                    identify["d"]["authentication"],
                    "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
                );
            } else {
                assert!(identify["d"].get("authentication").is_none());
            }
            ws.send(tungstenite::Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            let get: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(get["d"]["requestType"], "GetVersion");
            ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestId":get["d"]["requestId"],"requestType":"GetVersion","requestStatus":{"result":true,"code":100},"responseData":{"availableRequests":crate::plugin::REQUIRED}}}).to_string().into())).unwrap();
            assert!(ws.read().is_err());
        });
        let (mut host, peer) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let input = peer.try_clone().unwrap();
        let plugin = ObsPlugin::for_test(endpoint).unwrap();
        let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, peer));
        send(
            &mut host,
            1,
            json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
        );
        assert_eq!(recv(&mut host)["message"]["kind"], "ready");
        send(
            &mut host,
            2,
            json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":"supersecretpassword"}]}),
        );
        assert_eq!(recv(&mut host)["message"]["kind"], "credentials_applied");
        send(
            &mut host,
            3,
            json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":2}}}),
        );
        assert_eq!(recv(&mut host)["message"]["outcome"]["status"], "success");
        assert_server_finished(&server);
        send(
            &mut host,
            4,
            json!({"kind":"credentials_release","revision":1}),
        );
        assert_eq!(recv(&mut host)["message"]["kind"], "credentials_released");
        server.join().unwrap();
        drop(host);
        assert!(run.join().unwrap().is_ok());
    }
}

#[test]
fn event_during_snapshot_drains_old_reply_before_resync_and_never_publishes_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        ws.send(tungstenite::Message::Text(
            json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
        ))
        .unwrap();
        ws.read().unwrap();
        ws.send(tungstenite::Message::Text(
            json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                .to_string()
                .into(),
        ))
        .unwrap();
        let v: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestType":"GetVersion","requestId":v["d"]["requestId"],"requestStatus":{"result":true},"responseData":{"availableRequests":REQUIRED}}}).to_string().into())).unwrap();
        let old: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(old["d"]["requestType"], "GetRecordStatus");
        ws.send(tungstenite::Message::Text(json!({"op":5,"d":{"eventType":"RecordStateChanged","eventData":{"outputActive":true}}}).to_string().into())).unwrap();
        ws.get_mut()
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        assert!(
            ws.read().is_err(),
            "overlapped request while old snapshot still in flight"
        );
        ws.get_mut()
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestType":"GetRecordStatus","requestId":old["d"]["requestId"],"requestStatus":{"result":true},"responseData":{"outputActive":false,"outputPaused":false}}}).to_string().into())).unwrap();
        let fresh: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        assert_ne!(old["d"]["requestId"], fresh["d"]["requestId"]);
        ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestType":"GetRecordStatus","requestId":fresh["d"]["requestId"],"requestStatus":{"result":true},"responseData":{"outputActive":true,"outputPaused":false}}}).to_string().into())).unwrap();
        let _ = ws.read();
    });
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let input = peer.try_clone().unwrap();
    let plugin = ObsPlugin::for_test(endpoint).unwrap();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    send(
        &mut host,
        3,
        json!({"kind":"subscribe","credential_revision":1,"source":"record","parameters":{"schema_version":1,"values":{}}}),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < deadline);
        let v = recv(&mut host);
        if v["message"]["state"]["availability"] == "current" {
            assert_eq!(v["message"]["state"]["content"]["text"], "Recording");
            break;
        }
    }
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
}

#[test]
fn stalled_or_flooded_old_revision_does_not_block_new_revision() {
    for flood in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V4(v) => v,
            _ => unreachable!(),
        };
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stalled, _) = listener.accept().unwrap();
            let _flood_socket = if flood {
                let mut ws = tungstenite::accept(stalled.try_clone().unwrap()).unwrap();
                for _ in 0..8 {
                    ws.send(tungstenite::Message::Text(json!({"op":5,"d":{"eventType":"SyntheticIgnored","padding":"x".repeat(16000)}}).to_string().into())).unwrap();
                }
                Some(ws)
            } else {
                None
            };
            accepted_tx.send(()).unwrap();
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
            ))
            .unwrap();
            ws.read().unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestType":"GetVersion","requestId":req["d"]["requestId"],"requestStatus":{"result":true},"responseData":{"availableRequests":REQUIRED}}}).to_string().into())).unwrap();
            let _ = ws.read();
            drop(stalled);
        });
        let (mut host, peer) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let input = peer.try_clone().unwrap();
        let plugin = ObsPlugin::for_test(endpoint).unwrap();
        let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, peer));
        send(
            &mut host,
            1,
            json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
        );
        recv(&mut host);
        send(
            &mut host,
            2,
            json!({"kind":"credentials","revision":1,"values":[]}),
        );
        recv(&mut host);
        send(
            &mut host,
            3,
            json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":3}}}),
        );
        accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        send(
            &mut host,
            4,
            json!({"kind":"credentials","revision":2,"values":[]}),
        );
        recv(&mut host);
        let start = Instant::now();
        send(
            &mut host,
            5,
            json!({"kind":"invoke","credential_revision":2,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":1}}}),
        );
        let reply = recv(&mut host);
        assert_eq!(reply["correlation"]["request"], 5);
        assert_eq!(reply["message"]["outcome"]["status"], "success");
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(host);
        assert!(run.join().unwrap().is_ok());
        server.join().unwrap();
    }
}

#[test]
fn flooded_session_defers_payload_over_64k_until_next_worker_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let (sent_tx, sent_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        ws.send(tungstenite::Message::Text("ready".into())).unwrap();
        for i in 0..5 {
            ws.send(tungstenite::Message::Text(json!({"op":5,"d":{"eventType":if i==4 {"CurrentSceneCollectionChanging"}else{"SyntheticIgnored"},"padding":"x".repeat(16000)}}).to_string().into())).unwrap();
        }
        sent_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    let mut transport = Transport::connect(endpoint).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while transport.receive().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let now = Instant::now();
    let mut session = Session {
        transport: Some(transport),
        phase: 3,
        attempt: now,
        retry: now,
        waits: vec![],
        queries: vec![],
        watches: vec![],
        serial: 0,
        blocked: false,
        capabilities: vec![],
        discarded: None,
        pending_text: None,
        backoff: Duration::from_millis(250),
    };
    session.receive(now).unwrap();
    assert!(!session.blocked, "processed fifth frame past 64KiB budget");
    assert!(
        !session.ready(),
        "pending event must drain before new requests/readiness"
    );
    session.receive(now).unwrap();
    assert!(session.blocked, "deferred frame lost");
    done_tx.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn wait_deadlines_default30_and_bounds1_120_use_injected_instant_without_sleep() {
    let now = Instant::now();
    for (value, seconds) in [(None, 30), (Some(json!(1)), 1), (Some(json!(120)), 120)] {
        let mut params = Parameters {
            schema_version: 1,
            values: serde_json::Map::new(),
        };
        if let Some(v) = value {
            params.values.insert("timeout_seconds".into(), v);
        }
        let deadline = now + Duration::from_secs(action_timeout("wait", &params).unwrap());
        assert_eq!(deadline.duration_since(now), Duration::from_secs(seconds));
        assert!(now + Duration::from_secs(seconds - 1) < deadline);
        assert!(now + Duration::from_secs(seconds) >= deadline);
    }
    for value in [json!(0), json!(121), json!(-1), json!(1.5), json!("30")] {
        let params = Parameters {
            schema_version: 1,
            values: json!({"timeout_seconds":value})
                .as_object()
                .unwrap()
                .clone(),
        };
        assert_eq!(
            action_timeout("wait", &params),
            Err(FailureCode::InvalidParameters)
        );
        assert_eq!(action_timeout("record", &params), Ok(5));
    }
}

#[test]
fn worker_clock_expires_wait_at_one_default_thirty_and_120_seconds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            sessions: BTreeMap::new(),
            stop: false,
        }),
        wake: Condvar::new(),
        endpoint,
    });
    // No worker thread: the identical production turn is driven by this test clock.
    let plugin = ObsPlugin {
        shared: shared.clone(),
        thread: None,
    };
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = peer.try_clone().unwrap();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    for (i, values, seconds) in [
        (3, json!({"timeout_seconds":1}), 1),
        (4, json!({}), 30),
        (5, json!({"timeout_seconds":120}), 120),
    ] {
        let start = Instant::now();
        send(
            &mut host,
            i,
            json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":values}}),
        );
        let deadline = loop {
            let inner = lock(&shared.inner);
            if let Some(w) = inner
                .sessions
                .get(&(1, endpoint))
                .and_then(|s| s.waits.first())
            {
                break w.deadline;
            }
            drop(inner);
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::yield_now();
        };
        assert!(deadline.duration_since(start) >= Duration::from_secs(seconds));
        assert!(
            deadline.duration_since(start)
                < Duration::from_secs(seconds) + Duration::from_millis(100)
        );
        worker_turn(&shared, deadline - Duration::from_nanos(1));
        assert_eq!(lock(&shared.inner).sessions[&(1, endpoint)].waits.len(), 1);
        worker_turn(&shared, deadline);
        assert!(lock(&shared.inner).sessions.is_empty());
        let answer = recv(&mut host);
        assert_eq!(answer["message"]["outcome"]["code"], "timed_out");
    }
    drop(host);
    assert!(run.join().unwrap().is_ok());
}

#[test]
fn thousand_revision_release_cycles_keep_fd_threads_and_tables_stable() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "plugin::tests::revision_churn_child",
            "--test-threads=1",
        ])
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
#[ignore = "isolated child invoked by thousand_revision_release_cycles"]
fn revision_churn_child() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn count(path: &str) -> usize {
        std::fs::read_dir(path).unwrap().count()
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let closed = Arc::new(AtomicUsize::new(0));
    let observed = closed.clone();
    let server = std::thread::spawn(move || {
        for n in 1..=1000 {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
            ))
            .unwrap();
            ws.read().unwrap();
            ws.send(tungstenite::Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            let request: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(tungstenite::Message::Text(json!({"op":7,"d":{"requestType":"GetVersion","requestId":request["d"]["requestId"],"requestStatus":{"result":true},"responseData":{"availableRequests":REQUIRED}}}).to_string().into())).unwrap();
            assert!(ws.read().is_err(), "release must close socket");
            drop(ws);
            observed.store(n, Ordering::SeqCst);
        }
    });
    let plugin = ObsPlugin::for_test(endpoint).unwrap();
    let shared = plugin.shared.clone();
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = peer.try_clone().unwrap();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    std::thread::sleep(Duration::from_millis(20));
    let initial_fds = count("/proc/self/fd");
    let initial_threads = count("/proc/self/task");
    for revision in 1..=1000 {
        let id = revision * 3 - 1;
        send(
            &mut host,
            id,
            json!({"kind":"credentials","revision":revision,"values":[]}),
        );
        assert_eq!(recv(&mut host)["message"]["kind"], "credentials_applied");
        send(
            &mut host,
            id + 1,
            json!({"kind":"invoke","credential_revision":revision,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":2}}}),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if closed.load(Ordering::SeqCst) >= revision as usize {
                break;
            }
            assert!(Instant::now() < deadline);
            worker_turn(&shared, Instant::now());
            std::thread::yield_now();
        }
        assert_eq!(recv(&mut host)["message"]["outcome"]["status"], "success");
        send(
            &mut host,
            id + 2,
            json!({"kind":"credentials_release","revision":revision}),
        );
        assert_eq!(recv(&mut host)["message"]["kind"], "credentials_released");
        assert!(
            lock(&shared.inner).sessions.is_empty(),
            "ACK before retirement"
        );
        while closed.load(Ordering::SeqCst) < revision as usize {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(
            count("/proc/self/fd") <= initial_fds,
            "FD growth at revision {revision}"
        );
        assert!(count("/proc/self/task") <= initial_threads, "thread growth");
    }
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
}

#[test]
fn failed_attempt_backoff_grows_to_two_seconds_with_controlled_clock() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    drop(listener);
    let mut now = Instant::now();
    let mut s = Session {
        transport: None,
        phase: 0,
        attempt: now,
        retry: now,
        waits: vec![],
        queries: vec![],
        watches: vec![],
        serial: 0,
        blocked: false,
        capabilities: vec![],
        discarded: None,
        pending_text: None,
        backoff: Duration::from_millis(250),
    };
    for millis in [250, 500, 1000, 2000, 2000] {
        s.transport = Some(Transport::connect(endpoint).unwrap());
        s.attempt = now;
        let limit = Instant::now() + Duration::from_secs(2);
        while s.step(endpoint, now).is_ok() {
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        assert_eq!(s.retry.duration_since(now), Duration::from_millis(millis));
        now += Duration::from_secs(3);
    }
}

#[test]
fn shared_transport_survives_one_consumer_and_closes_after_the_last() {
    let (mut host, run, server) = scripted(vec![
        (
            "GetRecordStatus",
            json!({"outputActive":true,"outputPaused":false}),
        ),
        (
            "GetRecordStatus",
            json!({"outputActive":true,"outputPaused":false}),
        ),
    ]);
    for request in [3, 4] {
        send(
            &mut host,
            request,
            json!({"kind":"subscribe","credential_revision":1,"source":"record","parameters":{"schema_version":1,"values":{}}}),
        );
        loop {
            let reply = recv(&mut host);
            if reply["correlation"]["request"] == request
                && reply["message"]["state"]["availability"] == "current"
            {
                break;
            }
        }
    }
    send(
        &mut host,
        5,
        json!({"kind":"unsubscribe","subscription":{"generation":1,"request":3}}),
    );
    // Acknowledged later input establishes that unsubscribe has been handled.
    send(
        &mut host,
        6,
        json!({"kind":"credentials","revision":2,"values":[]}),
    );
    while recv(&mut host)["message"]["kind"] != "credentials_applied" {}
    assert!(
        !server.is_finished(),
        "remaining consumer lost its shared transport"
    );
    // A Wait shares the surviving authenticated connection; a second connection would not
    // be served by this fixture and could not answer successfully.
    send(
        &mut host,
        7,
        json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":1}}}),
    );
    loop {
        let reply = recv(&mut host);
        if reply["correlation"]["request"] == 7 {
            assert_eq!(reply["message"]["outcome"]["status"], "success");
            break;
        }
    }
    send(
        &mut host,
        8,
        json!({"kind":"unsubscribe","subscription":{"generation":1,"request":4}}),
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    while !server.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let released = server.is_finished();
    drop(host);
    assert!(run.join().unwrap().is_ok());
    server.join().unwrap();
    assert!(
        released,
        "last consumer left its OBS socket open without demand"
    );
}

fn assert_server_finished(server: &std::thread::JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !server.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        server.is_finished(),
        "last operation left its OBS transport alive"
    );
}

#[test]
fn construction_without_consumers_opens_no_obs_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let std::net::SocketAddr::V4(endpoint) = listener.local_addr().unwrap() else {
        unreachable!()
    };
    let plugin = ObsPlugin::for_test(endpoint).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(lock(&plugin.shared.inner).sessions.is_empty());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn manifest_declares_optional_bounded_port_settings() {
    let manifest: Value = serde_json::from_str(include_str!("../../manifest.json")).unwrap();
    assert!(
        manifest["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("settings"))
    );
    assert_eq!(
        manifest["settings"],
        json!({"version":1,"fields":[{"id":"port","label":"WebSocket port","required":false,"kind":{"type":"integer","min":1,"max":65535}}]})
    );
}

#[test]
fn settings_port_separates_live_sessions_with_the_same_credentials() {
    let first = TcpListener::bind("127.0.0.1:0").unwrap();
    let second = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = match first.local_addr().unwrap() {
        std::net::SocketAddr::V4(v) => v,
        _ => unreachable!(),
    };
    let other = second.local_addr().unwrap().port();
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            sessions: BTreeMap::new(),
            stop: false,
        }),
        wake: Condvar::new(),
        endpoint,
    });
    let plugin = ObsPlugin {
        shared: shared.clone(),
        thread: None,
    };
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let input = peer.try_clone().unwrap();
    let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","settings"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":2,"patch":0}}),
    );
    recv(&mut host);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    for (i, port) in [(3, endpoint.port()), (4, other)] {
        send(
            &mut host,
            i,
            json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":{}},"settings":{"schema_version":1,"values":{"port":port}}}),
        );
    }
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let inner = lock(&shared.inner);
        if inner
            .sessions
            .values()
            .map(|s| s.waits.len())
            .sum::<usize>()
            == 2
        {
            break;
        }
        drop(inner);
        assert!(Instant::now() < until, "SDK did not admit both calls");
        std::thread::yield_now();
    }
    let count = lock(&shared.inner).sessions.len();
    worker_turn(&shared, Instant::now());
    first.set_nonblocking(true).unwrap();
    second.set_nonblocking(true).unwrap();
    assert!(
        first.accept().is_ok(),
        "old operation keeps its original endpoint"
    );
    assert!(
        second.accept().is_ok(),
        "new operation connects to the selected endpoint"
    );
    drop(host);
    assert!(run.join().unwrap().is_ok());
    assert_eq!(
        count, 2,
        "changing port must not move old work into the new session"
    );
}

#[test]
fn settings_port_is_bounded_loopback_and_test_mode_never_reaches_personal_obs() {
    let shared = Shared {
        inner: Mutex::new(Inner {
            sessions: BTreeMap::new(),
            stop: false,
        }),
        wake: Condvar::new(),
        endpoint: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4455),
    };
    assert_eq!(shared.endpoint_for(None).unwrap().port(), 4455);
    for port in [1, 65535] {
        let parameters =
            serde_json::from_value(json!({"schema_version":1,"values":{"port":port}})).unwrap();
        let endpoint = shared.endpoint_for(Some(&parameters)).unwrap();
        assert_eq!(*endpoint.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(endpoint.port(), port);
    }
    for value in [
        json!(0),
        json!(65536),
        json!(-1),
        json!(1.5),
        json!("4455"),
        Value::Null,
    ] {
        let parameters =
            serde_json::from_value(json!({"schema_version":1,"values":{"port":value}})).unwrap();
        assert_eq!(
            shared.endpoint_for(Some(&parameters)),
            Err(FailureCode::InvalidParameters)
        );
    }
    let test = Shared {
        endpoint: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 12345),
        ..shared
    };
    let parameters =
        serde_json::from_value(json!({"schema_version":1,"values":{"port":4455}})).unwrap();
    assert_eq!(
        test.endpoint_for(Some(&parameters)),
        Err(FailureCode::InvalidParameters)
    );
    assert_eq!(test.endpoint_for(None).unwrap().port(), 12345);
}
