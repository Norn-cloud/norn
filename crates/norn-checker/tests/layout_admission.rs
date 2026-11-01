#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeSet;

use norn_checker::{
    ArtifactRef, EntityArtifactSetContext, check_artifact, check_artifact_json,
    check_artifact_set_with_receipt, decode_artifact_json, obligations::DefinitionSource,
    validate_artifact,
};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile};
use serde_json::{Value, json};

fn valid_rule() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/rule-ir-v2/valid/reorder-priority.rule.json"
    ))
    .expect("valid RuleIR fixture")
}

fn add_inline_layout(artifact: &mut Value) {
    let graph = artifact
        .pointer_mut("/payload/graph")
        .and_then(Value::as_object_mut)
        .expect("rule graph");
    graph.insert(
        "viewport".to_owned(),
        json!({ "x": 12.5, "y": -4, "zoom": 1.25 }),
    );
    graph["nodes"]
        .as_array_mut()
        .and_then(|nodes| nodes.first_mut())
        .and_then(Value::as_object_mut)
        .expect("rule node")
        .insert(
            "description".to_owned(),
            Value::String("editor-only cafe\u{301} description".to_owned()),
        );
    graph["edges"]
        .as_array_mut()
        .and_then(|edges| edges.first_mut())
        .and_then(Value::as_object_mut)
        .expect("rule edge")
        .insert(
            "targetHandle".to_owned(),
            Value::String("editor-target".to_owned()),
        );
}

#[test]
fn inline_editor_layout_is_admitted_by_json_and_typed_single_artifact_entries() {
    let mut raw = valid_rule();
    add_inline_layout(&mut raw);
    let original = raw.clone();

    assert_eq!(check_artifact_json(&raw), []);
    assert_eq!(raw, original, "JSON admission does not mutate caller input");

    let typed = decode_artifact_json(&raw).expect("lenient envelope decode");
    assert!(validate_artifact(&typed).is_ok());
    assert_eq!(check_artifact(&typed), []);

    // Retain the inline description rather than letting admission extract it.
    let retained: ArtifactEnvelope =
        serde_json::from_value(raw.clone()).expect("retained envelope");
    assert_eq!(
        serde_json::to_value(&retained).expect("retained JSON")["payload"]["graph"]["nodes"][0]["description"],
        json!("editor-only cafe\u{301} description")
    );
    let clean: ArtifactEnvelope = serde_json::from_value(valid_rule()).expect("clean envelope");
    assert_eq!(
        ArtifactRef::of(&retained).expect("editor text is outside semantic identity"),
        ArtifactRef::of(&clean).expect("clean identity")
    );
    let context = EntityArtifactSetContext {
        registry: &BTreeSet::new(),
        base: &[],
        tenant: None,
    };
    let limits = ResourceLimitProfile::norn_limits_v1();
    let checked = check_artifact_set_with_receipt(&[retained], &context, &limits)
        .expect("ordinary receipt accepts decomposed editor text");
    assert!(checked.diagnostics.is_empty());
    assert_eq!(
        checked.receipt,
        check_artifact_set_with_receipt(&[clean], &context, &limits)
            .expect("clean receipt")
            .receipt
    );

    // Both proof source layers accept the verbatim decomposed editor description.
    let source = serde_json::to_string(&[raw]).expect("source documents");
    DefinitionSource::parse(&source, "[]").expect("candidate source retains editor spelling");
    DefinitionSource::parse("[]", &source).expect("base source retains editor spelling");
}

#[test]
fn malformed_layout_is_rejected_without_mutating_json_input() {
    let mut raw = valid_rule();
    add_inline_layout(&mut raw);
    raw["payload"]["graph"]["viewport"]["zoom"] = json!("large");
    let original = raw.clone();

    let diagnostics = check_artifact_json(&raw);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "layout.validation.invalid_inline_field" })
    );
    assert_eq!(
        raw, original,
        "failed extraction does not mutate caller input"
    );
}

#[test]
fn semantic_graph_fields_remain_closed_after_layout_extraction() {
    let mut raw = valid_rule();
    add_inline_layout(&mut raw);
    raw["payload"]["graph"]["unrecognizedSemanticField"] = json!(true);

    let diagnostics = check_artifact_json(&raw);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "rule.validation.invalid_payload"
            && diagnostic.message.contains("unrecognizedSemanticField")
    }));
}

#[test]
fn unknown_payload_root_fields_remain_closed_after_layout_extraction() {
    let mut raw = valid_rule();
    add_inline_layout(&mut raw);
    raw["payload"]["unrecognizedSemanticRoot"] = json!(true);

    let diagnostics = check_artifact_json(&raw);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "rule.validation.invalid_payload"
            && diagnostic.message.contains("unrecognizedSemanticRoot")
    }));
}
