//! Unit alternatives must retain the same closed schema as struct alternatives.
use rad_plugin_contract::{ChoiceList, FieldKind, VisualState};
#[test]
fn unit_alternatives_reject_unknown_fields() {
    assert!(
        serde_json::from_str::<VisualState>(r#"{"availability":"unavailable","extra":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<ChoiceList>(r#"{"availability":"unavailable","extra":true}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<FieldKind>(r#"{"type":"boolean","extra":true}"#).is_err());
}
#[test]
fn wire_rejects_shared_negative_vectors() {
    let vectors: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../vectors/v1-invalid.json")).unwrap();
    for value in vectors {
        let bytes = serde_json::to_vec(&value).unwrap();
        let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
        frame.extend(bytes);
        assert!(rad_plugin_contract::decode(&frame).is_err());
    }
}
