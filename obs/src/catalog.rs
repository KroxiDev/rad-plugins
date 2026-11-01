//! Bounded option enumeration; values identify a container, never a label match.
use crate::workflow::{Target, target};
use rad_plugin_sdk::{ChoiceList, ChoiceOption, FailureCode, Parameters};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
struct Query {
    name: &'static str,
    data: Value,
    depth: usize,
}
pub(crate) struct Catalog {
    field: String,
    target: Option<Target>,
    collection: String,
    queue: VecDeque<Query>,
    options: Vec<ChoiceOption>,
    groups: BTreeSet<String>,
}
impl Catalog {
    pub fn new(field: &str, context: &Parameters) -> Result<Self, FailureCode> {
        let t = match field {
            "item" => Some(target(
                context
                    .text("scene")
                    .ok_or(FailureCode::InvalidParameters)?,
            )?),
            "filter" => Some(target(
                context
                    .text("source")
                    .ok_or(FailureCode::InvalidParameters)?,
            )?),
            "source" | "scene" | "input" => None,
            _ => return Err(FailureCode::InvalidParameters),
        };
        Ok(Self {
            field: field.into(),
            target: t,
            collection: String::new(),
            queue: [Query {
                name: "GetSceneCollectionList",
                data: json!({}),
                depth: 0,
            }]
            .into(),
            options: vec![],
            groups: BTreeSet::new(),
        })
    }
    pub fn request(&self) -> Option<(&str, Value)> {
        self.queue.front().map(|q| (q.name, q.data.clone()))
    }
    fn push(&mut self, name: &'static str, data: Value, depth: usize) {
        self.queue.push_back(Query { name, data, depth });
    }
    fn add(&mut self, t: Target, label: String) -> Result<(), FailureCode> {
        let value = serde_json::to_string(&t).map_err(|_| FailureCode::Failed)?;
        self.add_value(value, label)
    }
    fn add_value(&mut self, value: String, label: String) -> Result<(), FailureCode> {
        if !rad_plugin_sdk::is_label(&value) || !rad_plugin_sdk::is_label(&label) {
            return Err(FailureCode::Unavailable);
        }
        if !self.options.iter().any(|o| o.value == value) {
            self.options.push(ChoiceOption { value, label });
        }
        if self.options.len() > 64 {
            return Err(FailureCode::Unavailable);
        }
        Ok(())
    }
    pub fn accept(&mut self, data: Value) -> Result<(), FailureCode> {
        let q = self.queue.pop_front().ok_or(FailureCode::Failed)?;
        if q.name == "GetSceneCollectionList" {
            self.collection = data["currentSceneCollectionName"]
                .as_str()
                .ok_or(FailureCode::Failed)?
                .into();
            if self
                .target
                .as_ref()
                .is_some_and(|t| t.collection != self.collection)
            {
                return Err(FailureCode::Unavailable);
            }
            match self.field.as_str() {
                "scene" => self.push("GetSceneList", json!({}), 0),
                "input" => self.push("GetInputList", json!({}), 0),
                "source" => {
                    self.push("GetInputList", json!({}), 0);
                    self.push("GetSceneList", json!({}), 0);
                    self.push("GetGroupList", json!({}), 0);
                }
                "filter" => self.push(
                    "GetSourceFilterList",
                    json!({"sourceName":self.target.as_ref().unwrap().name}),
                    0,
                ),
                "item" => {
                    let t = self.target.as_ref().unwrap();
                    self.push(
                        if t.kind == "group" {
                            "GetGroupSceneItemList"
                        } else {
                            "GetSceneItemList"
                        },
                        json!({"sceneName":t.name}),
                        0,
                    );
                }
                _ => return Err(FailureCode::InvalidParameters),
            }
        } else if ["GetSceneItemList", "GetGroupSceneItemList"].contains(&q.name) {
            let container = q.data["sceneName"].as_str().ok_or(FailureCode::Failed)?;
            for item in data["sceneItems"].as_array().ok_or(FailureCode::Failed)? {
                let source = item["sourceName"].as_str().ok_or(FailureCode::Failed)?;
                let id = item["sceneItemId"]
                    .as_i64()
                    .filter(|v| *v >= 0)
                    .ok_or(FailureCode::Failed)?;
                self.add(
                    Target {
                        v: 1,
                        collection: self.collection.clone(),
                        kind: if q.name == "GetGroupSceneItemList" {
                            "group"
                        } else {
                            "scene"
                        }
                        .into(),
                        name: container.into(),
                        item: Some(id),
                        source: Some(source.into()),
                    },
                    // A group's children say which group; an item of the scene is its source.
                    if q.name == "GetGroupSceneItemList" {
                        format!("{container} / {source}")
                    } else {
                        source.into()
                    },
                )?;
                if item["isGroup"] == true {
                    if q.depth >= 16 || !self.groups.insert(source.into()) {
                        return Err(FailureCode::Unavailable);
                    }
                    self.push(
                        "GetGroupSceneItemList",
                        json!({"sceneName":source}),
                        q.depth + 1,
                    );
                }
            }
        } else if q.name == "GetSourceFilterList" {
            for f in data["filters"].as_array().ok_or(FailureCode::Failed)? {
                let name = f["filterName"].as_str().ok_or(FailureCode::Failed)?;
                self.add_value(name.into(), name.into())?;
            }
        } else {
            let (key, namekey, kind) = match q.name {
                "GetSceneList" => ("scenes", "sceneName", "scene"),
                "GetInputList" => ("inputs", "inputName", "input"),
                "GetGroupList" => ("groups", "", "group"),
                _ => return Err(FailureCode::Failed),
            };
            for entry in data[key].as_array().ok_or(FailureCode::Failed)? {
                let name = if namekey.is_empty() {
                    entry.as_str()
                } else {
                    entry[namekey].as_str()
                }
                .ok_or(FailureCode::Failed)?;
                self.add(
                    Target {
                        v: 1,
                        collection: self.collection.clone(),
                        kind: kind.into(),
                        name: name.into(),
                        item: None,
                        source: None,
                    },
                    // OBS names are unique across inputs, scenes and groups.
                    name.into(),
                )?;
            }
        }
        Ok(())
    }
    pub fn result(&self) -> ChoiceList {
        if !self.queue.is_empty() {
            return ChoiceList::Unavailable;
        }
        // The same source added twice to a scene: only its number tells the two apart.
        let repeated = |label: &str| self.options.iter().filter(|o| o.label == label).count() > 1;
        let mut options = self.options.clone();
        for option in &mut options {
            if repeated(&option.label)
                && let Ok(Target { item: Some(id), .. }) = target(&option.value)
            {
                option.label = format!("{} [{id}]", option.label);
            }
        }
        ChoiceList::Available { options }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn oversized_choice_catalog_or_identity_is_unavailable_not_truncated() {
        let p = Parameters {
            schema_version: 1,
            values: serde_json::Map::new(),
        };
        for names in [
            (0..65).map(|i| format!("Scene{i}")).collect::<Vec<_>>(),
            vec!["x".repeat(256)],
        ] {
            let mut c = Catalog::new("scene", &p).unwrap();
            c.accept(json!({"currentSceneCollectionName":"C"})).unwrap();
            let scenes: Vec<_> = names
                .into_iter()
                .map(|name| json!({"sceneName":name}))
                .collect();
            assert!(c.accept(json!({"scenes":scenes})).is_err());
        }
    }
    #[test]
    fn labels_are_plain_names_and_number_only_repeated_items() {
        let empty = Parameters {
            schema_version: 1,
            values: serde_json::Map::new(),
        };
        let mut c = Catalog::new("input", &empty).unwrap();
        c.accept(json!({"currentSceneCollectionName":"C"})).unwrap();
        assert_eq!(c.request().unwrap().0, "GetInputList");
        c.accept(json!({"inputs":[{"inputName":"Mic/Aux"},{"inputName":"Desktop Audio"}]}))
            .unwrap();
        let ChoiceList::Available { options } = c.result() else {
            panic!("missing options")
        };
        let labels: Vec<_> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Mic/Aux", "Desktop Audio"]);
        assert_eq!(
            crate::workflow::target(&options[0].value).unwrap().kind,
            "input"
        );

        let scene =
            r#"{"v":1,"collection":"C","kind":"scene","name":"Root","item":null,"source":null}"#;
        let p = Parameters {
            schema_version: 1,
            values: json!({"scene":scene}).as_object().unwrap().clone(),
        };
        let mut c = Catalog::new("item", &p).unwrap();
        c.accept(json!({"currentSceneCollectionName":"C"})).unwrap();
        c.accept(json!({"sceneItems":[
            {"sceneItemId":1,"sourceName":"Camera","isGroup":false},
            {"sceneItemId":2,"sourceName":"Logo","isGroup":false},
            {"sceneItemId":5,"sourceName":"Logo","isGroup":false}]}))
            .unwrap();
        let ChoiceList::Available { options } = c.result() else {
            panic!("missing options")
        };
        let labels: Vec<_> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Camera", "Logo [2]", "Logo [5]"]);
    }
    #[test]
    fn group_children_are_addressed_inside_group_not_root() {
        let scene =
            r#"{"v":1,"collection":"C","kind":"scene","name":"Root","item":null,"source":null}"#;
        let p = Parameters {
            schema_version: 1,
            values: json!({"scene":scene}).as_object().unwrap().clone(),
        };
        let mut c = Catalog::new("item", &p).unwrap();
        c.accept(json!({"currentSceneCollectionName":"C"})).unwrap();
        c.accept(json!({"sceneItems":[{"sceneItemId":1,"sourceName":"G","isGroup":true}]}))
            .unwrap();
        assert_eq!(
            c.request().unwrap(),
            ("GetGroupSceneItemList", json!({"sceneName":"G"}))
        );
        c.accept(json!({"sceneItems":[{"sceneItemId":2,"sourceName":"Camera","isGroup":false}]}))
            .unwrap();
        let ChoiceList::Available { options } = c.result() else {
            panic!("missing options")
        };
        assert_eq!(options.len(), 2);
        let t = crate::workflow::target(&options[1].value).unwrap();
        assert_eq!(t.name, "G");
        assert_eq!(t.item, Some(2));
    }
}
