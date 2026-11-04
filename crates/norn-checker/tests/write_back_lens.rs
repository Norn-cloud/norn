#![allow(clippy::expect_used, missing_docs, reason = "test assertions")]
use norn_checker::mapping::{
    check_mapping_local,
    lens::{LENS_SLOT, admit},
};
use norn_ir::mapping::MappingIr;
use serde_json::{Value, json};

fn mapping(get: &str, mut put: Value) -> MappingIr {
    for row in put.as_array_mut().expect("put table") {
        for index in row.as_array_mut().expect("put row") {
            *index = json!(index.to_string());
        }
    }
    serde_json::from_value(json!({
        "format": "norn.mapping/1",
        "source": {"schemaId":"source", "version":"0.1.0", "sha256":"0".repeat(64)},
        "interfaces":["core.Order.interface"],
        "maps":[{"id":"order", "source":{"dataset":"orders"},
            "target":{"entity":"order.entity", "interface":"core.Order.interface"},
            "subject":"'one'", "sourceRecord":"'one'",
            "properties":[{"property":"approved", "value":get}]}],
        "extensions":{(LENS_SLOT):{
            "map":"order", "property":"approved",
            "sources":[{"approved":false,"other":false},{"approved":true,"other":false},
                       {"approved":false,"other":true},{"approved":true,"other":true}],
            "put":put
        }}
    }))
    .expect("typed mapping")
}

#[test]
fn lawful_projection_preserves_complement_and_refuses_outside_domain() {
    let mapping = mapping("input.approved", json!([[0, 1], [0, 1], [2, 3], [2, 3]]));
    assert!(
        check_mapping_local(&mapping).is_empty(),
        "{:?}",
        check_mapping_local(&mapping)
    );
    let lens = admit(&mapping).expect("checked").expect("declared");
    assert_eq!(lens.source_states(), 4);
    let source = [("approved".into(), false), ("other".into(), true)].into();
    let updated = lens.put(&source, true).expect("in domain");
    assert_eq!(updated.get("other"), Some(&true));
    assert!(lens.get(updated).expect("checked view"));
    assert!(
        lens.put(&std::collections::BTreeMap::default(), false)
            .is_err()
    );
}

#[test]
fn lawful_projection_round_trips_original_non_nfc_source_keys() {
    let mapping = mapping("input.approved", json!([[0, 1], [0, 1], [2, 3], [2, 3]]));
    let mut raw = serde_json::to_value(mapping).expect("mapping JSON");
    let sources = raw["extensions"][LENS_SLOT]["sources"]
        .as_array_mut()
        .expect("source records");
    let key = "e\u{301}";
    for source in sources {
        let record = source.as_object_mut().expect("source record");
        let complement = record.remove("other").expect("complement");
        record.insert(key.into(), complement);
    }
    let mapping = serde_json::from_value(raw).expect("typed mapping");
    assert!(check_mapping_local(&mapping).is_empty());
    let lens = admit(&mapping).expect("checked").expect("declared");
    for approved in [false, true] {
        for complement in [false, true] {
            let source = [("approved".into(), approved), (key.into(), complement)].into();
            let view = lens.get(&source).expect("original source is in domain");
            assert_eq!(view, approved);
            assert_eq!(lens.put(&source, view).expect("GetPut"), &source);
            for next_view in [false, true] {
                let updated = lens.put(&source, next_view).expect("original key retained");
                assert_eq!(updated.get(key), Some(&complement));
                assert!(!updated.contains_key("\u{e9}"));
                assert_eq!(lens.get(updated).expect("PutGet"), next_view);
                assert_eq!(lens.put(updated, view).expect("round trip"), &source);
            }
        }
    }
}

#[test]
fn seeded_lossy_overwrite_refuses_live_mapping_admission() {
    let mapping = mapping("input.approved", json!([[0, 1], [0, 1], [0, 1], [0, 1]]));
    let findings = check_mapping_local(&mapping);
    assert!(
        findings.iter().any(|d| d.message.contains("GetPut")),
        "{findings:?}"
    );
    assert!(admit(&mapping).is_err());
}

#[test]
fn wrong_view_and_out_of_range_put_refuse() {
    for put in [
        json!([[0, 0], [1, 1], [2, 2], [3, 3]]),
        json!([[0, 99], [0, 1], [2, 3], [2, 3]]),
    ] {
        assert!(admit(&mapping("input.approved", put)).is_err());
    }
}

#[test]
fn putput_checks_history_dependence_even_when_round_trips_hold() {
    // False states 0,2 and true states 1,3. Each update is a section,
    // but a false -> true -> false cycle changes the hidden complement.
    let mapping = mapping("input.approved", json!([[0, 1], [2, 1], [2, 3], [0, 3]]));
    assert!(
        admit(&mapping)
            .expect_err("PutPut fails")
            .contains("PutPut")
    );
}

#[test]
fn unknown_evaluation_or_fragment_never_passes() {
    for get in ["input.missing", "'not Boolean'", "1 / 0"] {
        assert!(admit(&mapping(get, json!([[0, 1], [0, 1], [2, 3], [2, 3]]))).is_err());
    }
    let mut mapping = mapping("input.approved", json!([[0, 1], [0, 1], [2, 3], [2, 3]]));
    mapping.extensions.as_mut().expect("extensions").insert(
        LENS_SLOT.into(),
        serde_json::from_value(json!({})).expect("JSON"),
    );
    assert!(!check_mapping_local(&mapping).is_empty());
}

#[test]
fn oversized_and_unsupported_declarations_refuse() {
    let mut mapping = mapping("input.approved", json!([[0, 1], [0, 1], [2, 3], [2, 3]]));
    let extensions = mapping.extensions.as_mut().expect("extension");
    extensions.insert(
        "norn.write-back/99.0.0".into(),
        serde_json::from_value(json!({})).expect("JSON"),
    );
    assert!(admit(&mapping).is_err());
    let extensions = mapping.extensions.as_mut().expect("extension");
    extensions.remove("norn.write-back/99.0.0");
    extensions.insert(
        LENS_SLOT.into(),
        serde_json::from_value(json!("x".repeat(65_537))).expect("JSON"),
    );
    assert!(admit(&mapping).is_err());
}
