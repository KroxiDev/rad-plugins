//! Binary observed states and their action bindings are a negotiated public contract.
use rad_plugin_contract::{Document, Manifest};
use serde_json::{Value, json};
fn fixture() -> Value {
    json!({"contract":1,"capabilities":["visual_freshness","visual_states"],"id":"org.rad.fixture",
      "version":{"major":1,"minor":0,"patch":0},"host_min":1,"host_max":1,"targets":["x86_64-unknown-linux-gnu"],
      "actions":[{"id":"mute","label":"Mute","visual":"mute","schema":{"version":1,"fields":[]},"credential_slots":[]}],
      "visuals":[{"id":"mute","label":"Mute","states":[{"id":"muted","label":"Muted"},{"id":"unmuted","label":"Unmuted"}],"schema":{"version":1,"fields":[]},"credential_slots":[]}]})
}
fn parse(v: &Value) -> Result<Manifest, rad_plugin_contract::Error> {
    Document::new(&serde_json::to_vec(v).unwrap())
        .unwrap()
        .manifest(1, "x86_64-unknown-linux-gnu")
}
#[test]
fn binary_states_and_action_binding_round_trip_strictly() {
    let source = fixture();
    let manifest = parse(&source).expect("valid binary states");
    let value = serde_json::to_value(manifest).unwrap();
    assert_eq!(
        value["visuals"][0]["states"],
        source["visuals"][0]["states"]
    );
    for (path, value) in [
        ("/capabilities", json!(["visual_freshness"])),
        ("/actions/0/visual", json!("missing")),
        ("/visuals/0/states", json!([])),
        ("/visuals/0/states", json!([{"id":"one","label":"One"}])),
        ("/visuals/0/states/1/id", json!("muted")),
        ("/visuals/0/states/0/icon", json!("not-png")),
    ] {
        let mut bad = source.clone();
        if path.ends_with("/icon") {
            bad["visuals"][0]["states"][0]["icon"] = value;
        } else {
            *bad.pointer_mut(path).unwrap() = value;
        }
        assert!(parse(&bad).is_err(), "{path}");
    }
}

#[test]
fn state_replies_require_negotiation_and_declared_identifiers() {
    use rad_plugin_contract::*;
    let manifest = parse(&fixture()).unwrap();
    let state = VisualState::Current {
        content: VisualContent {
            state: Some("muted".into()),
            ..Default::default()
        },
    };
    let message = Message::Visual {
        valid_for_ms: 25000,
        state: state.clone(),
    };
    assert!(state.is_valid());
    assert!(!message.permitted(&[VISUAL_FRESHNESS.into()]));
    assert!(message.permitted(&[VISUAL_FRESHNESS.into(), VISUAL_STATES.into()]));
    assert!(manifest.visuals[0].accepts_state(&state));
    assert!(!manifest.visuals[0].accepts_state(&VisualState::Current {
        content: VisualContent {
            state: Some("other".into()),
            ..Default::default()
        }
    }));
}
