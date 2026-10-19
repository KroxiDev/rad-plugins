use rad_plugin_sdk::{FailureCode, Parameters};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;

#[derive(Clone)]
struct Call {
    name: &'static str,
    data: Value,
}
pub(crate) struct Workflow {
    calls: std::collections::VecDeque<Call>,
    target: Option<Target>,
    verify: Option<(&'static str, Value)>,
    pub result: Value,
}
impl Workflow {
    pub fn visual(name: &str, params: &Parameters) -> Result<Self, FailureCode> {
        if name == "record" || name == "stream" {
            return Ok(Self {
                calls: [Call {
                    name: if name == "record" {
                        "GetRecordStatus"
                    } else {
                        "GetStreamStatus"
                    },
                    data: json!({}),
                }]
                .into(),
                target: None,
                verify: None,
                result: Value::Null,
            });
        }
        let mut p = params.clone();
        p.values.insert(
            "operation".into(),
            json!(if name == "item" { "show" } else { "on" }),
        );
        let mut flow = Self::action(name, &p)?;
        flow.calls.retain(|c| c.name.starts_with("Get"));
        flow.verify = None;
        if name == "scene" {
            flow.calls.insert(
                1,
                Call {
                    name: "GetSceneList",
                    data: json!({}),
                },
            );
        }
        if name == "filter" {
            flow.calls.pop_back();
        }
        Ok(flow)
    }

