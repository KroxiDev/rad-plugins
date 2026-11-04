//! Event regressions use the real websocket/SDK boundaries and a controlled worker clock.
use super::tests::{AVAILABLE, recv, send};
use super::*;
use std::{io, net::TcpListener, os::unix::net::UnixStream, sync::mpsc};

type Socket = tungstenite::WebSocket<std::net::TcpStream>;
struct Peer {
    host: UnixStream,
    shared: Arc<Shared>,
    events: mpsc::Sender<Value>,
    requests: mpsc::Receiver<Value>,
    run: Option<JoinHandle<Result<(), rad_plugin_sdk::Error>>>,
    server: Option<JoinHandle<()>>,
    now: Instant,
    serial: u64,
    inbox: std::collections::VecDeque<Value>,
}
impl Peer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let std::net::SocketAddr::V4(endpoint) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        let (events, incoming) = mpsc::channel::<Value>();
        let (requests_tx, requests) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.set_nodelay(true).unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            emit(&mut ws, json!({"op":0,"d":{"rpcVersion":1}}));
            ws.read().unwrap();
            emit(&mut ws, json!({"op":2,"d":{"negotiatedRpcVersion":1}}));
            stream_timeout(&ws, Duration::from_millis(2));
            let mut held = false;
            let mut pending = Vec::new();
            let mut missing = false;
            let mut muted = false;
            'serve: loop {
                for event in incoming.try_iter() {
                    match event["test"].as_str() {
                        Some("hold") => held = true,
                        Some("release") => {
                            held = false;
                            for response in pending.drain(..) {
                                emit(&mut ws, response);
                            }
                        }
                        Some("missing") => missing = true,
                        Some("disconnect") => break 'serve,
                        _ => emit(&mut ws, event),
                    }
                }
                let request = match ws.read() {
                    Ok(message) => {
                        serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap()
                    }
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                };
                requests_tx.send(request.clone()).unwrap();
                let data = match request["d"]["requestType"].as_str().unwrap() {
                    "GetVersion" => json!({"availableRequests":AVAILABLE}),
                    "GetSceneCollectionList" => json!({"currentSceneCollectionName":"C"}),
                    "GetCurrentProgramScene" => json!({"currentProgramSceneName":"Main"}),
                    "GetStreamStatus" | "GetVirtualCamStatus" | "GetReplayBufferStatus" => {
                        json!({"outputActive":false})
                    }
                    "GetInputMute" => json!({"inputMuted":muted}),
                    "SetInputMute" => {
                        muted = request["d"]["requestData"]["inputMuted"].as_bool().unwrap();
                        json!({})
                    }
                    "GetSceneList" => {
                        json!({"scenes":[{"sceneName":"Main"},{"sceneName":"Other"},{"sceneName":"Third"}]})
                    }
                    "GetSourceFilter" => json!({"filterEnabled":true}),
                    "GetSceneItemList" => {
                        json!({"sceneItems":[{"sceneItemId":1,"sourceName":"Camera"},{"sceneItemId":2,"sourceName":"Camera"}]})
                    }
                    "GetSceneItemEnabled" => json!({"sceneItemEnabled":true}),
                    "GetRecordStatus" => json!({"outputActive":true,"outputPaused":false}),
                    _ => panic!("unexpected request: {request}"),
                };
                let response = json!({"op":7,"d":{"requestId":request["d"]["requestId"],"requestType":request["d"]["requestType"],"requestStatus":{"result":!(missing && request["d"]["requestType"] == "GetInputMute")},"responseData":data}});
                if held {
                    pending.push(response);
                } else {
                    emit(&mut ws, response);
                }
            }
        });
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                sessions: BTreeMap::new(),
                stop: false,
            }),
            wake: Wake::new().unwrap(),
            endpoint,
        });
        let plugin = ObsPlugin {
            shared: shared.clone(),
            thread: None,
        };
        let (mut host, socket) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let input = socket.try_clone().unwrap();
        let run = std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, socket));
        send(
            &mut host,
            1,
            json!({"kind":"hello","capabilities":["credentials","dynamic_options","response_timeout","visual_freshness","visual_states"],"host_api":1,"target":"x86_64-unknown-linux-gnu","plugin_id":"org.rad.obs","plugin_version":{"major":0,"minor":4,"patch":1}}),
        );
        recv(&mut host);
        send(
            &mut host,
            2,
            json!({"kind":"credentials","revision":1,"values":[]}),
        );
        recv(&mut host);
        Self {
            host,
            shared,
            events,
            requests,
            run: Some(run),
            server: Some(server),
            now: Instant::now(),
            serial: 2,
            inbox: Default::default(),
        }
    }
    fn subscribe(&mut self, source: &str, values: Value) {
        self.serial += 1;
        send(
            &mut self.host,
            self.serial,
            json!({"kind":"subscribe","credential_revision":1,"source":source,"parameters":{"schema_version":1,"values":values}}),
        );
        let subscription = self.serial;
        self.serial += 1;
        send(
            &mut self.host,
            self.serial,
            json!({"kind":"credentials","revision":self.serial,"values":[]}),
        );
        while recv(&mut self.host)["message"]["kind"] != "credentials_applied" {}
        self.now = self.now.max(Instant::now());
        self.pump();
        loop {
            let message = recv(&mut self.host);
            if message["correlation"]["request"] == subscription
                && message["message"]["state"]["availability"] == "current"
            {
                break;
            }
        }
        self.requests.try_iter().for_each(drop);
    }
    fn pump(&self) {
        let until = Instant::now() + Duration::from_millis(80);
        while Instant::now() < until {
            worker_turn(&self.shared, self.now);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn event(&self, name: &str, data: Value) -> Vec<Value> {
        self.events
            .send(json!({"op":5,"d":{"eventType":name,"eventData":data}}))
            .unwrap();
        self.pump();
        self.requests.try_iter().collect()
    }
    fn control(&self, command: &str) {
        self.events.send(json!({"test":command})).unwrap();
        self.pump();
    }
    fn submit(&mut self, message: Value) -> u64 {
        self.serial += 1;
        let request = self.serial;
        send(&mut self.host, request, message);
        self.serial += 1;
        send(
            &mut self.host,
            self.serial,
            json!({"kind":"credentials","revision":self.serial,"values":[]}),
        );
        loop {
            let message = recv(&mut self.host);
            if message["message"]["kind"] == "credentials_applied" {
                break;
            }
            self.inbox.push_back(message);
        }
        self.now = self.now.max(Instant::now());
        request
    }
    fn response(&mut self, request: u64) -> Value {
        loop {
            let message = self
                .inbox
                .pop_front()
                .unwrap_or_else(|| recv(&mut self.host));
            if message["correlation"]["request"] == request {
                return message;
            }
        }
    }
    fn messages(&mut self) -> Vec<Value> {
        use std::io::Read;
        self.host
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut messages: Vec<_> = self.inbox.drain(..).collect();
        let mut header = [0; 4];
        while self.host.read_exact(&mut header).is_ok() {
            let mut body = vec![0; u32::from_be_bytes(header) as usize];
            self.host.read_exact(&mut body).unwrap();
            messages.push(serde_json::from_slice(&body).unwrap());
        }
        self.host
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        messages
    }
    fn assert_current(&self) {
        let inner = lock(&self.shared.inner);
        for session in inner.sessions.values() {
            for watch in &session.watches {
                assert!(matches!(watch.last, VisualState::Current { .. }));
            }
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.host.shutdown(std::net::Shutdown::Both).unwrap();
        lock(&self.shared.inner).sessions.clear();
        let result = self.run.take().unwrap().join();
        if !std::thread::panicking() {
            result.unwrap().unwrap();
        }
        let result = self.server.take().unwrap().join();
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}
fn emit(ws: &mut Socket, value: Value) {
    ws.send(tungstenite::Message::Text(value.to_string().into()))
        .unwrap();
}
fn stream_timeout(ws: &Socket, timeout: Duration) {
    ws.get_ref().set_read_timeout(Some(timeout)).unwrap();
}
fn target(kind: &str, name: &str, item: Option<i64>) -> String {
    json!({"v":1,"collection":"C","kind":kind,"name":name,"item":item,"source":item.map(|_| "Camera")}).to_string()
}

#[test]
fn targeted_events_read_only_matching_visuals_without_losing_current_state() {
    let cases = [
        (
            "mute",
            json!({"input":target("input", "Mic", None)}),
            json!({"input":target("input", "Other", None)}),
            "InputMuteStateChanged",
            json!({"inputName":"Mic","inputMuted":true}),
            "GetInputMute",
            "inputName",
            json!("Mic"),
        ),
        (
            "item",
            json!({"item":target("scene", "Main", Some(1))}),
            json!({"item":target("scene", "Main", Some(2))}),
            "SceneItemEnableStateChanged",
            json!({"sceneName":"Main","sceneItemId":1,"sceneItemEnabled":false}),
            "GetSceneItemEnabled",
            "sceneItemId",
            json!(1),
        ),
        (
            "filter",
            json!({"source":target("input", "Camera", None),"filter":"A"}),
            json!({"source":target("input", "Camera", None),"filter":"B"}),
            "SourceFilterEnableStateChanged",
            json!({"sourceName":"Camera","filterName":"A","filterEnabled":false}),
            "GetSourceFilter",
            "filterName",
            json!("A"),
        ),
    ];
    for (source, first, second, event, data, read, field, expected) in cases {
        let mut peer = Peer::new();
        peer.subscribe(source, first);
        peer.subscribe(source, second);
        let requests = peer.event(event, data);
        let targets: Vec<_> = requests
            .iter()
            .filter(|r| r["d"]["requestType"] == read)
            .map(|r| r["d"]["requestData"][field].clone())
            .collect();
        assert_eq!(targets, [expected], "{event}");
        peer.assert_current();
        assert!(
            peer.event(
                "SceneItemSelected",
                json!({"sceneName":"Main","sceneItemId":1})
            )
            .is_empty()
        );
        assert!(
            peer.event("InputVolumeChanged", json!({"inputName":"Mic"}))
                .is_empty()
        );
        let requests = peer.event(event, json!({}));
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["d"]["requestType"] == read)
                .count(),
            2,
            "missing target rereads only its type"
        );
        peer.host
            .set_read_timeout(Some(Duration::from_millis(30)))
            .unwrap();
        let mut header = [0; 4];
        use std::io::Read;
        // Unchanged reads only renew freshness; no event publishes unavailable.
        while peer.host.read_exact(&mut header).is_ok() {
            let mut body = vec![0; u32::from_be_bytes(header) as usize];
            peer.host.read_exact(&mut body).unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_ne!(value["message"]["state"]["availability"], "unavailable");
        }
    }
}

#[test]
fn object_lifecycle_events_refresh_only_visuals_naming_old_or_new_object() {
    let mut peer = Peer::new();
    peer.subscribe("mute", json!({"input":target("input", "Mic", None)}));
    peer.subscribe("mute", json!({"input":target("input", "Other", None)}));
    for (event, data, expected) in [
        (
            "InputNameChanged",
            json!({"oldInputName":"Mic","inputName":"New"}),
            vec!["Mic"],
        ),
        (
            "InputNameChanged",
            json!({"oldInputName":"Old","inputName":"Other"}),
            vec!["Other"],
        ),
        ("InputCreated", json!({"inputName":"Mic"}), vec!["Mic"]),
        ("InputRemoved", json!({"inputName":"Other"}), vec!["Other"]),
        (
            "InputNameChanged",
            json!({"inputName":"Other"}),
            vec!["Mic", "Other"],
        ),
    ] {
        let requests = peer.event(event, data);
        let inputs: Vec<_> = requests
            .iter()
            .filter(|r| r["d"]["requestType"] == "GetInputMute")
            .map(|r| r["d"]["requestData"]["inputName"].as_str().unwrap())
            .collect();
        assert_eq!(inputs, expected, "{event}");
    }
}

#[test]
fn options_survive_state_events_but_object_list_events_invalidate_them() {
    let mut peer = Peer::new();
    peer.subscribe("record", json!({}));
    for (event, expected) in [
        ("InputVolumeChanged", "available"),
        ("InputCreated", "unavailable"),
    ] {
        peer.requests.try_iter().for_each(drop);
        peer.control("hold");
        let request = peer.submit(json!({"kind":"options","credential_revision":1,"scope":"action","definition":"scene","field":"scene","context":{"schema_version":1,"values":{}}}));
        peer.pump();
        assert_eq!(
            peer.requests.try_iter().next().unwrap()["d"]["requestType"],
            "GetSceneCollectionList"
        );
        peer.event(event, json!({"inputName":"Mic"}));
        peer.control("release");
        assert_eq!(
            peer.response(request)["message"]["list"]["availability"],
            expected
        );
    }
}

#[test]
fn collection_barrier_preserves_visuals_even_when_inflight_read_expires_or_connection_fails() {
    let mut peer = Peer::new();
    peer.subscribe("record", json!({}));
    peer.control("hold");
    peer.now += Duration::from_secs(2);
    peer.pump();
    assert_eq!(peer.requests.try_iter().count(), 1);
    assert!(
        peer.event("CurrentSceneCollectionChanging", json!({}))
            .is_empty()
    );
    peer.now += Duration::from_secs(3);
    peer.pump();
    peer.assert_current();
    assert!(peer.requests.try_iter().next().is_none());
    peer.control("disconnect");
    peer.assert_current();
}

#[test]
fn collection_change_silences_publications_then_rereads_all_and_rejects_missing_targets() {
    let mut peer = Peer::new();
    peer.subscribe("record", json!({}));
    peer.subscribe("mute", json!({"input":target("input", "Mic", None)}));
    peer.control("hold");
    peer.now += Duration::from_secs(2);
    peer.pump();
    assert_eq!(peer.requests.try_iter().count(), 1);
    peer.event("CurrentSceneCollectionChanging", json!({}));
    peer.now += Duration::from_secs(3);
    peer.pump();
    assert!(
        peer.messages().is_empty(),
        "transition must not publish or renew"
    );
    assert!(peer.requests.try_iter().next().is_none());
    let action = peer.submit(json!({"kind":"invoke","credential_revision":1,"action":"record","parameters":{"schema_version":1,"values":{"operation":"toggle"}}}));
    peer.pump();
    assert_eq!(
        peer.response(action)["message"]["outcome"]["code"],
        "unavailable"
    );
    peer.control("release");
    assert!(
        peer.messages().is_empty(),
        "old snapshot must stay discarded"
    );
    peer.control("missing");
    let requests = peer.event("CurrentSceneCollectionChanged", json!({}));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["d"]["requestType"] == "GetRecordStatus")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["d"]["requestType"] == "GetInputMute")
            .count(),
        1
    );
    let messages = peer.messages();
    assert!(
        messages
            .iter()
            .any(|m| m["message"]["state"]["availability"] == "unavailable")
    );
    assert!(
        messages
            .iter()
            .any(|m| m["message"]["kind"] == "visual_renew"
                || m["message"]["state"]["availability"] == "current")
    );
}

