//! Contract major 1 range compatibility and capability-gated options/JPEG.
use rad_plugin_contract::*;
use serde_json::{Value, json};

const TARGET: &str = "x86_64-unknown-linux-gnu";

fn field(id: &str, kind: Value) -> Value {
    json!({"id":id,"label":id,"required":false,"kind":kind})
}
fn manifest(host_min: u32, host_max: u32, fields: Vec<Value>) -> Value {
    json!({"contract":1,"capabilities":["credentials","dynamic_options","jpeg","response_timeout","visual_freshness"],"id":"org.rad.test.options","version":{"major":1,"minor":0,"patch":0},
        "host_min":host_min,"host_max":host_max,"targets":[TARGET],
        "actions":[{"id":"pick","label":"Pick","schema":{"version":1,"fields":fields},"credential_slots":[]}],
        "visuals":[]})
}
fn parse(value: &Value, host_api: u32) -> Result<Manifest, Error> {
    Document::new(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .manifest(host_api, TARGET)
}
fn dynamic(depends_on: &[&str]) -> Value {
    json!({"type":"dynamic_choice","depends_on":depends_on})
}
fn choice() -> Value {
    json!({"type":"choice","options":["normal","empty"]})
}

#[test]
fn major_one_accepts_matching_ranges_and_rejects_future_only_ranges() {
    assert_eq!((HOST_API, HOST_API_MIN), (1, 1));
    // (host_min, host_max, host_api) -> negotiated API, or the compatibility error.
    for (min, max, host_api, expected) in [
        (1, 1, 1, Ok(1)),
        (1, 2, 1, Ok(1)),
        (1, 3, 1, Ok(1)),
        (1, 99, 1, Ok(1)),
        (1, 9, 1, Ok(1)),
        (2, 2, 1, Err(Error::IncompatibleHost)),
        (3, 3, 1, Err(Error::IncompatibleHost)),
        (1, 1, 0, Err(Error::IncompatibleHost)),
    ] {
        let m = parse(&manifest(min, max, vec![]), host_api);
        assert_eq!(
            m.map(|m| m.negotiated_api(host_api)),
            expected,
            "range {min}..={max} against host API {host_api}"
        );
    }
}

#[test]
fn public_fixture_manifest_stays_valid_and_negotiates_api_one() {
    let bytes = include_bytes!("../../rad-plugin-host/fixture/manifest-v1.json");
    let m = Document::new(bytes)
        .unwrap()
        .manifest(HOST_API, TARGET)
        .unwrap();
    assert_eq!((m.host_min, m.host_max), (1, 1));
    assert_eq!(m.negotiated_api(HOST_API), 1);
}

#[test]
fn dynamic_choice_has_a_closed_json_shape_and_ordered_acyclic_dependencies() {
    let m = parse(
        &manifest(
            1,
            1,
            vec![
                field("scenario", choice()),
                field("group", dynamic(&["scenario"])),
                field("member", dynamic(&["scenario", "group"])),
                field("device", dynamic(&[])),
            ],
        ),
        HOST_API,
    )
    .unwrap();
    assert_eq!(
        m.actions[0].schema.fields[2].kind,
        FieldKind::DynamicChoice {
            depends_on: vec!["scenario".into(), "group".into()]
        }
    );
    assert_eq!(
        serde_json::to_value(&m.actions[0].schema.fields[1].kind).unwrap(),
        json!({"type":"dynamic_choice","depends_on":["scenario"]})
    );
    let four = vec!["a", "b", "c", "d"];
    let mut fields: Vec<_> = four.iter().map(|id| field(id, choice())).collect();
    fields.push(field("last", dynamic(&four)));
    assert!(parse(&manifest(1, 1, fields.clone()), HOST_API).is_ok());
    fields.insert(0, field("e", choice()));
    fields.last_mut().unwrap()["kind"] = dynamic(&["a", "b", "c", "d", "e"]);
    let five = fields;
    for (case, fields) in [
        ("five dependencies", five),
        (
            "forward reference",
            vec![
                field("group", dynamic(&["scenario"])),
                field("scenario", choice()),
            ],
        ),
        ("self reference", vec![field("group", dynamic(&["group"]))]),
        ("unknown field", vec![field("group", dynamic(&["missing"]))]),
        (
            "text dependency",
            vec![
                field("name", json!({"type":"text","max_bytes":8})),
                field("group", dynamic(&["name"])),
            ],
        ),
        (
            "boolean dependency",
            vec![
                field("flag", json!({"type":"boolean"})),
                field("group", dynamic(&["flag"])),
            ],
        ),
        (
            "integer dependency",
            vec![
                field("n", json!({"type":"integer","min":0,"max":1})),
                field("group", dynamic(&["n"])),
            ],
        ),
        (
            "duplicate dependency",
            vec![
                field("scenario", choice()),
                field("group", dynamic(&["scenario", "scenario"])),
            ],
        ),
        (
            "missing depends_on",
            vec![field("group", json!({"type":"dynamic_choice"}))],
        ),
        (
            "unknown property",
            vec![field(
                "group",
                json!({"type":"dynamic_choice","depends_on":[],"options":["a"]}),
            )],
        ),
    ] {
        assert_eq!(
            parse(&manifest(1, 1, fields), HOST_API).unwrap_err(),
            Error::InvalidManifest,
            "{case}"
        );
    }
}

#[test]
fn dynamic_choice_requires_its_declared_capability() {
    let fields = vec![field("group", dynamic(&[]))];
    for (capabilities, valid) in [
        (json!([]), false),
        (json!(["jpeg"]), false),
        (json!(["dynamic_options"]), true),
    ] {
        let mut value = manifest(1, 1, fields.clone());
        value["capabilities"] = capabilities;
        let result = parse(&value, HOST_API);
        assert_eq!(result.is_ok(), valid);
        if !valid {
            assert_eq!(result.unwrap_err(), Error::InvalidManifest);
        }
    }
    let mut visual = manifest(1, 1, vec![]);
    visual["visuals"] = json!([{"id":"probe","label":"Probe","schema":{"version":1,"fields":fields},"credential_slots":[]}]);
    visual["capabilities"] = json!(["visual_freshness"]);
    assert_eq!(
        parse(&visual, HOST_API).unwrap_err(),
        Error::InvalidManifest
    );
    visual["capabilities"] = json!(["visual_freshness", "dynamic_options"]);
    assert!(parse(&visual, HOST_API).is_ok());
}

fn picker() -> Definition {
    let mut group = field("group", dynamic(&["scenario"]));
    group["required"] = json!(true);
    parse(
        &manifest(
            1,
            1,
            vec![
                field("scenario", choice()),
                group,
                field("member", dynamic(&["scenario", "group"])),
            ],
        ),
        HOST_API,
    )
    .unwrap()
    .actions
    .remove(0)
}
fn parameters(values: Value) -> Parameters {
    serde_json::from_value(json!({"schema_version":1,"values":values})).unwrap()
}

#[test]
fn dynamic_values_accept_any_valid_string_without_checking_a_list() {
    let definition = picker();
    for group in [
        "a",
        "Escena ñandú 🎬",
        &"x".repeat(256),
        "gone-since-yesterday",
    ] {
        let p = parameters(json!({"group":group,"member":"anything"}));
        assert_eq!(p.validate(&definition), Ok(()), "{group:?}");
        let bytes = serde_json::to_vec(&p).unwrap();
        assert_eq!(
            Document::new(&bytes).unwrap().parameters(&definition),
            Ok(())
        );
    }
    // Optional dynamic values may be absent; required ones may not.
    assert_eq!(
        parameters(json!({"group":"a"})).validate(&definition),
        Ok(())
    );
    assert_eq!(
        parameters(json!({"member":"a"})).validate(&definition),
        Err(Error::InvalidParameters)
    );
    // A static choice keeps checking its closed list next to the dynamic ones.
    assert_eq!(
        parameters(json!({"scenario":"normal","group":"a"})).validate(&definition),
        Ok(())
    );
    assert_eq!(
        parameters(json!({"scenario":"other","group":"a"})).validate(&definition),
        Err(Error::InvalidParameters)
    );
}

#[test]
fn dynamic_values_reject_non_strings_empty_oversized_and_control_characters() {
    let definition = picker();
    let long = "x".repeat(257);
    let wide = "é".repeat(129); // 258 bytes
    for bad in [
        json!(7),
        json!(true),
        json!(""),
        json!(long),
        json!(wide),
        json!("two\nlines"),
        json!("nul\u{0}byte"),
        json!("tab\there"),
    ] {
        assert_eq!(
            parameters(json!({"group":bad})).validate(&definition),
            Err(Error::InvalidParameters),
            "{bad:?}"
        );
    }
}

#[test]
fn options_context_carries_only_declared_dependencies_with_valid_values() {
    let definition = picker();
    let check = |field: &str, values: Value| {
        definition.validate_options_context(field, &parameters(values))
    };
    assert_eq!(check("group", json!({})), Ok(()));
    assert_eq!(check("group", json!({"scenario":"normal"})), Ok(()));
    // Dynamic dependencies are not membership-checked: their list is dynamic.
    assert_eq!(
        check("member", json!({"scenario":"empty","group":"not-listed"})),
        Ok(())
    );
    for (case, field, values) in [
        ("unknown field", "nope", json!({})),
        ("static field", "scenario", json!({})),
        ("self as context", "group", json!({"group":"a"})),
        ("later field as context", "group", json!({"member":"a"})),
        ("undeclared key", "group", json!({"zzz":"a"})),
        (
            "choice outside its options",
            "group",
            json!({"scenario":"bogus"}),
        ),
        ("choice not a string", "group", json!({"scenario":1})),
        ("empty dynamic dependency", "member", json!({"group":""})),
        (
            "control in dynamic dependency",
            "member",
            json!({"group":"a\nb"}),
        ),
        ("nested value", "group", json!({"scenario":{"deep":1}})),
    ] {
        assert_eq!(
            check(field, values),
            Err(Error::InvalidParameters),
            "{case}"
        );
    }
    let stale = serde_json::from_value(json!({"schema_version":2,"values":{}})).unwrap();
    assert_eq!(
        definition.validate_options_context("group", &stale),
        Err(Error::UnsupportedVersion)
    );
}

fn framed(v: &Value) -> Vec<u8> {
    let b = serde_json::to_vec(v).unwrap();
    let mut f = (b.len() as u32).to_be_bytes().to_vec();
    f.extend(b);
    f
}
fn envelope(message: Value) -> Value {
    json!({"protocol":1,"correlation":{"generation":3,"request":5},"message":message})
}
fn key() -> Correlation {
    Correlation {
        generation: 3,
        request: 5,
    }
}
fn choices(options: usize) -> Value {
    let options: Vec<_> = (0..options)
        .map(|n| json!({"value":format!("v{n}"),"label":format!("Label {n}")}))
        .collect();
    envelope(json!({"kind":"choices","list":{"availability":"available","options":options}}))
}

#[test]
fn options_and_choices_messages_have_their_documented_json_and_roundtrip() {
    let context = serde_json::from_value::<Parameters>(
        json!({"schema_version":1,"values":{"scenario":"normal","group":"a"}}),
    )
    .unwrap();
    let option = |value: &str, label: &str| ChoiceOption {
        value: value.into(),
        label: label.into(),
    };
    let cases = [
        (
            json!({"kind":"options","scope":"action","definition":"pick","field":"member",
                "context":{"schema_version":1,"values":{"scenario":"normal","group":"a"}}}),
            Message::Options {
                settings: None,
                credential_revision: None,
                scope: OptionsScope::Action,
                definition: "pick".into(),
                field: "member".into(),
                context: context.clone(),
            },
        ),
        (
            json!({"kind":"options","scope":"visual","definition":"probe","field":"group",
                "context":{"schema_version":1,"values":{}}}),
            Message::Options {
                settings: None,
                credential_revision: None,
                scope: OptionsScope::Visual,
                definition: "probe".into(),
                field: "group".into(),
                context: Parameters {
                    schema_version: 1,
                    values: serde_json::Map::new(),
                },
            },
        ),
        (
            json!({"kind":"choices","list":{"availability":"available","options":[
                {"value":"a","label":"Group A"},{"value":"b","label":"Group B"}]}}),
            Message::Choices {
                list: ChoiceList::Available {
                    options: vec![option("a", "Group A"), option("b", "Group B")],
                },
            },
        ),
        (
            json!({"kind":"choices","list":{"availability":"available","options":[]}}),
            Message::Choices {
                list: ChoiceList::Available { options: vec![] },
            },
        ),
        (
            json!({"kind":"choices","list":{"availability":"unavailable"}}),
            Message::Choices {
                list: ChoiceList::Unavailable,
            },
        ),
    ];
    for (json, message) in cases {
        let expected = Envelope {
            protocol: 1,
            correlation: key(),
            message,
        };
        let frame = framed(&envelope(json.clone()));
        assert_eq!(decode(&frame), Ok(Some((expected.clone(), frame.len()))));
        let encoded = encode(&expected).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded[4..]).unwrap(),
            envelope(json)
        );
    }
}

