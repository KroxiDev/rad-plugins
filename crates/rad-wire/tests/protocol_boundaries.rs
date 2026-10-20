use rad_wire::*;
fn bytes(payload: &[u8]) -> Vec<u8> {
    [(payload.len() as u32).to_be_bytes().as_slice(), payload].concat()
}
fn accept(s: &mut Session, payload: &str) -> Result<Request, RequestError> {
    let b = bytes(payload.as_bytes());
    s.accept(read_frame(&b).unwrap().unwrap())
}
fn ready() -> Session {
    let mut s = Session::default();
    accept(&mut s, r#"{"v":1,"id":"1","op":"hello","args":{}}"#).unwrap();
    s
}
fn envelope(args: &str) -> String {
    format!(r#"{{"v":1,"id":"2","op":"unknown_future_operation","args":{args}}}"#)
}
#[test]
fn nested_duplicates_after_valid_handshake_are_fatal_even_null_and_unescaped_aliases() {
    for args in [
        r#"{"a":null,"a":null}"#,
        r#"{"arr":[{"nested":{"a":0,"\u0061":1}}]}"#,
        r#"{"𐀀":0,"\uD800\uDC00":1}"#,
        r#"{"obj":{"x":{},"x":[]}}"#,
    ] {
        let mut s = ready();
        let err = accept(&mut s, &envelope(args)).unwrap_err();
        assert_eq!(
            err,
            RequestError {
                code: ErrorCode::InvalidRequest,
                id: None,
                close: true
            }
        );
        assert_eq!(
            accept(&mut s, r#"{"v":1,"id":"3","op":"status","args":{}}"#)
                .unwrap_err()
                .code,
            ErrorCode::Closed
        );
    }
}
#[test]
fn mixed_container_depth_empty_boundary_and_sibling_limits_are_independent() {
    for depth in 0..=16 {
        let mut inner = "null".to_owned();
        for i in 0..depth {
            inner = if i % 2 == 0 {
                format!("[{inner}]")
            } else {
                format!("{{\"x\":{inner}}}")
            };
        }
        let payload = envelope(&format!("{{\"x\":{inner}}}"));
        assert_eq!(
            accept(&mut ready(), &payload).is_ok(),
            depth <= 14,
            "depth {depth}"
        );
    }
    let max = vec!["null"; 256].join(",");
    let over = vec!["null"; 257].join(",");
    assert!(
        accept(
            &mut ready(),
            &envelope(&format!("{{\"a\":[{max}],\"b\":[{max}]}}"))
        )
        .is_ok()
    );
    assert!(
        accept(
            &mut ready(),
            &envelope(&format!("{{\"a\":[{max}],\"b\":[{over}]}}"))
        )
        .is_err()
    );
}
#[test]
fn borrowed_frame_pointer_and_suffix_after_recoverable_error_are_exact() {
    let a = bytes(br#"{"v":1,"id":"2","op":"run","args":false}"#);
    let b = bytes(br#"{"v":1,"id":"3","op":"status","args":{}}"#);
    let joined = [a.as_slice(), b.as_slice(), &[0xff, 0xff, 0xff, 0xff]].concat();
    let f = read_frame(&joined).unwrap().unwrap();
    assert_eq!(f.payload().as_ptr(), joined[4..].as_ptr());
    assert_eq!(f.consumed(), a.len());
    let mut s = ready();
    assert_eq!(
        s.accept(f).unwrap_err(),
        RequestError {
            code: ErrorCode::InvalidRequest,
            id: Some(2),
            close: false
        }
    );
    let next = read_frame(&joined[f.consumed()..]).unwrap().unwrap();
    assert_eq!(next.consumed(), b.len());
    assert_eq!(s.accept(next).unwrap().id(), 3);
    assert_eq!(
        read_frame(&joined[a.len() + b.len()..]).unwrap_err(),
        FrameError::InvalidLength
    );
}
#[test]
fn all_recoverable_args_shapes_consume_id_and_max_error_cannot_wrap() {
    for args in ["null", "true", "0", "\"private\"", "[]"] {
        let mut s = ready();
        let e = accept(&mut s, &envelope(args)).unwrap_err();
        assert_eq!(e.id, Some(2));
        assert!(!e.close);
        assert!(accept(&mut s, &envelope("{}")).unwrap_err().close);
        assert_eq!(
            accept(&mut s, r#"{"v":1,"id":"3","op":"status","args":{}}"#)
                .unwrap_err()
                .code,
            ErrorCode::Closed
        );
    }
    let mut s = ready();
    let e = accept(
        &mut s,
        r#"{"v":1,"id":"18446744073709551615","op":"run","args":null}"#,
    )
    .unwrap_err();
    assert!(!e.close);
    assert_eq!(e.id, Some(u64::MAX));
    assert!(
        accept(&mut s, r#"{"v":1,"id":"2","op":"status","args":{}}"#)
            .unwrap_err()
            .close
    );
}
#[test]
fn unicode_string_boundary_counts_bytes_and_error_does_not_echo_secret() {
    for (n, extra, ok) in [(4096, "", true), (4096, "a", false)] {
        let payload = envelope(&format!("{{\"text\":\"{}{extra}\"}}", "😀".repeat(n)));
        assert_eq!(accept(&mut ready(), &payload).is_ok(), ok);
    }
    let secret = "SENTINEL_PRIVATE";
    let payload = envelope(&format!("{{\"{secret}\":1e999}}"));
    let err = accept(&mut ready(), &payload).unwrap_err();
    assert!(!format!("{err:?}").contains(secret));
    assert_eq!(err.id, None);
    assert!(err.close);
    // Closed operations are deliberately NOT interpreted by this envelope-only seam.
    let request = accept(&mut ready(), &envelope(r#"{"client_role":"editor"}"#)).unwrap();
    assert_eq!(request.operation(), "unknown_future_operation");
    assert_eq!(request.args()["client_role"], "editor");
}