    pub fn action(name: &str, params: &Parameters) -> Result<Self, FailureCode> {
        if name == "record" || name == "stream" {
            let op = params
                .text("operation")
                .ok_or(FailureCode::InvalidParameters)?;
            let (request, field, want) = match (name, op) {
                ("record", "start") => ("StartRecord", "outputActive", true),
                ("record", "stop") => ("StopRecord", "outputActive", false),
                ("record", "pause") => ("PauseRecord", "outputPaused", true),
                ("record", "resume") => ("ResumeRecord", "outputPaused", false),
                ("stream", "start") => ("StartStream", "outputActive", true),
                ("stream", "stop") => ("StopStream", "outputActive", false),
                _ => return Err(FailureCode::InvalidParameters),
            };
            let status = if name == "record" {
                "GetRecordStatus"
            } else {
                "GetStreamStatus"
            };
            return Ok(Self {
                calls: [
                    Call {
                        name: request,
                        data: json!({}),
                    },
                    Call {
                        name: status,
                        data: json!({}),
                    },
                ]
                .into(),
                target: None,
                verify: Some((field, json!(want))),
                result: Value::Null,
            });
        }
        if name == "filter" {
            let t = target(
                params
                    .text("source")
                    .ok_or(FailureCode::InvalidParameters)?,
            )?;
            let filter = params
                .text("filter")
                .filter(|v| rad_plugin_sdk::is_label(v))
                .ok_or(FailureCode::InvalidParameters)?;
            let desired = match params.text("operation") {
                Some("on") => json!(true),
                Some("off") => json!(false),
                Some("toggle") => Value::Null,
                _ => return Err(FailureCode::InvalidParameters),
            };
            let query = json!({"sourceName":t.name,"filterName":filter});
            let mut set = query.clone();
            set["filterEnabled"] = desired.clone();
            return Ok(Self {
                calls: [
                    Call {
                        name: "GetSceneCollectionList",
                        data: json!({}),
                    },
                    Call {
                        name: "GetSourceFilter",
                        data: query.clone(),
                    },
                    Call {
                        name: "SetSourceFilterEnabled",
                        data: set,
                    },
                    Call {
                        name: "GetSourceFilter",
                        data: query,
                    },
                ]
                .into(),
                target: Some(t),
                verify: Some(("filterEnabled", desired)),
                result: Value::Null,
            });
        }
        if name == "item" {
            let t = target(params.text("item").ok_or(FailureCode::InvalidParameters)?)?;
            if t.item.is_none() || t.kind == "input" {
                return Err(FailureCode::InvalidParameters);
            }
            let enabled = match params.text("operation") {
                Some("show") => true,
                Some("hide") => false,
                _ => return Err(FailureCode::InvalidParameters),
            };
            let list = if t.kind == "group" {
                "GetGroupSceneItemList"
            } else {
                "GetSceneItemList"
            };
            return Ok(Self{calls:[Call{name:"GetSceneCollectionList",data:json!({})},Call{name:list,data:json!({"sceneName":t.name})},Call{name:"SetSceneItemEnabled",data:json!({"sceneName":t.name,"sceneItemId":t.item,"sceneItemEnabled":enabled})},Call{name:"GetSceneItemEnabled",data:json!({"sceneName":t.name,"sceneItemId":t.item})}].into(),target:Some(t),verify:Some(("sceneItemEnabled",json!(enabled))),result:Value::Null});
        }
        if name != "scene" {
            return Err(FailureCode::InvalidParameters);
        }
        let target = target(params.text("scene").ok_or(FailureCode::InvalidParameters)?)?;
        if target.kind != "scene" || target.item.is_some() {
            return Err(FailureCode::InvalidParameters);
        }
        let calls = [
            Call {
                name: "GetSceneCollectionList",
                data: json!({}),
            },
            Call {
                name: "SetCurrentProgramScene",
                data: json!({"sceneName":target.name}),
            },
            Call {
                name: "GetCurrentProgramScene",
                data: json!({}),
            },
        ]
        .into();
        Ok(Self {
            calls,
            target: Some(target.clone()),
            verify: Some(("currentProgramSceneName", json!(target.name))),
            result: Value::Null,
        })
    }
    pub fn request(&self) -> Option<(&str, Value)> {
        self.calls.front().map(|c| (c.name, c.data.clone()))
    }
    pub fn accept(&mut self, data: Value) -> Result<(), FailureCode> {
        let call = self.calls.pop_front().ok_or(FailureCode::Failed)?;
        if call.name == "GetSceneCollectionList"
            && self
                .target
                .as_ref()
                .is_some_and(|t| data["currentSceneCollectionName"] != t.collection)
        {
            return Err(FailureCode::Unavailable);
        }
        if call.name == "GetSceneList" {
            let t = self.target.as_ref().ok_or(FailureCode::Failed)?;
            if !data["scenes"]
                .as_array()
                .ok_or(FailureCode::Failed)?
                .iter()
                .any(|v| v["sceneName"] == t.name)
            {
                return Err(FailureCode::Unavailable);
            }
        }
        if ["GetSceneItemList", "GetGroupSceneItemList"].contains(&call.name) {
            let t = self.target.as_ref().ok_or(FailureCode::Failed)?;
            if !data["sceneItems"]
                .as_array()
                .ok_or(FailureCode::Failed)?
                .iter()
                .any(|v| {
                    v["sceneItemId"].as_i64() == t.item
                        && v["sourceName"].as_str() == t.source.as_deref()
                })
            {
                return Err(FailureCode::Unavailable);
            }
        }
        if call.name == "GetSourceFilter"
            && self
                .calls
                .front()
                .is_some_and(|c| c.name == "SetSourceFilterEnabled")
        {
            let current = data["filterEnabled"].as_bool().ok_or(FailureCode::Failed)?;
            if self.verify.as_ref().is_some_and(|(_, v)| v.is_null()) {
                let want = json!(!current);
                self.calls.front_mut().unwrap().data["filterEnabled"] = want.clone();
                self.verify = Some(("filterEnabled", want));
            }
        }
        if self.calls.is_empty() {
            if let Some((field, expected)) = &self.verify
                && data[*field] != *expected
            {
                self.calls.push_front(call);
                return Ok(());
            }
            self.result = data;
        }
        Ok(())
    }
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
    #[test]
    fn deleted_scene_visual_is_unavailable_not_inactive() {
        let p = Parameters { schema_version:1, values:json!({"scene":r#"{"v":1,"collection":"C","kind":"scene","name":"Gone","item":null,"source":null}"#}).as_object().unwrap().clone() };
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
        let params = Parameters {
            schema_version: 1,
            values: json!({"scene":value}).as_object().unwrap().clone(),
        };
        let mut flow = Workflow::action("scene", &params).unwrap();
        assert_eq!(flow.request().unwrap().0, "GetSceneCollectionList");
        assert_eq!(
            flow.accept(json!({"currentSceneCollectionName":"other"})),
            Err(FailureCode::Unavailable)
        );
        let mut flow = Workflow::action("scene", &params).unwrap();
        flow.accept(json!({"currentSceneCollectionName":"main"}))
            .unwrap();
        assert_eq!(
            flow.request().unwrap(),
            ("SetCurrentProgramScene", json!({"sceneName":"Live"}))
        );
    }
    #[test]
    fn output_actions_require_observed_state_not_ack() {
        for (kind, operation, request, field, value) in [
            ("record", "start", "StartRecord", "outputActive", true),
            ("record", "stop", "StopRecord", "outputActive", false),
            ("record", "pause", "PauseRecord", "outputPaused", true),
            ("record", "resume", "ResumeRecord", "outputPaused", false),
            ("stream", "start", "StartStream", "outputActive", true),
            ("stream", "stop", "StopStream", "outputActive", false),
        ] {
            let p = Parameters {
                schema_version: 1,
                values: json!({"operation":operation}).as_object().unwrap().clone(),
            };
            let mut f = Workflow::action(kind, &p).unwrap();
            assert_eq!(f.request().unwrap().0, request);
            f.accept(json!({})).unwrap();
            assert!(f.request().is_some());
            f.accept(json!({field:!value})).unwrap();
            assert!(
                f.request().is_some(),
                "ACK without state must remain pending"
            );
        }
    }
    #[test]
    fn item_requires_exact_container_id_and_source_before_effect() {
        let value =
            r#"{"v":1,"collection":"main","kind":"group","name":"G","item":8,"source":"camera"}"#;
        let p = Parameters {
            schema_version: 1,
            values: json!({"item":value,"operation":"show"})
                .as_object()
                .unwrap()
                .clone(),
        };
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
    fn toggle_filter_reads_then_inverts_only_selected_filter() {
        let value = r#"{"v":1,"collection":"main","kind":"input","name":"camera","item":null,"source":null}"#;
        let p = Parameters {
            schema_version: 1,
            values: json!({"source":value,"filter":"Blur","operation":"toggle"})
                .as_object()
                .unwrap()
                .clone(),
        };
        let mut f = Workflow::action("filter", &p).unwrap();
        f.accept(json!({"currentSceneCollectionName":"main"}))
            .unwrap();
        assert_eq!(f.request().unwrap().0, "GetSourceFilter");
        f.accept(json!({"filterEnabled":true})).unwrap();
        assert_eq!(
            f.request().unwrap(),
            (
                "SetSourceFilterEnabled",
                json!({"sourceName":"camera","filterName":"Blur","filterEnabled":false})
            )
        );
    }
}
