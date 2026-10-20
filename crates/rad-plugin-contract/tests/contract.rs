use rad_plugin_contract::{Document, Error};
use serde_json::{Value, json};
fn manifest() -> Value {
    json!({"contract":1,"capabilities":["credentials","dynamic_options","jpeg","response_timeout","visual_freshness"],"id":"org.rad.fixture","version":{"major":1,"minor":0,"patch":0},
        "host_min":1,"host_max":1,"targets":["x86_64-unknown-linux-gnu"],
        "actions":[],"visuals":[{"id":"status","label":"Status","schema":{"version":1,"fields":[]},"credential_slots":[]}]})
}
#[test]
fn manifest_compatibility_preserves_unknown_and_incompatible_bytes() {
    let bytes = serde_json::to_vec(&manifest()).unwrap();
    let doc = Document::new(&bytes).unwrap();
    assert!(doc.manifest(1, "x86_64-unknown-linux-gnu").is_ok());
    // A future host may include major 1 in its range; major 0 shares nothing with it.
    assert!(doc.manifest(2, "x86_64-unknown-linux-gnu").is_ok());
    assert_eq!(
        doc.manifest(0, "x86_64-unknown-linux-gnu").unwrap_err(),
        Error::IncompatibleHost
    );
    assert_eq!(
        doc.manifest(1, "aarch64-unknown-linux-gnu").unwrap_err(),
        Error::IncompatibleTarget
    );
    for (key, value) in [
        ("contract", json!(2)),
        ("future", json!({"opaque":"keep"})),
        ("id", json!("../escape")),
    ] {
        let mut v = manifest();
        v[key] = value;
        let bytes = serde_json::to_vec(&v).unwrap();
        let doc = Document::new(&bytes).unwrap();
        assert!(doc.manifest(1, "x86_64-unknown-linux-gnu").is_err());
        assert_eq!(doc.bytes(), bytes);
    }
    assert_eq!(doc.bytes(), bytes);
    assert!(!format!("{doc:?}").contains("org.rad.fixture"));
}
#[test]
fn parameters_roundtrip_versions_and_credentials_are_non_destructive() {
    let mut v = manifest();
    v["visuals"][0]["schema"]["fields"] = json!([
        {"id":"enabled","label":"Enabled","required":true,"kind":{"type":"boolean"}},
        {"id":"level","label":"Level","required":true,"kind":{"type":"integer","min":0,"max":100}},
        {"id":"name","label":"Name","required":false,"kind":{"type":"text","max_bytes":4}},
        {"id":"mode","label":"Mode","required":false,"kind":{"type":"choice","options":["one","two"]}}
    ]);
    let m = Document::new(&serde_json::to_vec(&v).unwrap())
        .unwrap()
        .manifest(1, "x86_64-unknown-linux-gnu")
        .unwrap();
    let v1 = &m.visuals[0];
    let bytes=br#"{ "schema_version":1, "values":{"enabled":true,"level":100,"name":"abcd","mode":"two"} }"#;
    let doc = Document::new(bytes).unwrap();
    assert_eq!(doc.parameters(v1), Ok(()));
    let mut v2 = v1.clone();
    v2.schema.version = 2;
    assert_eq!(doc.parameters(&v2), Err(Error::UnsupportedVersion));
    assert_eq!(doc.parameters(v1), Ok(()));
    assert_eq!(doc.bytes(), bytes);
    for values in [
        json!({"enabled":true,"level":101}),
        json!({"level":1}),
        json!({"enabled":1,"level":0}),
        json!({"enabled":true,"level":0,"future":"retain"}),
        json!({"enabled":true,"level":0,"name":"abcde"}),
        json!({"enabled":true,"level":0,"mode":"three"}),
    ] {
        let bytes = serde_json::to_vec(&json!({"schema_version":1,"values":values})).unwrap();
        let d = Document::new(&bytes).unwrap();
        assert_eq!(d.parameters(v1), Err(Error::InvalidParameters));
        assert_eq!(d.bytes(), bytes);
    }
    let mut secured = v1.clone();
    secured.credential_slots.push("password".into());
    assert_eq!(doc.parameters(&secured), Ok(()));
    secured.credential_slots.push("name".into());
    assert_eq!(secured.validate(), Err(Error::InvalidManifest));
}
#[test]
fn framed_protocol_roundtrips_and_rejects_wrong_generation() {
    use rad_plugin_contract::*;
    let key = Correlation {
        generation: 7,
        request: 1,
    };
    let version = Version {
        major: 1,
        minor: 0,
        patch: 0,
    };
    let parameters = Parameters {
        schema_version: 1,
        values: serde_json::Map::new(),
    };
    let content = VisualContent {
        text: Some("42".into()),
        png_hex: None,
        jpeg_hex: None,
        color: Some([1, 2, 3, 255]),
    };
    let messages = [
        Message::Hello {
            capabilities: vec![],
            host_api: 1,
            target: "x86_64-unknown-linux-gnu".into(),
            plugin_id: "org.rad.fixture".into(),
            plugin_version: version,
        },
        Message::Ready {
            capabilities: vec![],
            plugin_id: "org.rad.fixture".into(),
            plugin_version: version,
        },
        Message::Invoke {
            settings: None,
            control: None,
            credential_revision: None,
            action: "run".into(),
            parameters: parameters.clone(),
        },
        Message::Result {
            outcome: Outcome::Success,
        },
        Message::Result {
            outcome: Outcome::Failure {
                code: FailureCode::Unavailable,
            },
        },
        Message::Subscribe {
            settings: None,
            credential_revision: None,
            source: "status".into(),
            parameters,
        },
        Message::Visual {
            valid_for_ms: 1000,
            state: VisualState::Current {
                content: content.clone(),
            },
        },
        Message::Visual {
            valid_for_ms: 1000,
            state: VisualState::Stale { content },
        },
        Message::Visual {
            valid_for_ms: 1000,
            state: VisualState::Unavailable,
        },
        Message::Unsubscribe { subscription: key },
    ];
    for message in messages {
        let envelope = Envelope {
            protocol: 1,
            correlation: key,
            message,
        };
        let frame = encode(&envelope).unwrap();
        for n in 0..frame.len() {
            assert_eq!(decode(&frame[..n]).unwrap(), None);
        }
        let mut joined = frame.clone();
        joined.extend_from_slice(&frame);
        assert_eq!(decode(&joined).unwrap(), Some((envelope, frame.len())));
    }
    assert!(key.matches(key));
    assert!(!key.matches(Correlation {
        generation: 8,
        request: 1
    }));
    assert!(!key.matches(Correlation {
        generation: 7,
        request: 2
    }));
    assert!(
        !Correlation {
            generation: 0,
            request: 0
        }
        .matches(Correlation {
            generation: 0,
            request: 0
        })
    );
    assert_eq!(decode(&0u32.to_be_bytes()), Err(Error::InvalidFrame));
    assert_eq!(decode(&65537u32.to_be_bytes()), Err(Error::InvalidFrame));
}
#[test]
fn public_dtos_cannot_bypass_resource_and_schema_limits() {
    use rad_plugin_contract::*;
    let mut definition = Definition {
        gestures: vec![Gesture::Press],
        response_timeout: None,
        id: "run".into(),
        label: "Run".into(),
        schema: Schema {
            version: 1,
            fields: vec![],
        },
        credential_slots: vec![],
    };
    let mut parameters = Parameters {
        schema_version: 1,
        values: serde_json::Map::new(),
    };
    for n in 0..4 {
        let id = format!("field{n}");
        definition.schema.fields.push(Field {
            id: id.clone(),
            label: "Field".into(),
            required: true,
            kind: FieldKind::Text { max_bytes: 4096 },
        });
        parameters.values.insert(id, json!("x".repeat(4096)));
    }
    assert_eq!(parameters.validate(&definition), Err(Error::LimitExceeded));
    let mut m: Manifest = serde_json::from_value(manifest()).unwrap();
    for n in 0..64 {
        let mut d = definition.clone();
        d.id = format!("definition{n}");
        d.label = "x".repeat(256);
        for f in &mut d.schema.fields {
            f.label = "y".repeat(256);
        }
        m.actions.push(d);
    }
    assert_eq!(
        m.validate(1, "x86_64-unknown-linux-gnu"),
        Err(Error::LimitExceeded)
    );
    assert!(Document::new(&vec![0; 65536]).is_ok());
    assert_eq!(
        Document::new(&vec![0; 65537]).unwrap_err(),
        Error::LimitExceeded
    );
    for raw in [
        br#"{"contract":1,"capabilities":["credentials","dynamic_options","jpeg","response_timeout","visual_freshness"],"contract":1}"#.as_slice(),
        br#"{"schema_version":1,"values":{"secret":"a","secret":"b"}}"#.as_slice(),
    ] {
        let d = Document::new(raw).unwrap();
        assert!(d.manifest(1, "x86_64-unknown-linux-gnu").is_err());
        assert!(d.parameters(&definition).is_err());
        assert_eq!(d.bytes(), raw);
    }
    let deep = format!("{}0{}", "[".repeat(17), "]".repeat(17));
    assert!(
        Document::new(deep.as_bytes())
            .unwrap()
            .manifest(1, "x86_64-unknown-linux-gnu")
            .is_err()
    );
}
#[test]
fn incompatible_v2_fields_survive_rollback_and_reinstallation() {
    use rad_plugin_contract::*;
    let m: Manifest = serde_json::from_value(manifest()).unwrap();
    let v1 = &m.visuals[0];
    let mut v2 = v1.clone();
    // Even when a publisher reuses a schema version, unknown fields fail closed.
    v2.schema.fields.push(Field {
        id: "future".into(),
        label: "Future".into(),
        required: false,
        kind: FieldKind::Boolean,
    });
    let bytes = br#"{ "schema_version":1, "values":{"future":true} }"#;
    let d = Document::new(bytes).unwrap();
    assert_eq!(d.parameters(&v2), Ok(()));
    assert_eq!(d.parameters(v1), Err(Error::InvalidParameters));
    assert_eq!(d.bytes(), bytes);
    assert_eq!(d.parameters(&v2), Ok(()));
    let malformed = b"not supported future encoding";
    let d = Document::new(malformed).unwrap();
    assert!(d.parameters(v1).is_err());
    assert_eq!(d.bytes(), malformed);
}
#[test]
fn malformed_messages_and_sensitive_diagnostics_fail_closed() {
    use rad_plugin_contract::*;
    fn framed(v: &Value) -> Vec<u8> {
        let b = serde_json::to_vec(v).unwrap();
        let mut f = (b.len() as u32).to_be_bytes().to_vec();
        f.extend(b);
        f
    }
    let good = json!({"protocol":1,"correlation":{"generation":1,"request":2},"message":{"kind":"result","outcome":{"status":"success"}}});
    for (pointer, value) in [
        ("/protocol", json!(2)),
        ("/correlation/generation", json!(0)),
        ("/correlation/request", json!(0)),
        ("/message/kind", json!("future")),
        (
            "/message/outcome",
            json!({"status":"failure","code":"failed","detail":"SENSITIVE_FIXTURE"}),
        ),
        (
            "/message/outcome",
            json!({"status":"failure","code":"SENSITIVE_FIXTURE"}),
        ),
    ] {
        let mut v = good.clone();
        *v.pointer_mut(pointer).unwrap() = value;
        let err = decode(&framed(&v)).unwrap_err();
        assert!(!format!("{err:?}").contains("SENSITIVE_FIXTURE"));
        assert!(
            !serde_json::to_string(&err)
                .unwrap()
                .contains("SENSITIVE_FIXTURE")
        );
    }
    let mut v = good.clone();
    v["message"] = json!({"kind":"visual","valid_for_ms":1000,"state":{"availability":"current","content":{"text":null,"png_hex":null,"color":null}}});
    assert_eq!(decode(&framed(&v)), Err(Error::InvalidMessage));
    v["message"]["state"]["content"]["text"] = json!("x".repeat(257));
    assert_eq!(decode(&framed(&v)), Err(Error::InvalidMessage));
    v["message"]["state"]["content"]["text"] = json!("x".repeat(256));
    assert!(decode(&framed(&v)).is_ok());
    v["message"]["state"]["content"]["png_hex"] = json!("../image.png");
    assert_eq!(decode(&framed(&v)), Err(Error::InvalidMessage));
    v["message"] = json!({"kind":"invoke","action":"run","parameters":{"schema_version":1,"values":{"password":{"secret":"SENSITIVE_FIXTURE"}}}});
    assert_eq!(decode(&framed(&v)), Err(Error::InvalidParameters));
    let mut payload = serde_json::to_vec(&good).unwrap();
    payload.resize(65536, b' ');
    let mut exact = 65536u32.to_be_bytes().to_vec();
    exact.extend(payload);
    assert_eq!(decode(&exact).unwrap().unwrap().1, 65540);
    let duplicate = br#"{"protocol":1,"protocol":1}"#;
    let mut f = (duplicate.len() as u32).to_be_bytes().to_vec();
    f.extend(duplicate);
    assert_eq!(decode(&f), Err(Error::InvalidDocument));
}
#[test]
fn manifest_rejects_ambiguous_identity_and_declarative_schema() {
    for change in [
        json!({"id":""}),
        json!({"id":"UpperCase"}),
        json!({"targets":["x86_64-unknown-linux-gnu","x86_64-unknown-linux-gnu"]}),
        json!({"host_min":2,"host_max":1}),
        json!({"actions":[],"visuals":[]}),
    ] {
        let mut m = manifest();
        for (k, v) in change.as_object().unwrap() {
            m[k] = v.clone();
        }
        assert!(
            Document::new(&serde_json::to_vec(&m).unwrap())
                .unwrap()
                .manifest(1, "x86_64-unknown-linux-gnu")
                .is_err()
        );
    }
    for kind in [
        json!({"type":"secret"}),
        json!({"type":"text","max_bytes":4097}),
        json!({"type":"integer","min":1,"max":0}),
        json!({"type":"choice","options":["same","same"]}),
    ] {
        let mut m = manifest();
        m["visuals"][0]["schema"]["fields"] =
            json!([{"id":"field","label":"Field","required":false,"kind":kind}]);
        assert!(
            Document::new(&serde_json::to_vec(&m).unwrap())
                .unwrap()
                .manifest(1, "x86_64-unknown-linux-gnu")
                .is_err()
        );
    }
    let mut m = manifest();
    let d = m["visuals"][0].clone();
    m["visuals"].as_array_mut().unwrap().push(d);
    assert!(
        Document::new(&serde_json::to_vec(&m).unwrap())
            .unwrap()
            .manifest(1, "x86_64-unknown-linux-gnu")
            .is_err()
    );
}

#[test]
fn lifecycle_defaults_are_legacy_compatible_and_opt_in_is_strict() {
    let parse = |value: &Value| {
        Document::new(&serde_json::to_vec(value).unwrap())
            .unwrap()
            .manifest(1, "x86_64-unknown-linux-gnu")
    };
    let legacy = manifest();
    let parsed = parse(&legacy).unwrap();
    assert!(
        serde_json::to_value(&parsed)
            .unwrap()
            .get("lifecycle")
            .is_none()
    );
    let mut explicit = legacy.clone();
    explicit["lifecycle"] = json!("always_loaded");
    let parsed = parse(&explicit).expect("developer static opt-in is supported");
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["lifecycle"],
        "always_loaded"
    );
    explicit["lifecycle"] = json!("on_demand");
    assert_eq!(parse(&explicit).unwrap(), parse(&legacy).unwrap());
    for invalid in [
        json!("automatic"),
        json!(true),
        json!(null),
        json!({"mode":"on_demand"}),
    ] {
        explicit["lifecycle"] = invalid;
        assert!(parse(&explicit).is_err());
    }
}
