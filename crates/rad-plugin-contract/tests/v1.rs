use rad_plugin_contract::*;
#[test]
fn reference_vectors_round_trip_with_strict_shared_contract() {
    let vectors: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../vectors/v1.json")).unwrap();
    for value in vectors {
        let payload = serde_json::to_vec(&value).unwrap();
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend(payload);
        let (decoded, used) = decode(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
        assert_eq!(
            decode(&encode(&decoded).unwrap()).unwrap().unwrap().0,
            decoded
        );
    }
}
#[test]
fn negotiated_intersection_gates_fields_and_messages() {
    let offered = vec![
        "unknown_future".into(),
        JPEG.into(),
        VISUAL_FRESHNESS.into(),
    ];
    let ours = vec![VISUAL_FRESHNESS.into()];
    assert_eq!(negotiate_capabilities(&offered, &ours).unwrap(), ours);
    assert!(
        !Message::VisualRenew {
            valid_for_ms: 25000
        }
        .permitted(&[])
    );
    assert!(
        Message::VisualRenew {
            valid_for_ms: 25000
        }
        .permitted(&ours)
    );
    assert!(negotiate_capabilities(&[JPEG.into(), JPEG.into()], &ours).is_err());
}
#[test]
fn host_bounds_visual_validity() {
    for valid_for_ms in [0, 999, 60001, u64::MAX] {
        assert!(
            encode(&Envelope {
                protocol: 1,
                correlation: Correlation {
                    generation: 1,
                    request: 2
                },
                message: Message::VisualRenew { valid_for_ms }
            })
            .is_err()
        );
    }
}
