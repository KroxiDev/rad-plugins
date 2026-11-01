//! Each OBS control as a short sequence of requests. An action succeeds only once a read
//! shows its effect: OBS accepting a request is not enough.
use rad_plugin_sdk::{FailureCode, Parameters};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;

#[derive(Clone)]
struct Call {
    name: &'static str,
    data: Value,
    step: Step,
}
/// What a request is for, beyond the checks every read of a target makes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Plain,
    /// Reads the state the change starts from, and decides the change.
    Read,
    /// Changes the state as the read before it decided.
    Change,
    /// Must show the effect; read again until it does or the action runs out of time.
    Confirm,
    /// The output must be running: there is nothing to save from a stopped replay buffer.
    Active,
    /// The last saved replay, if any: a failure means there is none yet.
    Previous,
}
/// How an action turns the state it read into the state it wants.
#[derive(Clone, Copy)]
enum Effect {
    /// A `Set*` request whose boolean `data[key]` carries the wanted state.
    Flag { key: &'static str },
    /// Separate requests to turn an output on and off. Nothing is sent when it already is
    /// as wanted: OBS refuses to start what runs or stop what is stopped.
    Output {
        on: &'static str,
        off: &'static str,
        needs_active: bool,
    },
}
enum Expect {
    Is(Value),
    /// A new saved replay: a path, other than the one before saving.
    Not(Value),
}
impl Expect {
    fn met(&self, value: &Value) -> bool {
        match self {
            Self::Is(expected) => value == expected,
            Self::Not(before) => value.is_string() && value != before,
        }
    }
}
pub(crate) struct Workflow {
    calls: VecDeque<Call>,
    target: Option<Target>,
    /// Field that the read and the confirmation look at.
    field: &'static str,
    /// The state the action asks for; `None` toggles the state read.
    want: Option<bool>,
    effect: Option<Effect>,
    expect: Option<Expect>,
    pub result: Value,
}
fn call(name: &'static str, data: Value) -> Call {
    Call {
        name,
        data,
        step: Step::Plain,
    }
}
fn step(name: &'static str, data: Value, step: Step) -> Call {
    Call { name, data, step }
}
/// Status request and field of each output, and the requests that turn it on and off.
fn output(name: &str) -> Option<(&'static str, &'static str, Effect)> {
    let effect = |on, off| Effect::Output {
        on,
        off,
        needs_active: false,
    };
    Some(match name {
        "record" => (
            "GetRecordStatus",
            "outputActive",
            effect("StartRecord", "StopRecord"),
        ),
        "pause" => (
            "GetRecordStatus",
            "outputPaused",
            Effect::Output {
                on: "PauseRecord",
                off: "ResumeRecord",
                needs_active: true,
            },
        ),
        "stream" => (
            "GetStreamStatus",
            "outputActive",
            effect("StartStream", "StopStream"),
        ),
        "virtualcam" => (
            "GetVirtualCamStatus",
            "outputActive",
            effect("StartVirtualCam", "StopVirtualCam"),
        ),
        "replay" => (
            "GetReplayBufferStatus",
            "outputActive",
            effect("StartReplayBuffer", "StopReplayBuffer"),
        ),
        _ => return None,
    })
}
/// The wanted state an operation names; `None` for toggle.
fn wanted(action: &str, operation: Option<&str>) -> Result<Option<bool>, FailureCode> {
    let (on, off) = match action {
        "filter" => ("on", "off"),
        "item" => ("show", "hide"),
        "mute" => ("mute", "unmute"),
        "pause" => ("pause", "resume"),
        "record" | "stream" | "virtualcam" | "replay" => ("start", "stop"),
        _ => return Err(FailureCode::InvalidParameters),
    };
    match operation {
        Some("toggle") => Ok(None),
        Some(op) if op == on => Ok(Some(true)),
        Some(op) if op == off => Ok(Some(false)),
        _ => Err(FailureCode::InvalidParameters),
    }
}
/// The source whose filter a control addresses: an input, a scene or a group.
fn source(params: &Parameters, field: &str) -> Result<Target, FailureCode> {
    target(params.text(field).ok_or(FailureCode::InvalidParameters)?)
}
/// The input whose audio a control mutes.
fn input(params: &Parameters) -> Result<Target, FailureCode> {
    let t = source(params, "input")?;
    if t.kind != "input" || t.item.is_some() {
        return Err(FailureCode::InvalidParameters);
    }
    Ok(t)
}
/// The scene item a control addresses, and the list that proves it still exists.
fn item(params: &Parameters) -> Result<(Target, Call, Value), FailureCode> {
    let t = target(params.text("item").ok_or(FailureCode::InvalidParameters)?)?;
    if t.item.is_none() || t.kind == "input" {
        return Err(FailureCode::InvalidParameters);
    }
    let list = if t.kind == "group" {
        "GetGroupSceneItemList"
    } else {
        "GetSceneItemList"
    };
    let address = json!({"sceneName":t.name,"sceneItemId":t.item});
    Ok((t.clone(), call(list, json!({"sceneName":t.name})), address))
}
fn scene(params: &Parameters) -> Result<Target, FailureCode> {
    let t = target(params.text("scene").ok_or(FailureCode::InvalidParameters)?)?;
    if t.kind != "scene" || t.item.is_some() {
        return Err(FailureCode::InvalidParameters);
    }
    Ok(t)
}
fn collection() -> Call {
    call("GetSceneCollectionList", json!({}))
}
impl Workflow {
    fn new(calls: Vec<Call>, target: Option<Target>, field: &'static str) -> Self {
        Self {
            calls: calls.into(),
            target,
            field,
            want: None,
            effect: None,
            expect: None,
            result: Value::Null,
        }
    }
    /// Read-only requests whose last answer is the state a visual shows.
    pub fn visual(name: &str, params: &Parameters) -> Result<Self, FailureCode> {
        if let Some((status, field, _)) = output(name) {
            return Ok(Self::new(vec![call(status, json!({}))], None, field));
        }
        Ok(match name {
            "scene" => {
                let t = scene(params)?;
                Self::new(
                    vec![
                        collection(),
                        call("GetSceneList", json!({})),
                        call("GetCurrentProgramScene", json!({})),
                    ],
                    Some(t),
                    "currentProgramSceneName",
                )
            }
            "filter" => {
                let (t, query) = filter(params)?;
                Self::new(
                    vec![collection(), call("GetSourceFilter", query)],
                    Some(t),
                    "filterEnabled",
                )
            }
            "item" => {
                let (t, list, address) = item(params)?;
                Self::new(
                    vec![collection(), list, call("GetSceneItemEnabled", address)],
                    Some(t),
                    "sceneItemEnabled",
                )
            }
            "mute" => {
                let t = input(params)?;
                let query = json!({"inputName":t.name});
                Self::new(
                    vec![collection(), call("GetInputMute", query)],
                    Some(t),
                    "inputMuted",
                )
            }
            _ => return Err(FailureCode::InvalidParameters),
        })
    }

