#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "static evidence integration regressions"
)]
use norn_checker::definition_analysis::Unknown;
use norn_checker::{
    EntityArtifactSetContext, check_artifact_set_for_install, check_artifact_set_with_receipt,
};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn order() -> Value {
    serde_json::from_str::<Vec<Value>>(include_str!("../../../fixtures/law-ir/entities.json"))
        .expect("entities")
        .remove(0)
}
fn check(values: &[Value]) -> norn_checker::CheckedArtifactSet {
    let artifacts: Vec<ArtifactEnvelope> = values
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("artifact"))
        .collect();
    check_artifact_set_with_receipt(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("check")
}
fn table_rule() -> Value {
    let mut rule = order();
    rule["kind"] = json!("rule");
    rule["artifactId"] = json!("erpnext.finite.rule");
    let schema = json!({"type":"object","properties":{"approved":{"type":"boolean"}},"required":["approved"],"additionalProperties":false});
    rule["payload"] = json!({"ruleId":"erpnext.finite","ruleVersion":"2.0","contract":"sub_decision","scopeRequired":["orgId","environmentId"],"inputSchema":schema,"outputSchema":schema,"graph":{"nodes":[{"id":"input","name":"input","type":"inputNode"},{"id":"table","name":"table","type":"decisionTableNode","content":{"hitPolicy":"first","inputs":[{"id":"condition","field":"approved"}],"outputs":[{"id":"result","field":"approved"}],"rules":[{"_id":"approved","condition":"true","result":"true"}]}},{"id":"output","name":"output","type":"outputNode"}],"edges":[{"id":"a","sourceId":"input","targetId":"table"},{"id":"b","sourceId":"table","targetId":"output"}]}});
    rule
}
#[test]
fn table_findings_are_identical_on_shared_check_and_install_paths() {
    let rule = table_rule();
    let checked = check(std::slice::from_ref(&rule));
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let analysis = checked.analysis.as_ref().expect("analysis");
    let table = analysis.tables[0]
        .result
        .as_ref()
        .expect("exhaustive finite");
    assert_eq!(table.inputs_checked, 2);
    assert_eq!(table.uncovered, Some(json!({"approved":false})));
    let artifacts = vec![serde_json::from_value(rule).expect("rule")];
    let installed = check_artifact_set_for_install(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
        None,
    )
    .expect("install admission");
    assert_eq!(checked.analysis, installed.analysis);
}
#[test]
fn runtime_defaults_do_not_borrow_the_unmodified_root_input_domain() {
    let mut rule = table_rule();
    rule["payload"]["defaults"] = json!({"approved":true});
    let checked = check(&[rule]);
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    assert_eq!(
        checked.analysis.expect("analysis").tables[0].result,
        Err(Unknown::GraphInput)
    );
}

#[test]
fn unsupported_rule_graphs_never_disappear_from_analysis() {
    for (path, value, base_unknown) in [
        (
            "/payload/graph/viewport",
            json!({"x":"bad","y":-4,"zoom":1.25}),
            Unknown::NotAdmitted,
        ),
        (
            "/payload/graph/viewport",
            json!({"x":12.5,"y":-4,"zoom":1.25,"unknown":true}),
            Unknown::NotAdmitted,
        ),
        (
            "/payload/graph/nodes/1/semanticUnknown",
            json!(true),
            Unknown::GraphInput,
        ),
        (
            "/payload/graph/edges/0/semanticUnknown",
            json!(true),
            Unknown::GraphInput,
        ),
    ] {
        let mut rule = table_rule();
        // Object indexing inserts the unsupported field, including at nested nodes/edges.
        let (parent, key) = path.rsplit_once('/').expect("field path");
        rule.pointer_mut(parent).expect("parent")[key] = value;
        let checked = check(&[rule.clone()]);
        assert!(!checked.receipt.admitted, "{path}");
        assert_eq!(checked.analysis, Err(Unknown::NotAdmitted));
        // Malformed layout is refused even in the base: viewport coordinates must
        // be decimals and unknown viewport fields are rejected. Base semantic
        // admission is external to this API; unsupported admitted graphs must
        // still yield GraphInput rather than a successful empty report.
        let base = [serde_json::from_value(rule).expect("base rule")];
        let checked = check_artifact_set_with_receipt(
            &[],
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("check base");
        assert_eq!(
            checked.receipt.admitted,
            base_unknown != Unknown::NotAdmitted,
            "{path}: {:?}",
            checked.diagnostics
        );
        assert_eq!(checked.analysis, Err(base_unknown), "{path}");
    }
    // Non-rule definitions still produce ordinary evidence, without a graph requirement.
    assert!(check(&[order()]).analysis.is_ok());
}

#[test]
fn each_table_binds_the_artifact_version_digest_and_stable_node() {
    let mut rule = table_rule();
    let checked = check(std::slice::from_ref(&rule));
    let report = &checked.analysis.expect("analysis").tables[0];
    let artifact = serde_json::from_value(rule.clone()).expect("artifact");
    assert_eq!(
        report.artifact,
        norn_checker::ArtifactRef::of(&artifact).expect("identity")
    );
    assert_eq!(report.node_id, "table");
    let prior = report.artifact.clone();
    rule["version"] = json!("2.0.0");
    let updated = check(&[rule]).analysis.expect("updated analysis");
    assert_eq!(updated.tables[0].artifact.artifact_id, prior.artifact_id);
    assert_ne!(updated.tables[0].artifact.version, prior.version);
    assert_ne!(updated.tables[0].artifact.sha256, prior.sha256);
}

#[test]
fn executable_unicode_spelling_has_a_distinct_protected_identity() {
    let report = |literal: &str| {
        let mut rule = table_rule();
        rule["payload"]["graph"]["nodes"][1]["content"]["inputs"][0]
            .as_object_mut()
            .expect("input")
            .remove("field");
        rule["payload"]["graph"]["nodes"][1]["content"]["rules"][0]["condition"] =
            json!(format!("len('{literal}') == 2"));
        let checked = check(&[rule]);
        assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
        checked.analysis.expect("analysis").tables.remove(0)
    };
    let composed = report("é");
    let decomposed = report("e\u{301}");
    assert_eq!(composed.artifact, decomposed.artifact);
    assert_ne!(composed.artifact_exact_digest, decomposed.artifact_exact_digest);
}
