use base64::{Engine, engine::general_purpose::STANDARD};
use rad_plugin_sdk::FailureCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;
pub(crate) fn matches_response(sent: Option<&str>, message: &Value) -> bool {
    sent.is_some_and(|id| !id.is_empty() && message["d"]["requestId"].as_str() == Some(id))
}
pub(crate) fn decode(text: &str) -> Result<Value, FailureCode> {
    if text.len() > 1024 * 1024 {
        return Err(FailureCode::Failed);
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for b in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > 32 {
                        return Err(FailureCode::Failed);
                    }
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let v: Value = serde_json::from_str(text).map_err(|_| FailureCode::Failed)?;
    fn bounded(v: &Value) -> bool {
        match v {
            Value::String(s) => s.len() <= 16384,
            Value::Array(a) => a.iter().all(bounded),
            Value::Object(m) => m.iter().all(|(k, v)| k.len() <= 16384 && bounded(v)),
            _ => true,
        }
    }
    if bounded(&v) {
        Ok(v)
    } else {
        Err(FailureCode::Failed)
    }
}
pub(crate) fn supports(requests: &[Value]) -> bool {
    crate::plugin::REQUIRED
        .iter()
        .all(|r| requests.iter().any(|v| v == r))
}

pub(crate) fn identify(hello: &Value, password: Option<&str>) -> Result<String, FailureCode> {
    if hello["op"] != 0 || hello["d"]["rpcVersion"].as_u64().is_none_or(|v| v < 1) {
        return Err(FailureCode::Incompatible);
    }
    let mut message = json!({"op":1,"d":{"rpcVersion":1,"eventSubscriptions":239}});
    if let Some(auth) = hello["d"].get("authentication") {
        let password = password.ok_or(FailureCode::CredentialsUnavailable)?;
        let salt = auth["salt"].as_str().ok_or(FailureCode::Failed)?;
        let challenge = auth["challenge"].as_str().ok_or(FailureCode::Failed)?;
        let value = authentication(password, salt, challenge);
        message["d"]["authentication"] = Value::String(value.to_string());
    }
    let encoded = serde_json::to_string(&message).map_err(|_| FailureCode::Failed);
    if let Some(Value::String(value)) = message["d"].get_mut("authentication") {
        zeroize::Zeroize::zeroize(value);
    }
    encoded
}

fn authentication(password: &str, salt: &str, challenge: &str) -> Zeroizing<String> {
    let mut hash = Sha256::new();
    hash.update(password.as_bytes());
    hash.update(salt.as_bytes());
    let first = Zeroizing::new(hash.finalize().to_vec());
    let secret = Zeroizing::new(STANDARD.encode(&*first));
    let mut hash = Sha256::new();
    hash.update(secret.as_bytes());
    hash.update(challenge.as_bytes());
    let second = Zeroizing::new(hash.finalize().to_vec());
    Zeroizing::new(STANDARD.encode(&*second))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_matches_independent_sha256_vector() {
        let actual = authentication(
            "supersecretpassword",
            "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
            "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY=",
        );
        assert_eq!(
            actual.as_str(),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
    }
    #[test]
    fn hello_requires_rpc_one_and_snapshot_password_only_when_challenged() {
        assert_eq!(
            identify(&json!({"op":0,"d":{"rpcVersion":0}}), None),
            Err(FailureCode::Incompatible)
        );
        assert_eq!(
            identify(
                &json!({"op":0,"d":{"rpcVersion":1,"authentication":{"salt":"s","challenge":"c"}}}),
                None
            ),
            Err(FailureCode::CredentialsUnavailable)
        );
        let v: Value =
            serde_json::from_str(&identify(&json!({"op":0,"d":{"rpcVersion":1}}), None).unwrap())
                .unwrap();
        assert_eq!(
            v,
            json!({"op":1,"d":{"rpcVersion":1,"eventSubscriptions":239}})
        );
    }
    #[test]
    fn connection_requires_control_and_catalog_capabilities_not_just_version() {
        assert!(!supports(&[json!("GetVersion")]));
        let requests = crate::plugin::REQUIRED
            .iter()
            .map(|v| json!(v))
            .collect::<Vec<_>>();
        assert!(supports(&requests));
    }
    #[test]
    fn peer_json_is_bounded_before_recursive_decoding_and_strings_are_capped() {
        let deep = format!("{}0{}", "[".repeat(33), "]".repeat(33));
        assert!(decode(&deep).is_err());
        assert!(decode(&json!({"payload":"x".repeat(16385)}).to_string()).is_err());
        assert!(decode(r#"{"d":{"text":"[not nesting]"}}"#).is_ok());
    }
    #[test]
    fn unsolicited_or_missing_id_never_matches_unsent_work() {
        assert!(!matches_response(None, &json!({"op":7,"d":{}})));
        assert!(!matches_response(
            Some("new"),
            &json!({"op":7,"d":{"requestId":"old"}})
        ));
        assert!(matches_response(
            Some("new"),
            &json!({"op":7,"d":{"requestId":"new"}})
        ));
    }
}
