//! Private OBS protocol oracle. Never contacts OBS; binds only an ephemeral loopback port.
use serde_json::{Value, json};
#[derive(Default)]
struct Model {
    effects: Vec<Value>,
    items: std::collections::BTreeMap<(String, i64), bool>,
    scene: String,
    filter: bool,
    record: bool,
    paused: bool,
    stream: bool,
}
const REQUESTS: &[&str] = &[
    "GetVersion",
    "GetSceneCollectionList",
    "GetSceneList",
    "GetInputList",
    "GetGroupList",
    "GetSourceFilterList",
    "GetSourceFilter",
    "SetSourceFilterEnabled",
    "GetSceneItemList",
    "GetGroupSceneItemList",
    "GetSceneItemEnabled",
    "SetSceneItemEnabled",
    "GetCurrentProgramScene",
    "SetCurrentProgramScene",
    "GetRecordStatus",
    "GetStreamStatus",
    "StartRecord",
    "StopRecord",
    "PauseRecord",
    "ResumeRecord",
    "StartStream",
    "StopStream",
];
impl Model {
    fn request(&mut self, kind: &str, data: &Value) -> Result<Value, ()> {
        let response = match kind {
            "GetVersion" => json!({"availableRequests":REQUESTS}),
            "GetSceneCollectionList" => json!({"currentSceneCollectionName":"C"}),
            "GetSceneList" => json!({"scenes":[{"sceneName":"Root"},{"sceneName":"Live"}]}),
            "GetInputList" => json!({"inputs":[{"inputName":"Camera"}]}),
            "GetGroupList" => json!({"groups":["G"]}),
            "GetSourceFilterList" if data["sourceName"] == "Camera" => {
                json!({"filters":[{"filterName":"Blur"}]})
            }
            "GetSourceFilter" if data["sourceName"] == "Camera" && data["filterName"] == "Blur" => {
                json!({"filterEnabled":self.filter})
            }
            "SetSourceFilterEnabled"
                if data["sourceName"] == "Camera" && data["filterName"] == "Blur" =>
            {
                self.filter = data["filterEnabled"].as_bool().ok_or(())?;
                json!({})
            }
            "GetSceneItemList" if data["sceneName"] == "Root" => {
                json!({"sceneItems":[{"sceneItemId":1,"sourceName":"Camera","isGroup":false},{"sceneItemId":2,"sourceName":"Camera","isGroup":false},{"sceneItemId":3,"sourceName":"G","isGroup":true}]})
            }
            "GetSceneItemList" if data["sceneName"] == "Live" => json!({"sceneItems":[]}),
            "GetGroupSceneItemList" if data["sceneName"] == "G" => {
                json!({"sceneItems":[{"sceneItemId":8,"sourceName":"Camera","isGroup":false}]})
            }
            "SetSceneItemEnabled" | "GetSceneItemEnabled" => {
                let name = data["sceneName"].as_str().ok_or(())?;
                let id = data["sceneItemId"].as_i64().ok_or(())?;
                if !matches!((name, id), ("Root", 1..=3) | ("G", 8)) {
                    return Err(());
                }
                if kind.starts_with("Set") {
                    self.items.insert(
                        (name.into(), id),
                        data["sceneItemEnabled"].as_bool().ok_or(())?,
                    );
                    json!({})
                } else {
                    json!({"sceneItemEnabled":self.items.get(&(name.into(),id)).copied().unwrap_or(false)})
                }
            }
            "SetCurrentProgramScene"
                if [json!("Root"), json!("Live")].contains(&data["sceneName"]) =>
            {
                self.scene = data["sceneName"].as_str().unwrap().into();
                json!({})
            }
            "GetCurrentProgramScene" => {
                json!({"currentProgramSceneName":if self.scene.is_empty(){"Root"}else{&self.scene}})
            }
            "GetRecordStatus" => json!({"outputActive":self.record,"outputPaused":self.paused}),
            "GetStreamStatus" => json!({"outputActive":self.stream}),
            "StartRecord" => {
                self.record = true;
                json!({})
            }
            "StopRecord" => {
                self.record = false;
                json!({})
            }
            "PauseRecord" => {
                self.paused = true;
                json!({})
            }
            "ResumeRecord" => {
                self.paused = false;
                json!({})
            }
            "StartStream" => {
                self.stream = true;
                json!({})
            }
            "StopStream" => {
                self.stream = false;
                json!({})
            }
            _ => return Err(()),
        };
        if !kind.starts_with("Get") {
            self.effects.push(json!({"request":kind,"data":data}));
        }
        Ok(response)
    }
}
fn output(value: Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
fn main() {
    use std::{
        io::BufRead,
        net::{TcpListener, TcpStream},
        sync::mpsc,
        time::Duration,
    };
    use tungstenite::{Message, WebSocket};
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    output(json!({"port":listener.local_addr().unwrap().port()}));
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.len() > 4096 {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                break;
            };
            if tx.send(v).is_err() {
                break;
            }
        }
    });
    let mut clients: Vec<(WebSocket<TcpStream>, bool)> = vec![];
    let mut model = Model::default();
    let mut online = true;
    loop {
        match rx.try_recv() {
            Ok(command) => match command["op"].as_str() {
                Some("snapshot") => output(json!({"effects":model.effects})),
                Some("offline") => {
                    online = false;
                    clients.clear();
                    output(json!({"online":false}));
                }
                Some("online") => {
                    online = true;
                    output(json!({"online":true}));
                }
                Some("external") => {
                    if let Some(scene) = command["scene"]
                        .as_str()
                        .filter(|s| ["Root", "Live"].contains(s))
                    {
                        model.scene = scene.into();
                    }
                    for (ws, auth) in &mut clients {
                        if *auth {
                            let _=ws.send(Message::Text(json!({"op":5,"d":{"eventType":"CurrentProgramSceneChanged","eventIntent":4,"eventData":{"sceneName":model.scene}}}).to_string().into()));
                        }
                    }
                    output(json!({"external":true}));
                }
                _ => output(json!({"error":"invalid_control"})),
            },
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Ok((stream, _)) = listener.accept()
            && online
            && clients.len() < 8
        {
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            if let Ok(mut ws) = tungstenite::accept_with_config(
                stream,
                Some(
                    tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(65536))
                        .max_frame_size(Some(65536)),
                ),
            ) {
                let hello = json!({"op":0,"d":{"obsWebSocketVersion":"5.5.0","rpcVersion":1,"authentication":{"salt":"salt","challenge":"challenge"}}});
                if ws.send(Message::Text(hello.to_string().into())).is_ok() {
                    ws.get_mut().set_nonblocking(true).unwrap();
                    clients.push((ws, false));
                }
            }
        }
        clients.retain_mut(|(ws, auth)| match ws.read() {
            Ok(Message::Text(text)) => {
                if text.len() > 65536 {
                    return false;
                }
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    return false;
                };
                if !*auth {
                    // Independent hashlib vector, not the plugin authentication function.
                    if v["op"] != 1
                        || v["d"]["authentication"]
                            != "cISp6Ke/7kij8hHddTJ3KKN8jzFrOVBqfeSd6PJcJNk="
                    {
                        let _ = ws.close(Some(tungstenite::protocol::CloseFrame {
                            code: 4009u16.into(),
                            reason: "authentication failed".into(),
                        }));
                        return false;
                    }
                    *auth = true;
                    let identified = json!({"op":2,"d":{"negotiatedRpcVersion":1}});
                    return ws
                        .send(Message::Text(identified.to_string().into()))
                        .is_ok();
                }
                if v["op"] != 6 {
                    return false;
                }
                let Some(kind) = v["d"]["requestType"].as_str() else {
                    return false;
                };
                let data = v["d"].get("requestData").cloned().unwrap_or(json!({}));
                let result = model.request(kind, &data);
                let ok = result.is_ok();
                let response = json!({"op":7,"d":{
                    "requestId":v["d"]["requestId"], "requestType":kind,
                    "requestStatus":{"result":ok,"code":if ok {100} else {600}},
                    "responseData":result.unwrap_or(json!({}))
                }});
                ws.send(Message::Text(response.to_string().into())).is_ok()
            }
            Ok(Message::Close(_)) => false,
            Ok(_) => true,
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(_) => false,
        });
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_changes_are_observed_and_unknown_targets_have_no_effect() {
        let mut m = Model::default();
        assert!(
            m.request("SetCurrentProgramScene", &json!({"sceneName":"Live"}))
                .is_ok()
        );
        assert_eq!(
            m.request("GetCurrentProgramScene", &json!({})).unwrap()["currentProgramSceneName"],
            "Live"
        );
        assert!(
            m.request("SetCurrentProgramScene", &json!({"sceneName":"absent"}))
                .is_err()
        );
        assert_eq!(m.effects.len(), 1);
    }
    #[test]
    fn exact_item_target_is_required_before_any_effect() {
        let mut m = Model::default();
        assert!(
            m.request(
                "SetSceneItemEnabled",
                &json!({"sceneName":"G","sceneItemId":8,"sceneItemEnabled":true})
            )
            .is_ok()
        );
        assert_eq!(m.effects.len(), 1);
        assert!(
            m.request(
                "SetSceneItemEnabled",
                &json!({"sceneName":"Root","sceneItemId":8,"sceneItemEnabled":true})
            )
            .is_err()
        );
        assert_eq!(m.effects.len(), 1);
    }
}
