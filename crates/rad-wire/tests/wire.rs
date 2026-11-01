use rad_wire::*;
fn frame(payload: &[u8]) -> Vec<u8> {
    [(payload.len() as u32).to_be_bytes().as_slice(), payload].concat()
}
#[test]
fn partial_headers_payload_and_coalesced_frames_consume_exactly_one() {
    let first = frame(br#"{"v":1,"id":"1","op":"hello","args":{}}"#);
    for n in 0..first.len() {
        assert!(read_frame(&first[..n]).unwrap().is_none());
    }
    let second = frame(b"{}");
    let joined = [first.as_slice(), second.as_slice()].concat();
    let parsed = read_frame(&joined).unwrap().unwrap();
    assert_eq!(parsed.payload(), &first[4..]);
    assert_eq!(parsed.consumed(), first.len());
    let rest = read_frame(&joined[parsed.consumed()..]).unwrap().unwrap();
    assert_eq!(rest.payload(), b"{}");
    assert_eq!(rest.consumed(), second.len());
}
#[test]
fn length_cap_is_inclusive_and_invalid_header_rejected_without_payload() {
    assert_eq!(
        read_frame(&0u32.to_be_bytes()).unwrap_err(),
        FrameError::InvalidLength
    );
    for n in [65537, u32::MAX] {
        assert_eq!(
            read_frame(&n.to_be_bytes()).unwrap_err(),
            FrameError::InvalidLength
        );
    }
    for n in [1, 65536] {
        let bytes = frame(&vec![b' '; n]);
        assert_eq!(read_frame(&bytes).unwrap().unwrap().payload().len(), n);
        assert!(read_frame(&bytes[..bytes.len() - 1]).unwrap().is_none());
    }
}
fn request(session: &mut Session, json: &[u8]) -> Result<Request, RequestError> {
    let bytes = frame(json);
    session.accept(read_frame(&bytes).unwrap().unwrap())
}
const HELLO: &[u8] = br#"{"v":1,"id":"1","op":"hello","args":{}}"#;

#[test]
fn duplicates_at_root_nested_and_escaped_equivalent_keys_are_rejected() {
    for bad in [
        br#"{"v":1,"v":1,"id":"1","op":"hello","args":{}}"#.as_slice(),
        br#"{"v":1,"id":"1","op":"hello","args":{"a":1,"a":2}}"#,
        br#"{"v":1,"id":"1","op":"hello","args":{"x":[{"a":1,"\u0061":2}]}}"#,
    ] {
        assert_eq!(
            request(&mut Session::default(), bad).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}
fn after_hello(json: &str) -> Result<Request, RequestError> {
    let mut s = Session::default();
    request(&mut s, HELLO).unwrap();
    request(&mut s, json.as_bytes())
}

#[test]
fn strings_and_keys_limit_decoded_utf8_bytes_including_escapes() {
    for (value, ok) in [("é".repeat(8192), true), ("é".repeat(8192) + "x", false)] {
        let json = format!(r#"{{"v":1,"id":"2","op":"custom","args":{{"x":"{value}"}}}}"#);
        assert_eq!(after_hello(&json).is_ok(), ok);
    }
    for (n, ok) in [(16384, true), (16385, false)] {
        let json = format!(
            r#"{{"v":1,"id":"2","op":"custom","args":{{"{}":null}}}}"#,
            "x".repeat(n)
        );
        assert_eq!(after_hello(&json).is_ok(), ok);
        let value = "a".repeat(n - 1) + r"\u0061";
        let json = format!(r#"{{"v":1,"id":"2","op":"custom","args":{{"x":"{value}"}}}}"#);
        assert_eq!(after_hello(&json).is_ok(), ok);
    }
}

#[test]
fn envelope_is_closed_and_version_mismatch_requests_close() {
    for bad in [
        br#"{"v":1,"id":"1","op":"hello","args":{},"client_role":"editor"}"#.as_slice(),
        br#"{"id":"1","op":"hello","args":{}}"#,
        br#"{"v":1.0,"id":"1","op":"hello","args":{}}"#,
        br#"{"v":1,"id":"1","op":"","args":{}}"#,
    ] {
        assert_eq!(
            request(&mut Session::default(), bad).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
    let error = request(
        &mut Session::default(),
        br#"{"v":2,"id":"1","op":"hello","args":{}}"#,
    )
    .unwrap_err();
    assert_eq!(
        error,
        RequestError {
            code: ErrorCode::UnsupportedVersion,
            id: Some(1),
            close: true
        }
    );
}
#[test]
fn canonical_positive_monotonic_ids_accept_max_once_and_never_wrap() {
    for id in [
        "0",
        "01",
        "+1",
        "-1",
        " 1",
        "1.0",
        "1e0",
        "18446744073709551616",
    ] {
        let json = format!(r#"{{"v":1,"id":"{id}","op":"hello","args":{{}}}}"#);
        assert!(
            request(&mut Session::default(), json.as_bytes()).is_err(),
            "{id}"
        );
    }
    let mut s = Session::default();
    request(&mut s, HELLO).unwrap();
    let max = br#"{"v":1,"id":"18446744073709551615","op":"status","args":{}}"#;
    assert_eq!(request(&mut s, max).unwrap().id(), u64::MAX);
    let error = request(&mut s, max).unwrap_err();
    assert!(error.close);
    assert_eq!(error.id, Some(u64::MAX));
}
#[test]
fn hello_must_be_first_exact_id_one_empty_args_and_cannot_repeat() {
    for bad in [
        br#"{"v":1,"id":"1","op":"status","args":{}}"#.as_slice(),
        br#"{"v":1,"id":"2","op":"hello","args":{}}"#,
        br#"{"v":1,"id":"1","op":"hello","args":{"x":0}}"#,
        br#"{"v":1,"id":"1","op":"hello","args":[]}"#,
    ] {
        let mut s = Session::default();
        assert!(request(&mut s, bad).unwrap_err().close);
        assert_eq!(request(&mut s, HELLO).unwrap_err().code, ErrorCode::Closed);
    }
    let mut s = Session::default();
    request(&mut s, HELLO).unwrap();
    assert!(
        request(&mut s, br#"{"v":1,"id":"2","op":"hello","args":{}}"#)
            .unwrap_err()
            .close
    );
}

// Regression/characterization: parser behavior already implemented, not a TDD cycle.
#[test]
fn malformed_utf8_json_nonfinite_and_trailing_values_close_without_echoing_input() {
    for bad in [
        b"\xff".as_slice(),
        b"{}",
        b"[]",
        b"null",
        b"{",
        b"{}{}",
        br#"{"v":1,"id":"1","op":"hello","args":{"x":1e999}}"#,
        br#"{"v":1,"id":"1","op":"hello","args":{"x":"\uD800"}}"#,
    ] {
        let mut s = Session::default();
        let error = request(&mut s, bad).unwrap_err();
        assert_eq!(
            error,
            RequestError {
                code: ErrorCode::InvalidRequest,
                id: None,
                close: true
            }
        );
        assert_eq!(request(&mut s, HELLO).unwrap_err().code, ErrorCode::Closed);
    }
}