#[test]
fn disconnected_visuals_are_unavailable_outside_collection_transition() {
    let mut peer = Peer::new();
    peer.subscribe("record", json!({}));
    peer.control("disconnect");
    assert!(
        peer.messages()
            .iter()
            .any(|m| m["message"]["state"]["availability"] == "unavailable")
    );
}

#[test]
fn program_scene_event_refreshes_new_scene_and_previous_program_only() {
    let mut peer = Peer::new();
    for name in ["Main", "Other", "Third"] {
        peer.subscribe("scene", json!({"scene":target("scene", name, None)}));
    }
    let requests = peer.event("CurrentProgramSceneChanged", json!({"sceneName":"Other"}));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["d"]["requestType"] == "GetCurrentProgramScene")
            .count(),
        2
    );
    assert!(
        peer.messages()
            .iter()
            .all(|m| m["message"]["state"]["availability"] != "unavailable")
    );
    let requests = peer.event("CurrentProgramSceneChanged", json!({}));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["d"]["requestType"] == "GetCurrentProgramScene")
            .count(),
        3
    );
}

#[test]
fn collection_changed_does_not_wait_for_obsolete_response_and_ignores_it_when_late() {
    let mut peer = Peer::new();
    peer.subscribe("record", json!({}));
    peer.subscribe("mute", json!({"input":target("input", "Mic", None)}));
    peer.control("hold");
    peer.now += Duration::from_secs(2);
    peer.pump();
    assert_eq!(peer.requests.try_iter().count(), 1);
    peer.event("CurrentSceneCollectionChanging", json!({}));
    peer.now += Duration::from_secs(3);
    let requests = peer.event("CurrentSceneCollectionChanged", json!({}));
    assert_eq!(
        requests.len(),
        1,
        "new collection must be readable without the old reply"
    );
    assert_eq!(requests[0]["d"]["requestType"], "GetRecordStatus");
    assert!(
        peer.messages().is_empty(),
        "no obsolete timeout publication"
    );
    peer.control("release");
    let requests: Vec<_> = peer.requests.try_iter().collect();
    assert!(
        requests
            .iter()
            .any(|r| r["d"]["requestType"] == "GetInputMute")
    );
    peer.assert_current();
    assert!(
        peer.messages()
            .iter()
            .all(|m| m["message"]["state"]["availability"] != "unavailable")
    );
}

