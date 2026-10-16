//! `manifest.json` is what the host reads before it starts the plugin; the code must agree with it.
mod support;
use rad_plugin_audio::{Kind, config};
use serde_json::{Value, json};
use support::*;

fn manifest() -> Value {
    serde_json::from_str(include_str!("../manifest.json")).expect("manifest.json is JSON")
}

#[test]
fn the_manifest_and_the_code_declare_the_same_identity_and_host_api() {
    let manifest = manifest();
    let config = config();
    assert_eq!(manifest["contract"], 1);
    assert_eq!(manifest["id"], config.id);
    assert_eq!(manifest["version"], json!({"major":0,"minor":1,"patch":0}));
    assert_eq!(config.version.major, 0);
    assert_eq!((config.version.minor, config.version.patch), (1, 0));
    assert_eq!(manifest["host_min"], *config.host_api.start());
    assert_eq!(manifest["host_max"], *config.host_api.end());
    assert_eq!(manifest["host_min"], 2, "dynamic_choice needs host API 2");
    assert_eq!(manifest["targets"], json!(["x86_64-unknown-linux-gnu"]));
}

#[test]
fn every_definition_has_the_documented_fields() {
    let manifest = manifest();
    let operation = json!({"id":"operation","label":"Operation","required":true,
        "kind":{"type":"choice","options":["toggle","mute","unmute"]}});
    let device = json!({"id":"device","label":"Device","required":true,
        "kind":{"type":"dynamic_choice","depends_on":[]}});
    let labels = [
        ("actions", "output", "Audio output"),
        ("actions", "microphone", "Microphone"),
        ("visuals", "output", "Audio output state"),
        ("visuals", "microphone", "Microphone state"),
    ];
    assert_eq!(manifest["actions"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["visuals"].as_array().unwrap().len(), 2);
    for (category, id, label) in labels {
        let definition = manifest[category]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == id)
            .unwrap_or_else(|| panic!("{category}/{id} is declared"));
        assert_eq!(definition["label"], label);
        assert_eq!(definition["credential_slots"], json!([]));
        assert_eq!(definition["schema"]["version"], 1);
        let expected = if category == "actions" {
            json!([operation, device])
        } else {
            json!([device])
        };
        assert_eq!(definition["schema"]["fields"], expected, "{category}/{id}");
        assert!(
            Kind::from_definition(id).is_some(),
            "the plugin implements {id}"
        );
    }
}

#[test]
fn the_values_the_manifest_offers_are_the_ones_the_plugin_accepts() {
    // Each declared operation is a valid action; the dynamic `device` field is answered with
    // options for exactly the declared definitions and field.
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    for operation in manifest()["actions"][0]["schema"]["fields"][0]["kind"]["options"]
        .as_array()
        .unwrap()
    {
        let outcome = host.invoke("output", operation.as_str().unwrap(), "@default");
        assert_eq!(outcome, json!({"status":"success"}), "{operation}");
    }
    for (scope, definition) in [
        ("action", "output"),
        ("action", "microphone"),
        ("visual", "output"),
        ("visual", "microphone"),
    ] {
        assert_eq!(
            host.options(scope, definition, "device")["availability"],
            "available"
        );
    }
    host.finish().unwrap();
}
