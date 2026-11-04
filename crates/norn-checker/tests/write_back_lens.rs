#![allow(clippy::expect_used, missing_docs, reason = "test assertions")]
use norn_checker::mapping::{
    check_mapping_local,
    lens::{LENS_SLOT, admit},
};
use norn_ir::mapping::MappingIr;
use serde_json::{Value, json};

fn source_schema(complement: &str) -> norn_ir::source_schema::SourceSchemaIr {
    let mut raw: Value = serde_json::from_str(include_str!(
        "../../norn-ir/tests/fixtures/source_schema/frappe.json"
    ))
    .expect("source fixture");
    let record = raw.as_object_mut().expect("source schema");
    record.insert(
        "datasets".into(),
        json!([{"id":"orders", "nativeName":"orders", "kind":"document"}]),
    );
    for member in ["codeSets", "constraints", "profiles", "lineage"] {
        record.insert(member.into(), json!([]));
    }
    record.insert(
        "fields".into(),
        json!(
            ["approved", complement]
                .iter()
                .enumerate()
                .map(|(ordinal, name)| json!({
                    "dataset":"orders", "name":name, "ordinal":ordinal, "nativeType":"Check",
                    "typeCandidate":{"type":"boolean", "origin":"declared"}, "required":true
                }))
                .collect::<Vec<_>>()
        ),
    );
    serde_json::from_value(raw).expect("typed source")
}

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
    let lens = admit(&mapping, &source_schema("other"))
        .expect("checked")
        .expect("declared");
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
    let lens = admit(&mapping, &source_schema(key))
        .expect("checked")
        .expect("declared");
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
    assert!(admit(&mapping, &source_schema("other")).is_err());
}

#[test]
fn wrong_view_and_out_of_range_put_refuse() {
    for put in [
        json!([[0, 0], [1, 1], [2, 2], [3, 3]]),
        json!([[0, 99], [0, 1], [2, 3], [2, 3]]),
    ] {
        assert!(admit(&mapping("input.approved", put), &source_schema("other")).is_err());
    }
}

#[test]
fn putput_checks_history_dependence_even_when_round_trips_hold() {
    // False states 0,2 and true states 1,3. Each update is a section,
    // but a false -> true -> false cycle changes the hidden complement.
    let mapping = mapping("input.approved", json!([[0, 1], [2, 1], [2, 3], [0, 3]]));
    assert!(
        admit(&mapping, &source_schema("other"))
            .expect_err("PutPut fails")
            .contains("PutPut")
    );
}

#[test]
fn unknown_evaluation_or_fragment_never_passes() {
    for get in ["input.missing", "'not Boolean'", "1 / 0"] {
        assert!(
            admit(
                &mapping(get, json!([[0, 1], [0, 1], [2, 3], [2, 3]])),
                &source_schema("other")
            )
            .is_err()
        );
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
    assert!(admit(&mapping, &source_schema("other")).is_err());
    let extensions = mapping.extensions.as_mut().expect("extension");
    extensions.remove("norn.write-back/99.0.0");
    extensions.insert(
        LENS_SLOT.into(),
        serde_json::from_value(json!("x".repeat(65_537))).expect("JSON"),
    );
    assert!(
        admit(&mapping, &source_schema("other"))
            .err()
            .is_some_and(|error| error.contains("exceeds byte bound"))
    );
}

fn assert_schema_refusal(schema: norn_ir::source_schema::SourceSchemaIr, expected: &str) {
    use norn_checker::{ArtifactRef, EntityArtifactSetContext, check_artifact_set};
    use norn_ir::ArtifactEnvelope;
    let envelope = |id: &str, kind: &str, payload: Value| -> ArtifactEnvelope {
        serde_json::from_value(json!({
            "irVersion":"0.1", "artifactId":id, "kind":kind,
            "namespace":"toy", "name":id, "version":"0.1.0", "lifecycle":"draft",
            "scope":{"orgId":"org_demo", "environmentId":"dev"},
            "dependencies":[], "provenance":{"source":"import"}, "payload":payload
        }))
        .expect("envelope")
    };
    let source = envelope(
        "source",
        "source_schema",
        serde_json::to_value(schema).expect("schema"),
    );
    let mut mapping = mapping("input.approved", json!([[0, 1], [0, 1], [2, 3], [2, 3]]));
    mapping.source.sha256 = ArtifactRef::of(&source).expect("source hash").sha256;
    let artifacts = [
        source,
        envelope(
            "mapping",
            "mapping",
            serde_json::to_value(mapping).expect("mapping"),
        ),
    ];
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        findings
            .iter()
            .any(|d| d.code == "mapping.validation.invalid_document" && d.message == expected),
        "{findings:#?}"
    );
}

#[test]
fn declared_boolean_for_text_input_refuses_artifact_admission() {
    let mut schema = source_schema("other");
    schema.fields[0].type_candidate.normalized = norn_ir::source_schema::NormalizedType::Text;
    assert_schema_refusal(
        schema,
        "unknown: lens source field \"approved\" requires Text { format: None }, declared required Boolean",
    );
}

#[test]
fn missing_required_source_field_refuses_artifact_admission() {
    let mut schema = source_schema("other");
    let mut field = schema.fields[0].clone();
    field.name = "missing".into();
    field.ordinal = serde_json::from_value(json!(2)).expect("ordinal");
    schema.fields.push(field);
    assert_schema_refusal(schema, "unknown: lens source field \"missing\" is missing");
}

#[test]
fn extra_undeclared_source_field_refuses_artifact_admission() {
    let mut schema = source_schema("other");
    schema.fields.pop();
    assert_schema_refusal(schema, "unknown: lens source field \"other\" is undeclared");
}

#[test]
fn optional_source_field_refuses_artifact_admission() {
    let mut schema = source_schema("other");
    schema.fields[0].required = Some(false);
    assert_schema_refusal(
        schema,
        "unknown: lens source field \"approved\" requires Optional(Bool), declared required Boolean",
    );
}
