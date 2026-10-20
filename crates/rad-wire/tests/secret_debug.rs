#[test]
fn frames_and_requests_never_render_payload_in_debug() {
    fn frame(json: &[u8]) -> Vec<u8> {
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        bytes
    }
    let hello = frame(br#"{"v":1,"id":"1","op":"hello","args":{}}"#);
    let mut session = rad_wire::Session::default();
    session
        .accept(rad_wire::read_frame(&hello).unwrap().unwrap())
        .unwrap();
    let bytes = frame(br#"{"v":1,"id":"2","op":"credential_replace","args":{"plugin_id":"fixture","slot":"password","secret":"wire-canary"}}"#);
    let frame = rad_wire::read_frame(&bytes).unwrap().unwrap();
    let frame_debug = format!("{frame:?}");
    assert!(
        !frame_debug.contains("119, 105, 114, 101, 45, 99, 97, 110, 97, 114, 121"),
        "raw UTF-8 bytes must not be logged"
    );
    let request = session.accept(frame).unwrap();
    assert!(!format!("{request:?}").contains("wire-canary"));
}
