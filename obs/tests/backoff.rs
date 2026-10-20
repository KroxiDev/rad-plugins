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
#[test]
fn ordinary_during_backoff_never_executes_on_a_later_attempt() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let SocketAddr::V4(endpoint) = listener.local_addr().unwrap() else {
        unreachable!()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        text(&mut ws, json!({"op":0,"d":{"rpcVersion":1}}));
        ws.read().unwrap();
        ws.close(None).unwrap();
        drop(ws);
        tx.send(()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        text(&mut ws, json!({"op":0,"d":{"rpcVersion":1}}));
        ws.read().unwrap();
        text(&mut ws, json!({"op":2,"d":{"negotiatedRpcVersion":1}}));
        let mut effects = 0;
        loop {
            let Ok(frame) = ws.read() else { break };
            let req: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            let r = req["d"]["requestType"].as_str().unwrap();
            let data = match r {
                "GetVersion" => {
                    json!({"availableRequests":["GetVersion","StartRecord","GetRecordStatus"]})
                }
                "StartRecord" => {
                    effects += 1;
                    json!({})
                }
                "GetRecordStatus" => json!({"outputActive":true,"outputPaused":false}),
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
        json!({"kind":"hello","host_api":3,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":1,"patch":1}}),
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
    send(
        &mut host,
        3,
        json!({"kind":"invoke","credential_revision":1,"action":"wait","parameters":{"schema_version":1,"values":{"timeout_seconds":2}}}),
    );
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    std::thread::sleep(Duration::from_millis(80));
    send(
        &mut host,
        4,
        json!({"kind":"invoke","credential_revision":1,"action":"record","parameters":{"schema_version":1,"values":{"operation":"start"}}}),
    );
    let mut outcome = None;
    let mut wait_done = false;
    while outcome.is_none() || !wait_done {
        let r = recv(&mut host);
        if r["correlation"]["request"] == 4 {
            outcome = Some(r["message"]["outcome"].clone());
        }
        if r["correlation"]["request"] == 3 {
            wait_done = true;
        }
    }
    let outcome = outcome.unwrap();
    drop(host);
    assert!(run.join().unwrap().is_ok());
    let effects = server.join().unwrap();
    assert_eq!(
        effects, 0,
        "ordinary queued during failed-attempt backoff executed on later connection: {outcome}"
    );
    assert_eq!(outcome["status"], "failure");
}