    pub fn action(name: &str, params: &Parameters) -> Result<Self, FailureCode> {
        let operation = params.text("operation");
        if name == "replay" && operation == Some("save") {
            let mut flow = Self::new(
                vec![
                    step("GetReplayBufferStatus", json!({}), Step::Active),
                    step("GetLastReplayBufferReplay", json!({}), Step::Previous),
                    call("SaveReplayBuffer", json!({})),
                    step("GetLastReplayBufferReplay", json!({}), Step::Confirm),
                ],
                None,
                "savedReplayPath",
            );
            flow.expect = Some(Expect::Not(Value::Null));
            return Ok(flow);
        }
        if name == "scene" {
            let t = scene(params)?;
            let mut flow = Self::new(
                vec![
                    collection(),
                    call("SetCurrentProgramScene", json!({"sceneName":t.name})),
                    step("GetCurrentProgramScene", json!({}), Step::Confirm),
                ],
                Some(t.clone()),
                "currentProgramSceneName",
            );
            flow.expect = Some(Expect::Is(json!(t.name)));
            return Ok(flow);
        }
        let want = wanted(name, operation)?;
        let mut flow = if let Some((status, field, effect)) = output(name) {
            let mut flow = Self::new(
                vec![
                    step(status, json!({}), Step::Read),
                    // Start or stop: decided once the state is read.
                    step(status, json!({}), Step::Change),
                    step(status, json!({}), Step::Confirm),
                ],
                None,
                field,
            );
            flow.effect = Some(effect);
            flow
        } else {
            let (t, mut calls, read, set, field) = match name {
                "filter" => {
                    let (t, query) = filter(params)?;
                    (
                        t,
                        vec![collection()],
                        ("GetSourceFilter", query.clone()),
                        ("SetSourceFilterEnabled", query),
                        "filterEnabled",
                    )
                }
                "item" => {
                    let (t, list, address) = item(params)?;
                    (
                        t,
                        vec![collection(), list],
                        ("GetSceneItemEnabled", address.clone()),
                        ("SetSceneItemEnabled", address),
                        "sceneItemEnabled",
                    )
                }
                "mute" => {
                    let t = input(params)?;
                    let query = json!({"inputName":t.name});
                    (
                        t,
                        vec![collection()],
                        ("GetInputMute", query.clone()),
                        ("SetInputMute", query),
                        "inputMuted",
                    )
                }
                _ => return Err(FailureCode::InvalidParameters),
            };
            calls.extend([
                step(read.0, read.1.clone(), Step::Read),
                step(set.0, set.1, Step::Change),
                step(read.0, read.1, Step::Confirm),
            ]);
            let mut flow = Self::new(calls, Some(t), field);
            flow.effect = Some(Effect::Flag { key: field });
            flow
        };
        flow.want = want;
        Ok(flow)
    }
    pub fn request(&self) -> Option<(&str, Value)> {
        self.calls.front().map(|c| (c.name, c.data.clone()))
    }
    /// OBS refused the current request. Only the lookup of a previous replay may fail:
    /// it means no replay has been saved yet.
    pub fn reject(&mut self) -> Result<(), FailureCode> {
        if self.calls.front().is_some_and(|c| c.step == Step::Previous) {
            self.accept(Value::Null)
        } else {
            Err(FailureCode::Failed)
        }
    }
    pub fn accept(&mut self, data: Value) -> Result<(), FailureCode> {
        let call = self.calls.pop_front().ok_or(FailureCode::Failed)?;
        self.check(&call, &data)?;
        match call.step {
            Step::Plain | Step::Change => {}
            Step::Active => {
                if data["outputActive"] != true {
                    return Err(FailureCode::Unavailable);
                }
            }
            Step::Previous => self.expect = Some(Expect::Not(data["savedReplayPath"].clone())),
            Step::Read => return self.decide(&data),
            Step::Confirm => {
                if !self
                    .expect
                    .as_ref()
                    .is_some_and(|e| e.met(&data[self.field]))
                {
                    // Not yet: OBS applies some changes after acknowledging them.
                    self.calls.push_front(call);
                    return Ok(());
                }
            }
        }
        if self.calls.is_empty() {
            self.result = data;
        }
        Ok(())
    }
    /// The read state decides the change, or ends the action when nothing is left to do.
    fn decide(&mut self, data: &Value) -> Result<(), FailureCode> {
        let current = data[self.field].as_bool().ok_or(FailureCode::Failed)?;
        let want = self.want.unwrap_or(!current);
        self.expect = Some(Expect::Is(json!(want)));
        let change = self.calls.front_mut().ok_or(FailureCode::Failed)?;
        match self.effect.ok_or(FailureCode::Failed)? {
            Effect::Flag { key } => change.data[key] = json!(want),
            Effect::Output {
                on,
                off,
                needs_active,
            } => {
                if needs_active && data["outputActive"] != true {
                    return Err(FailureCode::Unavailable);
                }
                if current == want {
                    self.calls.clear();
                    self.result = data.clone();
                    return Ok(());
                }
                change.name = if want { on } else { off };
            }
        }
        Ok(())
    }
    /// The target must still be where it was chosen: same collection, scene or item.
    fn check(&self, call: &Call, data: &Value) -> Result<(), FailureCode> {
        let t = self.target.as_ref();
        match call.name {
            "GetSceneCollectionList"
                if t.is_some_and(|t| data["currentSceneCollectionName"] != t.collection) =>
            {
                Err(FailureCode::Unavailable)
            }
            "GetSceneList" => {
                let t = t.ok_or(FailureCode::Failed)?;
                let scenes = data["scenes"].as_array().ok_or(FailureCode::Failed)?;
                if scenes.iter().any(|v| v["sceneName"] == t.name) {
                    Ok(())
                } else {
                    Err(FailureCode::Unavailable)
                }
            }
            "GetSceneItemList" | "GetGroupSceneItemList" => {
                let t = t.ok_or(FailureCode::Failed)?;
                let items = data["sceneItems"].as_array().ok_or(FailureCode::Failed)?;
                if items.iter().any(|v| {
                    v["sceneItemId"].as_i64() == t.item
                        && v["sourceName"].as_str() == t.source.as_deref()
                }) {
                    Ok(())
                } else {
                    Err(FailureCode::Unavailable)
                }
            }
            _ => Ok(()),
        }
    }
}
/// The source and filter a filter control addresses, as OBS names them in requests.
fn filter(params: &Parameters) -> Result<(Target, Value), FailureCode> {
    let t = source(params, "source")?;
    let filter = params
        .text("filter")
        .filter(|v| rad_plugin_sdk::is_label(v))
        .ok_or(FailureCode::InvalidParameters)?;
    let query = json!({"sourceName":t.name,"filterName":filter});
    Ok((t, query))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    pub v: u8,
    pub collection: String,
    pub kind: String,
    pub name: String,
    pub item: Option<i64>,
    pub source: Option<String>,
}
pub(crate) fn target(value: &str) -> Result<Target, FailureCode> {
    if !rad_plugin_sdk::is_label(value) {
        return Err(FailureCode::InvalidParameters);
    }
    let t: Target = serde_json::from_str(value).map_err(|_| FailureCode::InvalidParameters)?;
    if t.v != 1
        || !["scene", "group", "input"].contains(&t.kind.as_str())
        || t.collection.is_empty()
        || t.name.is_empty()
        || t.item.is_some_and(|id| id < 0)
        || (t.item.is_some() != t.source.is_some())
    {
        return Err(FailureCode::InvalidParameters);
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn params(values: Value) -> Parameters {
        Parameters {
            schema_version: 1,
            values: values.as_object().unwrap().clone(),
        }
    }
    const CAMERA: &str =
        r#"{"v":1,"collection":"main","kind":"input","name":"camera","item":null,"source":null}"#;
    #[test]
    fn deleted_scene_visual_is_unavailable_not_inactive() {
        let p = params(
            json!({"scene":r#"{"v":1,"collection":"C","kind":"scene","name":"Gone","item":null,"source":null}"#}),
        );
        let mut flow = Workflow::visual("scene", &p).unwrap();
        flow.accept(json!({"currentSceneCollectionName":"C"}))
            .unwrap();
        assert_eq!(flow.request().unwrap().0, "GetSceneList");
        assert_eq!(
            flow.accept(json!({"scenes":[{"sceneName":"Other"}]})),
            Err(FailureCode::Unavailable)
        );
    }
    #[test]
    fn target_preserves_container_and_item_and_refuses_oversized_identity() {
        let value =
            r#"{"v":1,"collection":"main","kind":"group","name":"G","item":8,"source":"camera"}"#;
        let t = target(value).unwrap();
        assert_eq!(t.name, "G");
        assert_eq!(t.item, Some(8));
        assert!(target(&"x".repeat(257)).is_err());
        assert!(target(r#"{"v":2,"collection":"main","kind":"group","name":"G","item":8,"source":"camera"}"#).is_err());
    }
    #[test]
    fn program_scene_checks_collection_then_sets_program_not_preview() {
        let value =
            r#"{"v":1,"collection":"main","kind":"scene","name":"Live","item":null,"source":null}"#;
        let p = params(json!({ "scene": value }));
        let mut flow = Workflow::action("scene", &p).unwrap();
        assert_eq!(flow.request().unwrap().0, "GetSceneCollectionList");
        assert_eq!(
            flow.accept(json!({"currentSceneCollectionName":"other"})),
            Err(FailureCode::Unavailable)
        );
        let mut flow = Workflow::action("scene", &p).unwrap();
        flow.accept(json!({"currentSceneCollectionName":"main"}))
            .unwrap();
        assert_eq!(
            flow.request().unwrap(),
            ("SetCurrentProgramScene", json!({"sceneName":"Live"}))
        );
    }
    /// Drives `flow` through scripted answers and returns the requests it made.
    fn run(flow: &mut Workflow, answers: &[Value]) -> Vec<String> {
        let mut made = vec![];
        for answer in answers {
            let (request, _) = flow.request().expect("a request to answer");
            made.push(request.to_owned());
            flow.accept(answer.clone()).unwrap();
        }
        made
    }
    #[test]
    fn outputs_act_from_the_state_they_read_and_confirm_it_after() {
        let active = |on: bool| json!({"outputActive":on,"outputPaused":false});
        for (action, operation, before, request) in [
            ("record", "toggle", false, "StartRecord"),
            ("record", "toggle", true, "StopRecord"),
            ("record", "start", false, "StartRecord"),
            ("stream", "stop", true, "StopStream"),
            ("virtualcam", "toggle", false, "StartVirtualCam"),
            ("replay", "toggle", true, "StopReplayBuffer"),
        ] {
            let mut flow =
                Workflow::action(action, &params(json!({ "operation": operation }))).unwrap();
            let made = run(&mut flow, &[active(before), json!({})]);
            assert_eq!(made[1], request, "{action} {operation}");
            // The acknowledgement alone is not success: the state must show the effect.
            flow.accept(active(before)).unwrap();
            assert!(flow.request().is_some(), "{action}: ACK without state");
            flow.accept(active(!before)).unwrap();
            assert!(flow.request().is_none(), "{action}: confirmed");
        }
    }
    #[test]
    fn an_output_already_as_asked_sends_no_request() {
        let mut flow = Workflow::action("record", &params(json!({"operation":"start"}))).unwrap();
        flow.accept(json!({"outputActive":true,"outputPaused":false}))
            .unwrap();
        assert!(flow.request().is_none(), "nothing left to do");
    }
    #[test]
    fn pause_needs_a_running_recording_and_toggles_its_pause() {
        let mut flow = Workflow::action("pause", &params(json!({"operation":"toggle"}))).unwrap();
        assert_eq!(
            flow.accept(json!({"outputActive":false,"outputPaused":false})),
            Err(FailureCode::Unavailable)
        );
        let mut flow = Workflow::action("pause", &params(json!({"operation":"toggle"}))).unwrap();
        flow.accept(json!({"outputActive":true,"outputPaused":true}))
            .unwrap();
        assert_eq!(flow.request().unwrap().0, "ResumeRecord");
    }
    #[test]
    fn a_saved_replay_is_confirmed_by_a_new_path_and_needs_a_running_buffer() {
        let save = params(json!({"operation":"save"}));
        let mut flow = Workflow::action("replay", &save).unwrap();
        assert_eq!(
            flow.accept(json!({"outputActive":false})),
            Err(FailureCode::Unavailable)
        );
        for before in [None, Some("/v/old.mkv")] {
            let mut flow = Workflow::action("replay", &save).unwrap();
            flow.accept(json!({"outputActive":true})).unwrap();
            match before {
                None => flow.reject().unwrap(),
                Some(path) => flow.accept(json!({ "savedReplayPath": path })).unwrap(),
            }
            assert_eq!(flow.request().unwrap().0, "SaveReplayBuffer");
            flow.accept(json!({})).unwrap();
            let same = before.map_or(Value::Null, |p| json!(p));
            flow.accept(json!({ "savedReplayPath": same })).unwrap();
            assert!(
                flow.request().is_some(),
                "the old replay is not the new one"
            );
            flow.accept(json!({"savedReplayPath":"/v/new.mkv"}))
                .unwrap();
            assert!(flow.request().is_none());
        }
        // Only the lookup of the previous replay may fail.
        let mut flow = Workflow::action("replay", &save).unwrap();
        assert_eq!(flow.reject(), Err(FailureCode::Failed));
    }
    #[test]
    fn item_requires_exact_container_id_and_source_before_effect() {
        let value =
            r#"{"v":1,"collection":"main","kind":"group","name":"G","item":8,"source":"camera"}"#;
        let p = params(json!({"item":value,"operation":"show"}));
        let mut f = Workflow::action("item", &p).unwrap();
        f.accept(json!({"currentSceneCollectionName":"main"}))
            .unwrap();
        assert_eq!(
            f.request().unwrap(),
            ("GetGroupSceneItemList", json!({"sceneName":"G"}))
        );
        assert_eq!(
            f.accept(json!({"sceneItems":[{"sceneItemId":8,"sourceName":"other"}]})),
            Err(FailureCode::Unavailable)
        );
    }
    #[test]
    fn toggles_read_then_invert_only_the_selected_target() {
        let item =
            r#"{"v":1,"collection":"main","kind":"scene","name":"S","item":3,"source":"camera"}"#;
        for (action, values, list, read, set) in [
            (
                "filter",
                json!({"source":CAMERA,"filter":"Blur","operation":"toggle"}),
                None,
                ("filterEnabled", true),
                (
                    "SetSourceFilterEnabled",
                    json!({"sourceName":"camera","filterName":"Blur","filterEnabled":false}),
                ),
            ),
            (
                "item",
                json!({"item":item,"operation":"toggle"}),
                Some(json!({"sceneItems":[{"sceneItemId":3,"sourceName":"camera"}]})),
                ("sceneItemEnabled", false),
                (
                    "SetSceneItemEnabled",
                    json!({"sceneName":"S","sceneItemId":3,"sceneItemEnabled":true}),
                ),
            ),
            (
                "mute",
                json!({"input":CAMERA,"operation":"toggle"}),
                None,
                ("inputMuted", false),
                (
                    "SetInputMute",
                    json!({"inputName":"camera","inputMuted":true}),
                ),
            ),
        ] {
            let mut f = Workflow::action(action, &params(values)).unwrap();
            f.accept(json!({"currentSceneCollectionName":"main"}))
                .unwrap();
            if let Some(list) = list {
                f.accept(list).unwrap();
            }
            f.accept(json!({ read.0: read.1 })).unwrap();
            assert_eq!(f.request().unwrap(), set, "{action}");
            f.accept(json!({})).unwrap();
            f.accept(json!({ read.0: !read.1 })).unwrap();
            assert!(f.request().is_none(), "{action} confirmed");
        }
    }
    #[test]
    fn each_action_accepts_only_its_own_operations() {
        for (action, operation) in [
            ("record", "pause"),
            ("pause", "start"),
            ("item", "on"),
            ("mute", "hide"),
            ("stream", "save"),
            ("virtualcam", ""),
        ] {
            assert!(
                Workflow::action(action, &params(json!({ "operation": operation }))).is_err(),
                "{action} {operation}"
            );
        }
    }
}
