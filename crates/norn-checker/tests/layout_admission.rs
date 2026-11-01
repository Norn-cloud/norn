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
    let checked = check_artifact_set_with_receipt(
        &fixture_sources(&[retained], context.base),
        &context,
        &limits,
    )
    .expect("ordinary receipt accepts decomposed editor text");
    assert!(checked.diagnostics.is_empty());
    assert_eq!(
        checked.receipt,
        check_artifact_set_with_receipt(
            &fixture_sources(&[clean], context.base),
            &context,
            &limits
        )
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

fn fixture_sources(
    artifacts: &[norn_ir::ArtifactEnvelope],
    base: &[norn_ir::ArtifactEnvelope],
) -> norn_checker::receipt::ReceiptSources {
    let candidates = norn_ir::runtime::encode(artifacts).expect("fixture");
    let installed = norn_ir::runtime::encode(base).expect("fixture");
    norn_checker::receipt::ReceiptSources::new(
        candidates.as_array().expect("fixture"),
        installed.as_array().expect("fixture"),
    )
    .expect("fixture")
}

fn unicode_rule_sources() -> (Value, Value) {
    let mut semantic = valid_rule();
    semantic["artifactId"] = json!("test.cafe\u{301}");
    semantic["payload"]["inputSchema"]["properties"]["cafe\u{301}"] = json!({"type":"string"});
    semantic["payload"]["graph"]["nodes"][4]["content"]["expressions"][0]["value"] =
        json!("short ? ['cafe\u{301}'] : []");
    let mut raw = semantic.clone();
    add_inline_layout(&mut raw);
    for edge in semantic["payload"]["graph"]["edges"]
        .as_array_mut()
        .expect("edges")
    {
        edge.as_object_mut().expect("edge").remove("type");
    }
    (raw, semantic)
}

#[test]
fn source_decode_verbatim_and_hash_correspondence() {
    use norn_checker::obligations::ApprovedContract;
    use norn_checker::receipt::{
        RECEIPT_HASH_PROFILE, check_artifact_sources_with_receipt, verify_admission_receipt_sources,
    };
    use norn_ir::RuntimeValue;

    let (raw, semantic) = unicode_rule_sources();
    let source = norn_ir::runtime::encode(&raw).expect("source");
    let prepared = norn_checker::prepare_artifact_source(source.clone()).expect("preparation");
    assert_eq!(
        prepared.source,
        norn_ir::runtime::encode(&semantic).expect("semantic source")
    );
    assert_eq!(
        prepared.artifact,
        decode_artifact_json(&semantic).expect("verbatim envelope")
    );
    assert!(!prepared.layout.artifacts.is_empty());
    let bytes = RECEIPT_HASH_PROFILE
        .canonicalize_json(serde_json::to_vec(&semantic).expect("semantic JSON"))
        .expect("existing hash profile");
    assert_eq!(
        prepared.source.canonical_json().expect("canonical source"),
        bytes
    );
    let expected_hash = RECEIPT_HASH_PROFILE.hash_canonical_bytes(&bytes);
    let limits = ResourceLimitProfile::norn_limits_v1();
    for in_base in [false, true] {
        let sources = [source.clone()];
        let (candidates, base): (&[RuntimeValue], &[RuntimeValue]) = if in_base {
            (&[], &sources)
        } else {
            (&sources, &[])
        };
        let checked = check_artifact_sources_with_receipt(
            candidates,
            base,
            &BTreeSet::new(),
            None,
            &limits,
            None,
        )
        .expect("source writer");
        assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
        let refs = if in_base {
            &checked.receipt.artifact_set.base
        } else {
            &checked.receipt.artifact_set.artifacts
        };
        assert_eq!(refs[0].artifact_id, "test.café");
        assert_eq!(refs[0].sha256, expected_hash);
        let body = String::from_utf8(checked.receipt.canonical_json().expect("receipt JSON"))
            .expect("receipt UTF-8");
        verify_admission_receipt_sources(
            &body,
            &checked.receipt.content_sha256().expect("receipt hash"),
            candidates,
            base,
            &limits,
        )
        .expect("writer to source verification");
        let mut changed = raw.clone();
        changed["payload"]["graph"]["nodes"][4]["content"]["expressions"][0]["value"] =
            json!("short ? ['changed'] : []");
        let changed = [norn_ir::runtime::encode(&changed).expect("changed source")];
        let (changed_candidates, changed_base): (&[RuntimeValue], &[RuntimeValue]) = if in_base {
            (&[], &changed)
        } else {
            (&changed, &[])
        };
        assert!(
            verify_admission_receipt_sources(
                &body,
                &checked.receipt.content_sha256().expect("receipt hash"),
                changed_candidates,
                changed_base,
                &limits,
            )
            .is_err()
        );
        let documents =
            serde_json::to_string(std::slice::from_ref(&raw)).expect("source documents");
        let (candidate_text, base_text) = if in_base {
            ("[]", documents.as_str())
        } else {
            (documents.as_str(), "[]")
        };
        let challenge_source =
            DefinitionSource::parse(candidate_text, base_text).expect("challenge source");
        ApprovedContract::approve(
            &challenge_source,
            &BTreeSet::new(),
            json!({"test":"semantics"}),
            json!({"test":"assumptions"}),
            json!({"test":"assurance"}),
        )
        .expect("challenge and source writer share the verbatim model");
    }
}

#[test]
fn receipt_shape_refuses_unknown_formats_and_malformed_law_origins() {
    let source = norn_ir::runtime::encode(&valid_rule()).expect("source");
    let checked = norn_checker::receipt::check_artifact_sources_with_receipt(
        &[source],
        &[],
        &BTreeSet::new(),
        None,
        &ResourceLimitProfile::norn_limits_v1(),
        None,
    )
    .expect("receipt");
    let mut receipt: Value =
        serde_json::from_slice(&checked.receipt.canonical_json().expect("bytes")).expect("JSON");
    receipt["format"] = json!("norn-checker/receipt@999");
    assert!(norn_checker::receipt::validate_receipt_shape(&receipt).is_err());
    let valid = json!({"inherited_from": {
        "pack_id":"test.pack", "pack_version":"0.1.0", "artifact_id":"test.law",
        "artifact_digest":"a".repeat(64), "law_id":"test.member"
    }});
    for format in ["norn-checker/receipt@4", "norn-checker/receipt@7"] {
        receipt["format"] = json!(format);
        receipt["lawEnrollment"] = json!([valid.clone()]);
        norn_checker::receipt::validate_receipt_shape(&receipt).expect("valid wire shape");
        for invalid in [Value::Null, json!({}), json!({"inherited_from": null})] {
            receipt["lawEnrollment"] = json!([invalid]);
            assert!(norn_checker::receipt::validate_receipt_shape(&receipt).is_err());
        }
        for field in [
            "pack_id",
            "pack_version",
            "artifact_id",
            "artifact_digest",
            "law_id",
        ] {
            for invalid in [Value::Null, json!(5)] {
                let mut entry = valid.clone();
                entry["inherited_from"][field] = invalid;
                receipt["lawEnrollment"] = json!([entry]);
                assert!(norn_checker::receipt::validate_receipt_shape(&receipt).is_err());
            }
            let mut entry = valid.clone();
            entry["inherited_from"]
                .as_object_mut()
                .expect("origin")
                .remove(field);
            receipt["lawEnrollment"] = json!([entry]);
            assert!(norn_checker::receipt::validate_receipt_shape(&receipt).is_err());
        }
    }
}

#[test]
fn source_hash_order_is_independent_of_unicode_spelling() {
    let (_, semantic) = unicode_rule_sources();
    let first = norn_ir::runtime::encode(&semantic).expect("source");
    let composed = norn_ir::RuntimeValue::from_json_text(first.canonical_json().expect("Norn1"))
        .expect("canonical source");
    let mut other = semantic;
    other["artifactId"] = json!("test.cafz");
    other["payload"]["ruleId"] = json!("test.rule.z");
    let other = norn_ir::runtime::encode(&other).expect("other source");
    let receipt = |source| {
        let checked = norn_checker::receipt::check_artifact_sources_with_receipt(
            &[source, other.clone()],
            &[],
            &BTreeSet::new(),
            None,
            &ResourceLimitProfile::norn_limits_v1(),
            None,
        )
        .expect("receipt");
        assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
        checked.receipt
    };
    let decomposed = receipt(first);
    let composed = receipt(composed);
    assert_eq!(decomposed.artifact_set, composed.artifact_set);
    assert_eq!(
        decomposed.content_sha256().expect("hash"),
        composed.content_sha256().expect("hash")
    );
}

#[test]
fn verbatim_execution_requires_more_than_the_norn1_artifact_hash() {
    let mut hashes = Vec::new();
    let mut results = Vec::new();
    for literal in ["é", "e\u{301}"] {
        let expression = format!("len('{literal}')");
        let mut document = valid_rule();
        document["payload"]["outputSchema"]["properties"]["band"] = json!({"type":"integer"});
        document["payload"]["graph"]["nodes"][4]["content"]["expressions"][1]["value"] =
            json!(expression);
        let source = norn_ir::runtime::encode(&document).expect("source");
        let checked = norn_checker::receipt::check_artifact_sources_with_receipt(
            &[source],
            &[],
            &BTreeSet::new(),
            None,
            &ResourceLimitProfile::norn_limits_v1(),
            None,
        )
        .expect("receipt");
        assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
        hashes.push(checked.receipt.artifact_set.artifacts[0].sha256.clone());
        results.push(
            norn_expr::Expression::compile(&expression)
                .expect("expression")
                .evaluate(
                    &norn_expr::Value::Null,
                    norn_expr::EvaluationBudget::new(100),
                )
                .expect("evaluation")
                .value()
                .clone(),
        );
    }
    assert_eq!(
        hashes[0], hashes[1],
        "the existing Norn1 profile is preserved"
    );
    assert_eq!(
        results,
        vec![norn_expr::Value::from(2), norn_expr::Value::from(3)]
    );
}
