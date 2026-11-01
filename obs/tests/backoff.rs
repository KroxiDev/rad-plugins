#![cfg(feature = "test-support")]
// Critical regression originally reproduced independently in review-obs/backoff-red.log.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    os::unix::net::UnixStream,
    time::Duration,
};
fn send(s: &mut UnixStream, id: u64, m: Value) {
    let b = serde_json::to_vec(
        &json!({"protocol":1,"correlation":{"generation":1,"request":id},"message":m}),
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
fn text(ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value) {
    ws.send(tungstenite::Message::Text(v.to_string().into()))
        .unwrap();
}
/// An action pressed while OBS is still opening waits for it and then runs exactly once.
/// What is never repeated is a request that already reached OBS (see the plugin's tests).
#[test]
fn an_action_during_backoff_runs_once_when_obs_accepts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let SocketAddr::V4(endpoint) = listener.local_addr().unwrap() else {
        unreachable!()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        // First attempt: OBS hangs up during the handshake, which starts a backoff.
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        tx.send(()).unwrap();
        text(&mut ws, json!({"op":0,"d":{"rpcVersion":1}}));
        ws.read().unwrap();
        ws.close(None).unwrap();
        drop(ws);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        text(&mut ws, json!({"op":0,"d":{"rpcVersion":1}}));
        ws.read().unwrap();
        text(&mut ws, json!({"op":2,"d":{"negotiatedRpcVersion":1}}));
        let (mut effects, mut active) = (0, false);
        loop {
            let Ok(frame) = ws.read() else { break };
            let req: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            let r = req["d"]["requestType"].as_str().unwrap();
            let data = match r {
                "GetVersion" => {
                    json!({"availableRequests":["GetVersion","StartRecord","StopRecord","GetRecordStatus"]})
                }
                "StartRecord" => {
                    effects += 1;
                    active = true;
                    json!({})
                }
                "GetRecordStatus" => json!({"outputActive":active,"outputPaused":false}),
                _ => panic!("unexpected {r}"),
            };
            text(
                &mut ws,
                json!({"op":7,"d":{"requestId":req["d"]["requestId"],"requestType":r,"requestStatus":{"result":true,"code":100},"responseData":data}}),
            );
        }
        effects
    });
    let (mut host, peer) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    let input = peer.try_clone().unwrap();
    let p = rad_plugin_obs::ObsPlugin::for_test(endpoint).unwrap();
    let run =
        std::thread::spawn(move || rad_plugin_sdk::run(rad_plugin_obs::config(), p, input, peer));
    send(
        &mut host,
        1,
        json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":4,"patch":0}}),
    );
    let ready = recv(&mut host);
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
    assert_eq!(ready["message"]["plugin_version"], manifest["version"]);
    send(
        &mut host,
        2,
        json!({"kind":"credentials","revision":1,"values":[]}),
    );
    recv(&mut host);
    let start = json!({"kind":"invoke","credential_revision":1,"action":"record","parameters":{"schema_version":1,"values":{"operation":"start"}}});
    send(&mut host, 3, start.clone());
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    // Pressed again while the first connection fails: both wait, and the second finds the
    // recording already started.
    std::thread::sleep(Duration::from_millis(80));
    send(&mut host, 4, start);
    let mut outcomes = std::collections::BTreeMap::new();
    while outcomes.len() < 2 {
        let r = recv(&mut host);
        outcomes.insert(
            r["correlation"]["request"].as_u64().unwrap(),
            r["message"]["outcome"]["status"].clone(),
        );
    }
    drop(host);
    assert!(run.join().unwrap().is_ok());
    let effects = server.join().unwrap();
    assert_eq!(outcomes[&3], "success");
    assert_eq!(outcomes[&4], "success");
    assert_eq!(effects, 1, "the recording started once");
}
