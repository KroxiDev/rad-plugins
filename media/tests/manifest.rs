//! `manifest.json` is what the host reads before it starts the plugin; the code must agree with it.
mod support;
use rad_plugin_media::config;
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
    assert_eq!(manifest["id"], "org.rad.media");
    assert_eq!(manifest["version"], json!({"major":0,"minor":3,"patch":0}));
    assert_eq!(
        (
            config.version.major,
            config.version.minor,
            config.version.patch
        ),
        (0, 3, 0)
    );
    assert_eq!(manifest["host_min"], *config.host_api.start());
    assert_eq!(manifest["host_max"], *config.host_api.end());
    assert_eq!(manifest["host_min"], 1);
    assert!(
        manifest["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "dynamic_options")
    );
    assert_eq!(manifest["targets"], json!(["x86_64-unknown-linux-gnu"]));
}

#[test]
fn the_action_and_the_visual_have_the_documented_fields() {
    let manifest = manifest();
    let player = json!({"id":"player","label":"Player","required":true,
        "kind":{"type":"dynamic_choice","depends_on":[]}});
    let option = |value: &str, label: &str| json!({"value":value,"label":label});
    let operation = json!({"id":"operation","label":"When pressed","required":true,
        "kind":{"type":"choice","options":[option("play_pause","Play or pause"),
            option("play","Play"),option("pause","Pause"),option("stop","Stop"),
            option("previous","Previous"),option("next","Next")]}});

    let actions = manifest["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(
        (&actions[0]["id"], &actions[0]["label"]),
        (&json!("control"), &json!("Media control"))
    );
    assert_eq!(
        actions[0]["schema"],
        json!({"version":1,"fields":[player, operation]})
    );
    assert_eq!(actions[0]["credential_slots"], json!([]));

    let visuals = manifest["visuals"].as_array().unwrap();
    assert_eq!(visuals.len(), 1);
    assert_eq!(
        (&visuals[0]["id"], &visuals[0]["label"]),
        (&json!("now_playing"), &json!("Now playing"))
    );
    assert_eq!(visuals[0]["schema"], json!({"version":1,"fields":[player]}));
    assert_eq!(visuals[0]["credential_slots"], json!([]));
}

#[test]
fn the_operations_the_manifest_offers_are_the_ones_the_plugin_accepts() {
    // Each declared operation runs against a fake player, and the dynamic field is answered for
    // exactly the declared definitions.
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.strawberry");
    let mut host = Host::start(Some(bus.address()), quick());
    let operations = manifest()["actions"][0]["schema"]["fields"][1]["kind"]["options"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(operations.len(), 6);
    for operation in &operations {
        assert_eq!(
            host.invoke(operation["value"].as_str().unwrap(), "strawberry"),
            json!({"status":"success"}),
            "{operation}"
        );
    }
    assert_eq!(player.calls().len(), 6);
    for (scope, definition) in [("action", "control"), ("visual", "now_playing")] {
        assert_eq!(
            host.options(scope, definition, "player")["availability"],
            "available"
        );
    }
    host.finish().unwrap();
}
