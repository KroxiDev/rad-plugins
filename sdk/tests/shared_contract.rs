use rad_plugin_sdk::Outcome;
#[test]
fn rejects_unknown_outcome_fields() {
    assert!(serde_json::from_str::<Outcome>(r#"{"status":"success","unexpected":true}"#).is_err());
}
#[test]
fn sdk_consumes_host_reference_vectors_as_identical_types() {
    use rad_plugin_sdk::Envelope;
    assert_eq!(
        std::any::TypeId::of::<Envelope>(),
        std::any::TypeId::of::<rad_plugin_contract::Envelope>()
    );
    let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../crates/rad-plugin-contract/vectors/v1.json"
    ))
    .unwrap();
    for value in vectors {
        let envelope: Envelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(envelope).unwrap(), value);
    }
}
#[test]
fn sdk_rejects_host_negative_vectors_as_identical_types() {
    let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../crates/rad-plugin-contract/vectors/v1-invalid.json"
    ))
    .unwrap();
    for value in vectors {
        assert!(serde_json::from_value::<rad_plugin_sdk::Envelope>(value).is_err());
    }
}
