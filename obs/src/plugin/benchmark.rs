//! Repeatable wall-clock measurements of the real SDK and production worker.
use super::tests::{AVAILABLE, recv, send};
use super::*;
use std::{io, net::TcpListener, os::unix::net::UnixStream, sync::mpsc};

#[derive(Default)]
struct Model {
    flags: BTreeMap<String, bool>,
    scene: String,
    replays: u64,
}
impl Model {
    fn answer(&mut self, name: &str, data: &Value) -> Value {
        match name {
            "GetVersion" => json!({"availableRequests":AVAILABLE}),
            "GetSceneCollectionList" => json!({"currentSceneCollectionName":"C"}),
            "GetSceneList" => json!({"scenes":[{"sceneName":"Main"},{"sceneName":"Other"}]}),
            "GetSceneItemList" => json!({"sceneItems":[{"sceneItemId":1,"sourceName":"Camera"}]}),
            "GetCurrentProgramScene" => json!({"currentProgramSceneName":self.scene}),
            "SetCurrentProgramScene" => {
                self.scene = data["sceneName"].as_str().unwrap().into();
                json!({})
            }
            "GetInputMute" => json!({"inputMuted":self.flags["mute"]}),
            "GetSourceFilter" => json!({"filterEnabled":self.flags["filter"]}),
            "GetSceneItemEnabled" => json!({"sceneItemEnabled":self.flags["item"]}),
            "SetInputMute" => {
                self.flags
                    .insert("mute".into(), data["inputMuted"].as_bool().unwrap());
                json!({})
            }
            "SetSourceFilterEnabled" => {
                self.flags
                    .insert("filter".into(), data["filterEnabled"].as_bool().unwrap());
                json!({})
            }
            "SetSceneItemEnabled" => {
                self.flags
                    .insert("item".into(), data["sceneItemEnabled"].as_bool().unwrap());
                json!({})
            }
            "GetRecordStatus" => {
                json!({"outputActive":self.flags["record"],"outputPaused":self.flags["pause"]})
            }
            "GetStreamStatus" => json!({"outputActive":self.flags["stream"]}),
            "GetVirtualCamStatus" => json!({"outputActive":self.flags["virtualcam"]}),
            "GetReplayBufferStatus" => json!({"outputActive":self.flags["replay"]}),
            "PauseRecord" | "ResumeRecord" => {
                self.flags.insert("pause".into(), name == "PauseRecord");
                json!({})
            }
            "StartRecord" | "StopRecord" => {
                self.flags.insert("record".into(), name == "StartRecord");
                json!({})
            }
            "StartStream" | "StopStream" => {
                self.flags.insert("stream".into(), name == "StartStream");
                json!({})
            }
            "StartVirtualCam" | "StopVirtualCam" => {
                self.flags
                    .insert("virtualcam".into(), name == "StartVirtualCam");
                json!({})
            }
            "StartReplayBuffer" | "StopReplayBuffer" => {
                self.flags
                    .insert("replay".into(), name == "StartReplayBuffer");
                json!({})
            }
            "SaveReplayBuffer" => {
                self.replays += 1;
                json!({})
            }
            "GetLastReplayBufferReplay" => {
                json!({"savedReplayPath":format!("synthetic-{}", self.replays)})
            }
            _ => panic!("unmodeled request: {name}"),
        }
    }
}
enum Control {
    HoldNext,
    Release,
}
struct Bench {
    host: UnixStream,
    shared: Arc<Shared>,
    controls: mpsc::Sender<Control>,
    held: mpsc::Receiver<()>,
    orders: mpsc::Receiver<Instant>,
    requests: mpsc::Receiver<(Instant, Value)>,
    run: Option<JoinHandle<Result<(), rad_plugin_sdk::Error>>>,
    server: Option<JoinHandle<()>>,
    serial: u64,
}
impl Bench {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let std::net::SocketAddr::V4(endpoint) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        let (controls, control_rx) = mpsc::channel();
        let (held_tx, held) = mpsc::channel();
        let (order_tx, orders) = mpsc::channel();
        let (request_tx, requests) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_nodelay(true).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let emit = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, value: Value| {
                ws.send(tungstenite::Message::Text(value.to_string().into()))
                    .unwrap();
            };
            emit(&mut ws, json!({"op":0,"d":{"rpcVersion":1}}));
            ws.read().unwrap();
            emit(&mut ws, json!({"op":2,"d":{"negotiatedRpcVersion":1}}));
            ws.get_mut()
                .set_read_timeout(Some(Duration::from_millis(1)))
                .unwrap();
            let mut model = Model {
                scene: "Main".into(),
                ..Model::default()
            };
            for name in [
                "mute",
                "filter",
                "item",
                "record",
                "pause",
                "stream",
                "virtualcam",
                "replay",
            ] {
                model
                    .flags
                    .insert(name.into(), matches!(name, "record" | "replay"));
            }
            let mut hold = false;
            let mut pending = None;
            loop {
                for command in control_rx.try_iter() {
                    match command {
                        Control::HoldNext => hold = true,
                        Control::Release => {
                            hold = false;
                            if let Some(response) = pending.take() {
                                emit(&mut ws, response);
                            }
                        }
                    }
                }
                let request: Value = match ws.read() {
                    Ok(message) => serde_json::from_str(message.to_text().unwrap()).unwrap(),
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
                request_tx.send((Instant::now(), request.clone())).unwrap();
                let name = request["d"]["requestType"].as_str().unwrap();
                if ["Set", "Start", "Stop", "Save", "Pause", "Resume"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                {
                    order_tx.send(Instant::now()).unwrap();
                }
                let data = model.answer(name, &request["d"]["requestData"]);
                let response = json!({"op":7,"d":{"requestId":request["d"]["requestId"],"requestType":name,"requestStatus":{"result":true},"responseData":data}});
                if hold {
                    assert!(
                        pending.replace(response).is_none(),
                        "only one request in flight"
                    );
                    held_tx.send(()).unwrap();
                } else {
                    emit(&mut ws, response);
                }
            }
        });
        let plugin = ObsPlugin::for_test(endpoint).unwrap();
        let shared = plugin.shared.clone();
        let (mut host, socket) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
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
            controls,
            held,
            orders,
            requests,
            run: Some(run),
            server: Some(server),
            serial: 2,
        }
    }
    fn subscribe_all(&mut self) {
        for name in [
            "record",
            "pause",
            "stream",
            "virtualcam",
            "replay",
            "mute",
            "filter",
            "item",
            "scene",
        ] {
            self.serial += 1;
            send(
                &mut self.host,
                self.serial,
                json!({"kind":"subscribe","credential_revision":1,"source":name,"parameters":{"schema_version":1,"values":parameters(name,0)}}),
            );
            loop {
                let m = recv(&mut self.host);
                if m["correlation"]["request"] == self.serial
                    && m["message"]["state"]["availability"] == "current"
                {
                    break;
                }
            }
        }
    }
    fn quiesce(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let inner = lock(&self.shared.inner);
            if inner.sessions.values().all(|s| {
                s.waits.is_empty()
                    && s.queries.is_empty()
                    && s.watches
                        .iter()
                        .all(|w| w.sent.is_none() && w.flow.is_none())
            }) {
                return;
            }
            drop(inner);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn visual_inflight(&self) {
        self.controls.send(Control::HoldNext).unwrap();
        // The peer's ACK below proves an actual visual request arrived before invoke.
        std::thread::sleep(Duration::from_millis(4));
        lock(&self.shared.inner)
            .sessions
            .values_mut()
            .next()
            .unwrap()
            .watches[0]
            .next = Instant::now();
        self.shared.wake.notify_all();
        self.held.recv_timeout(Duration::from_secs(2)).unwrap();
        let inner = lock(&self.shared.inner);
        assert!(
            inner
                .sessions
                .values()
                .any(|s| s.watches.iter().any(|w| w.sent.is_some()))
        );
    }
    fn invoke(&mut self, action: &str, sample: usize) -> (u64, Instant) {
        self.serial += 1;
        let started = Instant::now();
        let definition = if action == "save" { "replay" } else { action };
        let mut values = parameters(definition, sample);
        values["operation"] = json!(if action == "save" { "save" } else { "toggle" });
        send(
            &mut self.host,
            self.serial,
            json!({"kind":"invoke","credential_revision":1,"action":definition,"parameters":{"schema_version":1,"values":values}}),
        );
        (self.serial, started)
    }
    fn finished(&mut self, id: u64) {
        loop {
            let message = recv(&mut self.host);
            if message["correlation"]["request"] == id {
                assert_eq!(
                    message["message"]["outcome"]["status"], "success",
                    "{message}"
                );
                return;
            }
        }
    }
}
impl Drop for Bench {
    fn drop(&mut self) {
        let _ = self.host.shutdown(std::net::Shutdown::Both);
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
fn parameters(name: &str, sample: usize) -> Value {
    let target = |kind, name, item: Option<i64>| {
        json!({"v":1,"collection":"C","kind":kind,"name":name,"item":item,"source":item.map(|_| "Camera")}).to_string()
    };
    match name {
        "scene" => {
            json!({"scene":target("scene", if sample.is_multiple_of(2) {"Other"}else{"Main"}, None)})
        }
        "mute" => json!({"input":target("input","Camera",None)}),
        "filter" => json!({"source":target("input","Camera",None),"filter":"Blur"}),
        "item" => json!({"item":target("scene","Main",Some(1))}),
        _ => json!({}),
    }
}
fn report(condition: &str, action: &str, mut samples: Vec<Duration>) {
    samples.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.;
    println!(
        "OBS_BENCH,{condition},{action},{},{:.3},{:.3}",
        samples.len(),
        ms((samples[14] + samples[15]) / 2),
        ms(samples[28])
    );
}
#[test]
#[ignore = "wall-clock benchmark; synthetic loopback, run explicitly outside the gate"]
fn time_to_order_real_worker() {
    let selected = std::env::var("RAD_OBS_BENCH_ACTION").ok();
    for competition in [false, true] {
        let mut bench = Bench::new();
        bench.subscribe_all();
        for action in [
            "scene",
            "record",
            "pause",
            "stream",
            "mute",
            "item",
            "filter",
            "virtualcam",
            "replay",
            "save",
        ] {
            if selected.as_deref().is_some_and(|name| name != action) {
                continue;
            }
            let mut samples = Vec::new();
            for sample in 0..30 {
                bench.quiesce();
                if competition {
                    bench.visual_inflight();
                }
                let (id, start) = bench.invoke(action, sample);
                if competition {
                    bench.controls.send(Control::Release).unwrap();
                }
                let arrived = bench.orders.recv_timeout(Duration::from_secs(3)).unwrap();
                samples.push(arrived.duration_since(start));
                bench.finished(id);
            }
            report(
                if competition {
                    "visual_inflight"
                } else {
                    "base"
                },
                action,
                samples,
            );
        }
    }
    if selected.as_deref().is_some_and(|name| name != "queue") {
        return;
    }
    let mut bench = Bench::new();
    bench.subscribe_all();
    bench.quiesce();
    // Queue measurement isolates the two actions from periodic visual work.
    for session in lock(&bench.shared.inner).sessions.values_mut() {
        for watch in &mut session.watches {
            watch.next = Instant::now() + Duration::from_secs(3600);
        }
    }
    let mut queue = Vec::new();
    for _ in 0..30 {
        bench.quiesce();
        bench.requests.try_iter().for_each(drop);
        let (first, _) = bench.invoke("mute", 0);
        let (second, second_started) = bench.invoke("mute", 0);
        bench.orders.recv_timeout(Duration::from_secs(3)).unwrap();
        bench.finished(first);
        bench.orders.recv_timeout(Duration::from_secs(3)).unwrap();
        bench.finished(second);
        let second_request = bench
            .requests
            .try_iter()
            .filter(|(_, r)| r["d"]["requestType"] == "GetSceneCollectionList")
            .nth(1)
            .unwrap()
            .0;
        queue.push(second_request.duration_since(second_started));
    }
    report("queue_wait", "second_mute", queue);
    drop(bench);
    if selected.is_some() {
        return;
    }
    let mut cold = Vec::new();
    for _ in 0..30 {
        let mut bench = Bench::new();
        assert!(lock(&bench.shared.inner).sessions.is_empty());
        let (id, start) = bench.invoke("mute", 0);
        cold.push(
            bench
                .orders
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .duration_since(start),
        );
        bench.finished(id);
    }
    report("cold", "mute", cold);
}