#[test]
fn choice_lists_are_bounded_unique_and_closed() {
    assert!(decode(&framed(&choices(64))).unwrap().is_some());
    assert_eq!(decode(&framed(&choices(65))), Err(Error::InvalidMessage));
    let wrap = |options: Value| {
        envelope(json!({"kind":"choices","list":{"availability":"available","options":options}}))
    };
    let long = "x".repeat(257);
    assert!(
        decode(&framed(&wrap(
            json!([{"value":"x".repeat(256),"label":"x".repeat(256)}])
        )))
        .unwrap()
        .is_some()
    );
    for (case, options) in [
        (
            "duplicate values",
            json!([{"value":"a","label":"A"},{"value":"a","label":"Other"}]),
        ),
        ("empty value", json!([{"value":"","label":"A"}])),
        ("empty label", json!([{"value":"a","label":""}])),
        ("long value", json!([{"value":long,"label":"A"}])),
        ("long label", json!([{"value":"a","label":long}])),
        ("control in value", json!([{"value":"a\nb","label":"A"}])),
        ("control in label", json!([{"value":"a","label":"A\u{7f}"}])),
        (
            "unknown option property",
            json!([{"value":"a","label":"A","icon":"x"}]),
        ),
        ("missing label", json!([{"value":"a"}])),
    ] {
        assert_eq!(
            decode(&framed(&wrap(options))),
            Err(Error::InvalidMessage),
            "{case}"
        );
    }
    for list in [
        json!({"availability":"sometimes"}),
        json!({"availability":"available"}),
    ] {
        let message = envelope(json!({"kind":"choices","list":list}));
        assert_eq!(
            decode(&framed(&message)),
            Err(Error::InvalidMessage),
            "{message}"
        );
    }
    // encode applies the same validation before anything is written.
    let too_many = Envelope {
        protocol: 1,
        correlation: key(),
        message: Message::Choices {
            list: ChoiceList::Available {
                options: (0..65)
                    .map(|n| ChoiceOption {
                        value: format!("v{n}"),
                        label: "L".into(),
                    })
                    .collect(),
            },
        },
    };
    assert_eq!(encode(&too_many), Err(Error::InvalidMessage));
}

