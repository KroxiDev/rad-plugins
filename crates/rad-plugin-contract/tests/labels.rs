//! Plugin texts the user reads: labelled fixed options, translations and fixed response budgets.
use rad_plugin_contract::{Document, Error, FieldKind, Manifest};
use serde_json::{Value, json};

const TARGET: &str = "x86_64-unknown-linux-gnu";

fn manifest() -> Value {
    json!({"contract":1,"capabilities":["labels","response_timeout","visual_freshness"],"id":"org.rad.fixture",
        "version":{"major":1,"minor":0,"patch":0},"host_min":1,"host_max":1,"targets":[TARGET],
        "actions":[{"id":"record","label":"Recording","schema":{"version":1,"fields":[
            {"id":"operation","label":"Operation","required":true,"kind":{"type":"choice","options":[
                {"value":"toggle","label":"Toggle"},{"value":"start","label":"Start"},"stop"]}}]},
            "credential_slots":[]}],
        "visuals":[],
        "translations":{"es":{"Recording":"Grabación","Toggle":"Alternar","Operation":"Operación"}}})
}
fn parse(value: &Value) -> Result<Manifest, Error> {
    Document::new(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .manifest(1, TARGET)
}

#[test]
fn labelled_options_keep_values_apart_from_the_text_the_user_reads() {
    let manifest = parse(&manifest()).unwrap();
    let action = &manifest.actions[0];
    let FieldKind::Choice { options } = &action.schema.fields[0].kind else {
        panic!("choice field");
    };
    let shown: Vec<_> = options
        .iter()
        .map(|o| (o.value.as_str(), o.label.as_str()))
        .collect();
    // A plain option is its own label, as before the capability existed.
    assert_eq!(
        shown,
        [("toggle", "Toggle"), ("start", "Start"), ("stop", "stop")]
    );
    let stored = |value: &str| {
        let bytes =
            serde_json::to_vec(&json!({"schema_version":1,"values":{"operation":value}})).unwrap();
        Document::new(&bytes).unwrap().parameters(action)
    };
    assert_eq!(stored("toggle"), Ok(()));
    assert_eq!(stored("Toggle"), Err(Error::InvalidParameters));
}

#[test]
fn labels_and_translations_require_the_capability() {
    let mut without = manifest();
    without["capabilities"] = json!(["response_timeout", "visual_freshness"]);
    assert_eq!(parse(&without).unwrap_err(), Error::InvalidManifest);

    let mut translations_only = without.clone();
    translations_only["actions"][0]["schema"]["fields"][0]["kind"]["options"] =
        json!(["toggle", "start"]);
    assert_eq!(
        parse(&translations_only).unwrap_err(),
        Error::InvalidManifest
    );

    let mut legacy = translations_only;
    legacy.as_object_mut().unwrap().remove("translations");
    assert!(parse(&legacy).is_ok());
}

#[test]
fn translation_falls_back_to_the_plugin_text() {
    let manifest = parse(&manifest()).unwrap();
    assert_eq!(manifest.translate("es", "Recording"), "Grabación");
    assert_eq!(manifest.translate("es", "Start"), "Start");
    assert_eq!(manifest.translate("fr", "Recording"), "Recording");
    assert_eq!(manifest.translate("en", "Toggle"), "Toggle");
}

#[test]
fn malformed_translations_and_labels_are_rejected() {
    let invalid = [
        ("/translations", json!({"ES":{"Recording":"Grabación"}})),
        ("/translations", json!({"e":{"Recording":"Grabación"}})),
        ("/translations", json!({"es":{"Recording":""}})),
        ("/translations", json!({"es":{"Recording":"a\nb"}})),
        ("/translations", json!({"es":{"":"Grabación"}})),
        (
            "/actions/0/schema/fields/0/kind/options",
            json!([{"value":"toggle","label":""}]),
        ),
        (
            "/actions/0/schema/fields/0/kind/options",
            json!([{"value":"toggle","label":"A"},{"value":"toggle","label":"B"}]),
        ),
        (
            "/actions/0/schema/fields/0/kind/options",
            json!([{"value":"toggle","label":"A","extra":true}]),
        ),
    ];
    for (pointer, value) in invalid {
        let mut v = manifest();
        *v.pointer_mut(pointer).unwrap() = value.clone();
        assert!(parse(&v).is_err(), "{pointer} = {value}");
    }
    let mut many = manifest();
    many["translations"] = (0..17)
        .map(|i| {
            let language: String = [b'a' + i / 26, b'a' + i % 26]
                .iter()
                .map(|b| *b as char)
                .collect();
            (language, json!({"Recording":"x"}))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    assert_eq!(parse(&many).unwrap_err(), Error::InvalidManifest);
}
