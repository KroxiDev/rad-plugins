use rad_plugin_contract::*;
use serde_json::json;
fn manifest() -> Manifest {
    serde_json::from_value(json!({"contract":1,"capabilities":["credentials","dynamic_options","jpeg","response_timeout","visual_freshness"],"id":"org.rad.test","version":{"major":1,"minor":0,"patch":0},
    "host_min":1,"host_max":1,"targets":["test"],"actions":[{"id":"run","label":"Run","schema":{"version":1,"fields":[]},"credential_slots":["password"]}],"visuals":[]})).unwrap()
}
#[test]
fn credentials_require_capability_and_stay_out_of_parameters() {
    let mut m = manifest();
    assert!(m.validate(1, "test").is_ok());
    m.capabilities.retain(|c| c != CREDENTIALS);
    assert_eq!(m.validate(1, "test"), Err(Error::InvalidManifest));
    m.capabilities.push(CREDENTIALS.into());
    let p = Parameters {
        schema_version: 1,
        values: Default::default(),
    };
    assert!(p.validate(&m.actions[0]).is_ok());
    let mut p = p;
    p.values.insert("password".into(), json!("CANARY"));
    assert_eq!(p.validate(&m.actions[0]), Err(Error::InvalidParameters));
    for n in 0..32 {
        let mut d = m.actions[0].clone();
        d.id = format!("a{n}");
        d.credential_slots = vec![format!("slot{n}")];
        m.actions.push(d);
    }
    assert_eq!(m.validate(1, "test"), Err(Error::InvalidManifest));
}
fn framed(message: serde_json::Value) -> Vec<u8> {
    let b = serde_json::to_vec(
        &json!({"protocol":1,"correlation":{"generation":1,"request":2},"message":message}),
    )
    .unwrap();
    let mut f = (b.len() as u32).to_be_bytes().to_vec();
    f.extend(b);
    f
}
#[test]
fn credentials_codec_is_bounded_redacted_and_additive() {
    let good =
        json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":"CANARY"}]});
    let (e, _) = decode(&framed(good.clone())).unwrap().unwrap();
    assert!(!format!("{e:?}").contains("CANARY"));
    assert_eq!(decode(&encode(&e).unwrap()).unwrap().unwrap().0, e);
    for bad in [
        json!({"kind":"credentials","revision":0,"values":[]}),
        json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":"a"},{"slot":"password","value":"b"}]}),
        json!({"kind":"credentials","revision":1,"values":[{"slot":"z","value":""},{"slot":"a","value":""}]}),
        json!({"kind":"credentials","revision":1,"values":[{"slot":"password","value":"a".repeat(4097)}]}),
        json!({"kind":"credentials_applied","revision":1,"value":"CANARY"}),
    ] {
        assert!(decode(&framed(bad)).is_err());
    }
    let old = json!({"kind":"invoke","action":"run","parameters":{"schema_version":1,"values":{}}});
    let (e, _) = decode(&framed(old.clone())).unwrap().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&encode(&e).unwrap()[4..]).unwrap()["message"],
        old
    );
    assert!(decode(&framed(json!({"kind":"invoke","action":"run","credential_revision":0,"parameters":{"schema_version":1,"values":{}}}))).is_err());
}
#[test]
fn action_response_timeout_is_declared_bounded_and_capability_only() {
    let mut m = serde_json::to_value(manifest()).unwrap();
    m["actions"][0]["schema"]["fields"] = json!([{"id":"seconds","label":"Seconds","required":false,"kind":{"type":"integer","min":1,"max":120}}]);
    m["actions"][0]["response_timeout"] = json!({"field":"seconds","default_seconds":30});
    assert!(serde_json::from_value::<Manifest>(m.clone()).is_ok());
    for bad in [
        json!({"field":"missing","default_seconds":30}),
        json!({"field":"seconds","default_seconds":0}),
        json!({"field":"seconds","default_seconds":121}),
    ] {
        let mut bad_m = m.clone();
        bad_m["actions"][0]["response_timeout"] = bad;
        let parsed = serde_json::from_value::<Manifest>(bad_m).unwrap();
        assert!(parsed.validate(1, "test").is_err());
    }
}

#[test]
fn largest_escaped_snapshot_fits_and_limits_are_enforced_before_writing() {
    let values: Vec<_> = (0..32)
        .map(|i| CredentialValue {
            slot: format!("s{i:02}{}", "x".repeat(93)),
            value: "\0".repeat(256),
        })
        .collect();
    let e = Envelope {
        protocol: 1,
        correlation: Correlation {
            generation: 1,
            request: 2,
        },
        message: Message::Credentials {
            revision: 1,
            values,
        },
    };
    let frame = encode(&e).unwrap();
    assert!(frame.len() <= 65540);
    assert!(decode(&frame).is_ok());
    for (count, len) in [(33, 1), (3, 3000), (1, 4097)] {
        let values = (0..count)
            .map(|i| CredentialValue {
                slot: format!("s{i:02}"),
                value: "x".repeat(len),
            })
            .collect();
        assert!(CredentialValues::new(values).is_err());
    }
}