#[test]
fn options_queries_are_validated_like_every_other_host_message() {
    let valid = json!({"kind":"options","scope":"action","definition":"pick","field":"group",
        "context":{"schema_version":1,"values":{"scenario":"normal"}}});
    assert!(decode(&framed(&envelope(valid.clone()))).unwrap().is_some());
    for (pointer, value, error) in [
        ("/scope", json!("both"), Error::InvalidMessage),
        ("/definition", json!("Not An Id"), Error::InvalidMessage),
        ("/field", json!(""), Error::InvalidMessage),
        ("/extra", json!(1), Error::InvalidMessage),
        (
            "/context/values/scenario",
            json!({"deep":"SENSITIVE_FIXTURE"}),
            Error::InvalidParameters,
        ),
        (
            "/context/schema_version",
            json!(0),
            Error::InvalidParameters,
        ),
    ] {
        let mut message = valid.clone();
        match message.pointer_mut(pointer) {
            Some(slot) => *slot = value,
            None => message[pointer.trim_start_matches('/')] = value,
        }
        let err = decode(&framed(&envelope(message))).unwrap_err();
        assert_eq!(err, error, "{pointer}");
        assert!(!format!("{err:?}").contains("SENSITIVE_FIXTURE"));
    }
}

#[test]
fn visual_content_accepts_jpeg_with_the_png_rules_and_png_only_frames_still_decode() {
    let visual = |content: Value| {
        envelope(
            json!({"kind":"visual","valid_for_ms":1000,"state":{"availability":"current","content":content}}),
        )
    };
    let content_of = |message: Value| match decode(&framed(&message)).unwrap().unwrap().0.message {
        Message::Visual {
            valid_for_ms: 1000,
            state: VisualState::Current { content },
        } => content,
        other => panic!("unexpected {other:?}"),
    };
    // A non-JPEG frame has no jpeg_hex at all.
    let old = content_of(visual(json!({"text":"x","png_hex":null,"color":null})));
    assert_eq!(old.jpeg_hex, None);
    let jpeg = content_of(visual(
        json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":"ffd8ffd9"}),
    ));
    assert_eq!(jpeg.jpeg_hex.as_deref(), Some("ffd8ffd9"));
    assert!(jpeg.text.is_none() && jpeg.png_hex.is_none() && jpeg.color.is_none());
    let null = content_of(visual(
        json!({"text":"x","png_hex":null,"color":null,"jpeg_hex":null}),
    ));
    assert_eq!(null.jpeg_hex, None);
    let max = "ab".repeat(8192);
    assert!(
        decode(&framed(&visual(
            json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":max})
        )))
        .unwrap()
        .is_some()
    );
    for (case, content) in [
        (
            "png and jpeg",
            json!({"text":"x","png_hex":"89","color":null,"jpeg_hex":"ff"}),
        ),
        (
            "uppercase",
            json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":"FFD8"}),
        ),
        (
            "odd length",
            json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":"ffd"}),
        ),
        (
            "empty",
            json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":""}),
        ),
        (
            "not hex",
            json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":"../a.jpg"}),
        ),
    ] {
        assert_eq!(
            decode(&framed(&visual(content))),
            Err(Error::InvalidMessage),
            "{case}"
        );
    }
    // Past 16384 characters the wire layer refuses the string before the contract sees it.
    let huge = json!({"text":null,"png_hex":null,"color":null,"jpeg_hex":"ab".repeat(8193)});
    assert_eq!(decode(&framed(&visual(huge))), Err(Error::InvalidDocument));
    // Absent images stay absent on the wire even when JPEG is supported.
    let text_only = Envelope {
        protocol: 1,
        correlation: key(),
        message: Message::Visual {
            valid_for_ms: 1000,
            state: VisualState::Current { content: old },
        },
    };
    let wire = String::from_utf8(encode(&text_only).unwrap()[4..].to_vec()).unwrap();
    assert!(!wire.contains("jpeg_hex"), "{wire}");
    let mut with_jpeg = text_only.clone();
    if let Message::Visual {
        state: VisualState::Current { content },
        ..
    } = &mut with_jpeg.message
    {
        content.jpeg_hex = Some("ffd8ffd9".into());
    }
    let wire = String::from_utf8(encode(&with_jpeg).unwrap()[4..].to_vec()).unwrap();
    assert!(wire.contains(r#""jpeg_hex":"ffd8ffd9""#), "{wire}");
}