#[test]
fn action_prereads_send_immediately_and_confirmation_alone_waits_100ms() {
    let mut peer = Peer::new();
    peer.subscribe("mute", json!({"input":target("input", "Mic", None)}));
    let id = peer.submit(json!({"kind":"invoke","credential_revision":1,"action":"mute","parameters":{"schema_version":1,"values":{"input":target("input","Mic",None),"operation":"toggle"}}}));
    peer.pump();
    let requests: Vec<_> = peer.requests.try_iter().collect();
    let names: Vec<_> = requests
        .iter()
        .map(|r| r["d"]["requestType"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["GetSceneCollectionList", "GetInputMute", "SetInputMute"]
    );
    peer.now += Duration::from_millis(99);
    peer.pump();
    assert!(
        peer.requests.try_iter().next().is_none(),
        "confirmation must remain spaced"
    );
    peer.now += Duration::from_millis(1);
    peer.pump();
    assert_eq!(
        peer.requests.try_iter().next().unwrap()["d"]["requestType"],
        "GetInputMute"
    );
    assert_eq!(peer.response(id)["message"]["outcome"]["status"], "success");
}
#[test]
fn queued_actions_read_previous_effect_and_run_before_pending_visuals() {
    let mut peer = Peer::new();
    peer.subscribe("record", json!({}));
    let invoke = json!({"kind":"invoke","credential_revision":1,"action":"mute","parameters":{"schema_version":1,"values":{"input":target("input","Mic",None),"operation":"toggle"}}});
    let first = peer.submit(invoke.clone());
    let second = peer.submit(invoke);
    for session in lock(&peer.shared.inner).sessions.values_mut() {
        session.watches[0].next = peer.now;
    }
    peer.pump();
    let requests: Vec<_> = peer.requests.try_iter().collect();
    assert_eq!(
        requests[0]["d"]["requestType"], "GetSceneCollectionList",
        "ready action has priority over visual"
    );
    let effects: Vec<_> = requests
        .iter()
        .filter(|r| r["d"]["requestType"] == "SetInputMute")
        .map(|r| r["d"]["requestData"]["inputMuted"].clone())
        .collect();
    assert_eq!(
        effects,
        [json!(true)],
        "only first action before confirmation"
    );
    peer.now += Duration::from_millis(100);
    peer.pump();
    assert_eq!(
        peer.response(first)["message"]["outcome"]["status"],
        "success"
    );
    let requests: Vec<_> = peer.requests.try_iter().collect();
    let effects: Vec<_> = requests
        .iter()
        .filter(|r| r["d"]["requestType"] == "SetInputMute")
        .map(|r| r["d"]["requestData"]["inputMuted"].clone())
        .collect();
    assert_eq!(
        effects,
        [json!(false)],
        "second reads the first effect before toggling"
    );
    peer.now += Duration::from_millis(100);
    peer.pump();
    assert_eq!(
        peer.response(second)["message"]["outcome"]["status"],
        "success"
    );
}
