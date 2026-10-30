#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The checker receipt (#2851, completing #2519): the ZEN Expression type
//! environment reference and the expression checker profile are bound into
//! the receipt beside the resource-limit profile, the artifact-set identity,
//! the checker version, and the findings digest.

use std::collections::BTreeSet;
use std::path::Path;

use norn_checker::receipt::{
    CHECKER_NAME, CHECKER_VERSION, EXPR_PROFILE, RECEIPT_FORMAT, admission_receipt_sha256, codes,
    recover_legacy_admission_receipt, verify_admission_receipt,
};
use norn_checker::{
    ArtifactRef, ArtifactSetRef, CheckReceipt, CheckedArtifactSet, DiagnosticsDigest,
    EntityArtifactSetContext, IDENTIFIER_ADMISSION_PROFILE, TenantLayer,
    check_artifact_set_with_flow_source_map, check_artifact_set_with_receipt, derive_type_env,
};
use norn_ir::{
    ArtifactEnvelope, ExtensionNamespace, FlowSourceMap, FlowSourceMapArtifactRef,
    FlowSourceMapEdge, FlowSourceMapEdgeKind, FlowSourceMapFlow, FlowSourceMapFormat,
    FlowSourceMapSourceLocation, FlowSourceMapStep, FlowSourceMapStepKind, LimitProfileRef,
    PropertyTypeName, ResourceLimitProfile,
};
use serde_json::{Value, json};

const CATALOG: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/commerce.property-types.entity.json");
const INTERFACE: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/party.interface.entity.json");
const SUPPLIER: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/purchasing-supplier.entity.json");
const SKU: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/retail-sku.entity.json");
const SKU_V1: &str = include_str!("../../../fixtures/ir/retail-sku.entity.json");
const EXTENSION: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/acme-sku.extension.entity.json");
const UNKNOWN_SUPERTYPE: &str = include_str!(
    "../../../fixtures/ir/entity-v2/artifact-set/invalid/unknown-supertype.entity.json"
);
const FUNCTION_NODE_RULE: &str =
    include_str!("../../../fixtures/rule-ir-v2/invalid/function-node.rule.json");
const PURCHASE_ORDER_ENTITY: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/commerce-purchase-order.entity.json");
const REORDER_DECISION_RULE: &str =
    include_str!("../../../fixtures/rule-ir-v2/valid/reorder-decision.rule.json");
const PURCHASE_ORDER_APPROVAL_RULE: &str =
    include_str!("../../../fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json");

const UPDATE_ENV: &str = "NORN_CHECKER_UPDATE_GOLDENS";
const GOLDEN: &str = "tests/golden/check-receipt.retail.json";

/// Pinned content hash of the golden receipt. A change here means the
/// receipt's bytes changed: review the golden diff (a field or meaning change
/// is a new `RECEIPT_FORMAT`).
const RETAIL_RECEIPT_SHA256: &str =
    "c4f1873c8393a5148cc6b039bcb4e01355345f45e4d921ee55d0d0c8766b803e";

fn json_of(source: &str) -> Value {
    serde_json::from_str(source).expect("fixture should parse")
}

fn envelopes(values: &[Value]) -> Vec<ArtifactEnvelope> {
    values
        .iter()
        .cloned()
        .map(|value| serde_json::from_value(value).expect("envelope should decode leniently"))
        .collect()
}

fn registry_layer() -> Vec<Value> {
    [CATALOG, INTERFACE, SUPPLIER, SKU]
        .into_iter()
        .map(json_of)
        .collect()
}

fn check_with(
    artifacts: &[Value],
    base: &[Value],
    registry: &BTreeSet<PropertyTypeName>,
    tenant: Option<TenantLayer<'_>>,
    limits: &ResourceLimitProfile,
) -> CheckedArtifactSet {
    let base = envelopes(base);
    let context = EntityArtifactSetContext {
        registry,
        base: &base,
        tenant,
    };
    check_artifact_set_with_receipt(&envelopes(artifacts), &context, limits)
        .expect("receipt should be issued")
}

fn check(artifacts: &[Value]) -> CheckedArtifactSet {
    check_with(
        artifacts,
        &[],
        &BTreeSet::new(),
        None,
        &ResourceLimitProfile::norn_limits_v1(),
    )
}

fn receipt(artifacts: &[Value]) -> CheckReceipt {
    let checked = check(artifacts);
    assert!(
        checked.diagnostics.is_empty(),
        "fixture set should be clean: {:?}",
        checked.diagnostics
    );
    checked.receipt
}

fn env_sha(receipt: &CheckReceipt) -> String {
    receipt
        .type_env
        .as_ref()
        .expect("a clean set binds its environment")
        .sha256
        .clone()
}

fn compact(receipt: &CheckReceipt) -> String {
    String::from_utf8(receipt.canonical_json().expect("canonical receipt"))
        .expect("canonical receipt is UTF-8")
}

fn compact_value(value: &Value) -> String {
    String::from_utf8(norn_canonical::canonical_json_of(value).expect("canonical JSON"))
        .expect("canonical JSON is UTF-8")
}

fn flow_source_map(
    flow_id: &str,
    artifact: &ArtifactEnvelope,
    state_ids: Vec<String>,
    decision_row_ids: Vec<String>,
) -> FlowSourceMap {
    let artifact_ref = ArtifactRef::of(artifact).expect("artifact hash");
    FlowSourceMap {
        format: FlowSourceMapFormat::V1,
        flows: vec![FlowSourceMapFlow {
            flow_id: flow_id.to_owned(),
            flow_version: "source-v1".to_owned(),
            source_path: "flows/test.ts".to_owned(),
            artifact_refs: vec![FlowSourceMapArtifactRef {
                artifact_id: artifact_ref.artifact_id,
                version: artifact_ref.version,
                sha256: artifact_ref.sha256,
            }],
            steps: vec![FlowSourceMapStep {
                id: "step-1".to_owned(),
                kind: FlowSourceMapStepKind::Perform,
                at: Some(FlowSourceMapSourceLocation {
                    file: "flows/test.ts".to_owned(),
                    line: 1,
                    column: Some(1),
                }),
                artifact_ids: vec![artifact.artifact_id.clone()],
                state_ids,
                decision_row_ids,
            }],
            edges: Vec::new(),
        }],
    }
}

fn as_legacy_receipt(receipt: &CheckReceipt) -> (String, String) {
    let mut value: Value = serde_json::from_str(&compact(receipt)).expect("receipt JSON");
    value["format"] = json!("norn-checker/receipt@1");
    value
        .as_object_mut()
        .expect("receipt object")
        .remove("identifierProfile");
    let body = compact_value(&value);
    let digest = admission_receipt_sha256(&body).expect("legacy receipt hash");
    (body, digest)
}

fn empty_diagnostics_for(checker_version: &str) -> DiagnosticsDigest {
    let mut value = norn_checker::diagnostics_to_json(&[]);
    value["checkerVersion"] = json!(checker_version);
    let bytes = norn_canonical::canonical_json_of(&value).expect("canonical diagnostics");
    DiagnosticsDigest {
        count: 0,
        errors: 0,
        sha256: norn_canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes),
    }
}

fn diagnostics_for_version(
    diagnostics: &[norn_checker::Diagnostic],
    checker_version: &str,
) -> DiagnosticsDigest {
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == norn_ir::DiagnosticSeverity::Error)
        .count();
    let mut value = norn_checker::diagnostics_to_json(diagnostics);
    value["checkerVersion"] = json!(checker_version);
    let bytes = norn_canonical::canonical_json_of(&value).expect("canonical diagnostics");
    DiagnosticsDigest {
        count: u64::try_from(diagnostics.len()).expect("diagnostic count fits u64"),
        errors: u64::try_from(errors).expect("error count fits u64"),
        sha256: norn_canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes),
    }
}

fn pretty(bytes: &[u8]) -> String {
    let value: Value = serde_json::from_slice(bytes).expect("canonical JSON");
    let mut text = serde_json::to_string_pretty(&value).expect("pretty JSON");
    text.push('\n');
    text
}

#[test]
fn retail_receipt_matches_golden_and_pins_env_and_profiles() {
    let receipt = receipt(&registry_layer());
    let actual = pretty(&receipt.canonical_json().expect("canonical"));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::create_dir_all(path.parent().expect("golden directory"))
            .expect("golden directory should be creatable");
        std::fs::write(&path, &actual).expect("write golden");
    }
    let expected = std::fs::read_to_string(&path).expect("read golden");
    assert_eq!(
        actual, expected,
        "{GOLDEN} is stale; regenerate with `{UPDATE_ENV}=1 cargo test -p norn-checker --test check_receipt`"
    );
    assert_eq!(
        receipt.content_sha256().expect("hash"),
        RETAIL_RECEIPT_SHA256,
        "receipt bytes changed: review the golden and re-pin"
    );

    let value: Value = serde_json::from_str(&expected).expect("golden JSON");
    assert_eq!(value["format"], RECEIPT_FORMAT);
    assert_eq!(
        value["checker"],
        json!({ "name": CHECKER_NAME, "version": CHECKER_VERSION })
    );
    assert_eq!(value["exprProfile"], "norn-zen/6");
    assert_eq!(value["identifierProfile"], IDENTIFIER_ADMISSION_PROFILE);
    assert_eq!(EXPR_PROFILE, norn_expr::check::CHECKER_PROFILE);
    assert_eq!(value["typeEnv"]["id"], norn_expr::TYPE_ENV_ID);
    assert_eq!(value["typeEnv"]["version"], norn_expr::TYPE_ENV_VERSION);
    assert_eq!(value["admitted"], true);
    assert_eq!(value["diagnostics"]["count"], 0);
    assert_eq!(
        value["artifactSet"]["artifacts"].as_array().map(Vec::len),
        Some(4)
    );
    assert_eq!(receipt.limit_profile, LimitProfileRef::norn_limits_v1());
}

#[test]
fn flow_source_map_is_checked_bound_and_retained_with_receipt_v3() {
    let values = [
        json_of(PURCHASE_ORDER_ENTITY),
        json_of(PURCHASE_ORDER_APPROVAL_RULE),
    ];
    let artifacts = envelopes(&values);
    let map = flow_source_map(
        "purchase-order.approval",
        &artifacts[0],
        vec!["draft".to_owned()],
        Vec::new(),
    );
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let limits = ResourceLimitProfile::norn_limits_v1();
    let checked =
        check_artifact_set_with_flow_source_map(&artifacts, &context, &limits, Some(&map))
            .expect("receipt should be issued");
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let stored = compact(&checked.receipt);
    let receipt_json: Value = serde_json::from_str(&stored).expect("receipt JSON");
    assert_eq!(receipt_json["format"], "norn-checker/receipt@3");
    assert_eq!(
        receipt_json["flowSourceMap"],
        serde_json::to_value(&map).expect("typed map serializes")
    );
    let receipt_hash = checked.receipt.content_sha256().expect("receipt hash");
    let verified = verify_admission_receipt(&stored, &receipt_hash, &artifacts, &[], &limits)
        .expect("sidecar-bearing receipt verifies");
    assert_eq!(verified.flow_source_map(), Some(&map));
    let retained = norn_checker::check_artifact_set_with_verified_receipt(
        &artifacts, &context, &limits, &verified,
    )
    .expect("retained definitions re-admit");
    assert_eq!(
        retained.receipt.canonical_json().expect("receipt"),
        checked.receipt.canonical_json().expect("receipt")
    );

    let mut stale = map;
    stale.flows[0].artifact_refs[0].sha256 = "0".repeat(64);
    let refused =
        check_artifact_set_with_flow_source_map(&artifacts, &context, &limits, Some(&stale))
            .expect("stale map is a checker finding");
    assert!(!refused.receipt.admitted);
    assert!(refused.diagnostics.iter().any(|finding| {
        finding.code.as_str() == norn_checker::flow_source_map::codes::STALE_ARTIFACT
    }));

    let mut tampered: Value = serde_json::from_str(&stored).expect("receipt JSON");
    tampered["flowSourceMap"]["flows"][0]["artifactRefs"][0]["sha256"] = json!("0".repeat(64));
    let tampered_body = compact_value(&tampered);
    let tampered_hash = admission_receipt_sha256(&tampered_body).expect("updated receipt hash");
    assert!(matches!(
        verify_admission_receipt(&tampered_body, &tampered_hash, &artifacts, &[], &limits),
        Err(norn_checker::ReceiptError::InvalidStoredReceipt { .. })
    ));
}

#[test]
fn flow_source_map_checks_rule_rows_and_graph_references() {
    let rule_value = json_of(REORDER_DECISION_RULE);
    let rule = norn_checker::decode_artifact_json(&rule_value).expect("rule fixture decodes");
    let map = flow_source_map(
        "retail.reorder",
        &rule,
        Vec::new(),
        vec!["large".to_owned()],
    );
    assert!(
        norn_checker::validate_flow_source_map(&map, std::slice::from_ref(&rule), &[]).is_empty()
    );

    let mut invalid = map;
    invalid.flows[0].steps[0].decision_row_ids = vec!["missing-row".to_owned()];
    invalid.flows[0].edges.push(FlowSourceMapEdge {
        from: "step-1".to_owned(),
        to: "missing-step".to_owned(),
        kind: FlowSourceMapEdgeKind::Next,
        label: None,
    });
    let findings =
        norn_checker::validate_flow_source_map(&invalid, std::slice::from_ref(&rule), &[]);
    assert!(findings.iter().any(|finding| {
        finding.code.as_str() == norn_checker::flow_source_map::codes::MISSING_DECISION_ROW
    }));
    assert!(findings.iter().any(|finding| {
        finding.code.as_str() == norn_checker::flow_source_map::codes::INVALID_EDGE
    }));

    let entity = norn_checker::decode_artifact_json(&json_of(PURCHASE_ORDER_ENTITY))
        .expect("entity fixture decodes");
    let invalid_state = flow_source_map(
        "purchase-order.approval",
        &entity,
        vec!["missing-state".to_owned()],
        Vec::new(),
    );
    let findings =
        norn_checker::validate_flow_source_map(&invalid_state, std::slice::from_ref(&entity), &[]);
    assert!(findings.iter().any(|finding| {
        finding.code.as_str() == norn_checker::flow_source_map::codes::MISSING_STATE
    }));
}

#[test]
fn flow_source_map_containment_has_one_parent_and_no_cycles() {
    let rule = norn_checker::decode_artifact_json(&json_of(REORDER_DECISION_RULE))
        .expect("rule fixture decodes");
    let mut map = flow_source_map(
        "retail.reorder",
        &rule,
        Vec::new(),
        vec!["large".to_owned()],
    );
    let mut second = map.flows[0].steps[0].clone();
    second.id = "step-2".to_owned();
    let mut third = second.clone();
    third.id = "step-3".to_owned();
    map.flows[0].steps.extend([second, third]);
    let edge = |from: &str, to: &str| FlowSourceMapEdge {
        from: from.to_owned(),
        to: to.to_owned(),
        kind: FlowSourceMapEdgeKind::Contains,
        label: Some("body".to_owned()),
    };
    map.flows[0].edges = vec![edge("step-1", "step-2"), edge("step-2", "step-3")];
    assert!(
        norn_checker::validate_flow_source_map(&map, std::slice::from_ref(&rule), &[]).is_empty()
    );

    let mut cyclic = map.clone();
    cyclic.flows[0].edges.push(edge("step-3", "step-1"));
    let findings =
        norn_checker::validate_flow_source_map(&cyclic, std::slice::from_ref(&rule), &[]);
    assert!(findings.iter().any(|finding| {
        finding.code.as_str() == norn_checker::flow_source_map::codes::INVALID_EDGE
            && finding.message.contains("cycle")
    }));

    map.flows[0].edges.push(edge("step-1", "step-3"));
    let findings = norn_checker::validate_flow_source_map(&map, std::slice::from_ref(&rule), &[]);
    assert!(findings.iter().any(|finding| {
        finding.code.as_str() == norn_checker::flow_source_map::codes::INVALID_EDGE
            && finding.message.contains("only one")
    }));
}

#[test]
fn bound_env_is_the_derived_environment_of_the_set() {
    let artifacts = registry_layer();
    let receipt = receipt(&artifacts);
    let limits = ResourceLimitProfile::norn_limits_v1();
    let env = derive_type_env(&envelopes(&artifacts), &[], &limits).expect("environment");
    assert_eq!(receipt.type_env, Some(env.reference().expect("hash")));
    // Only `actor` is bound at the set level; sites bind `self`/`input`.
    assert_eq!(
        env.variables()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [norn_expr::ACTOR_VARIABLE]
    );
}

#[test]
fn changing_an_entity_definition_changes_the_recorded_env_hash() {
    let original = receipt(&registry_layer());

    // A new property on an object type.
    let mut added = registry_layer();
    added[3]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({ "id": "reorder_point", "type": { "kind": "integer" } }));
    // A property's type.
    let mut retyped = registry_layer();
    retyped[3]["payload"]["objectType"]["properties"][1] =
        json!({ "id": "title", "type": { "kind": "integer" } });
    // A link's cardinality.
    let mut relinked = registry_layer();
    relinked[3]["payload"]["objectType"]["links"][0]["cardinality"]["to"] = json!("many");
    // A shared property type in the catalog (feeds the environment through `ref`).
    let mut catalog = registry_layer();
    catalog[0]["payload"]["propertyTypes"][0]["type"] = json!({ "kind": "integer" });
    catalog[0]["payload"]["propertyTypes"][0]["constraints"] = json!({});

    for (label, set) in [
        ("added property", added),
        ("retyped property", retyped),
        ("link cardinality", relinked),
        ("catalog type", catalog),
    ] {
        let changed = receipt(&set);
        assert_ne!(env_sha(&changed), env_sha(&original), "{label}");
        assert_ne!(
            changed.artifact_set.sha256, original.artifact_set.sha256,
            "{label}"
        );
        assert_ne!(
            changed.content_sha256().expect("hash"),
            original.content_sha256().expect("hash"),
            "{label}"
        );
    }
}

#[test]
fn a_change_that_does_not_type_expressions_keeps_the_env_hash() {
    let original = receipt(&registry_layer());
    let mut described = registry_layer();
    described[3]["payload"]["objectType"]["description"] = json!("Reworded.");
    let changed = receipt(&described);
    assert_eq!(env_sha(&changed), env_sha(&original));
    // The set identity still records the change.
    assert_ne!(changed.artifact_set.sha256, original.artifact_set.sha256);
}

#[test]
fn receipt_is_independent_of_artifact_order() {
    let mut reversed = registry_layer();
    reversed.reverse();
    assert_eq!(receipt(&reversed), receipt(&registry_layer()));
}

#[test]
fn limit_profile_is_bound_and_sets_the_env_link_depth() {
    let mut limits = ResourceLimitProfile::norn_limits_v1();
    limits.version = 2;
    limits.slice.max_relation_depth = 1;
    let checked = check_with(&registry_layer(), &[], &BTreeSet::new(), None, &limits);
    let original = receipt(&registry_layer());
    assert_eq!(
        checked.receipt.limit_profile,
        limits.reference().expect("hash")
    );
    assert_ne!(checked.receipt.limit_profile, original.limit_profile);
    assert_ne!(env_sha(&checked.receipt), env_sha(&original));
}

#[test]
fn tenant_layer_env_includes_the_base_and_its_extension() {
    let namespace = ExtensionNamespace("acme".to_owned());
    let tenant = TenantLayer {
        tenant_id: "tenant_acme",
        namespace: &namespace,
    };
    let base = registry_layer();
    let checked = check_with(
        &[json_of(EXTENSION)],
        &base,
        &BTreeSet::new(),
        Some(tenant),
        &ResourceLimitProfile::norn_limits_v1(),
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    assert!(checked.receipt.admitted);
    assert_eq!(checked.receipt.artifact_set.artifacts.len(), 1);
    assert_eq!(checked.receipt.artifact_set.base.len(), 4);
    // The extension's members are part of the environment.
    assert_ne!(
        env_sha(&checked.receipt),
        env_sha(&receipt(&registry_layer()))
    );
}

#[test]
fn a_set_with_errors_binds_no_env_and_is_not_admitted() {
    let mut artifacts = registry_layer();
    artifacts.push(json_of(UNKNOWN_SUPERTYPE));
    let checked = check(&artifacts);
    assert!(!checked.diagnostics.is_empty());
    let receipt = checked.receipt;
    assert!(!receipt.admitted);
    assert_eq!(receipt.type_env, None);
    assert_eq!(
        receipt.diagnostics.count,
        u64::try_from(checked.diagnostics.len()).expect("count")
    );
    assert!(receipt.diagnostics.errors > 0);
    let value: Value =
        serde_json::from_slice(&receipt.canonical_json().expect("canonical")).expect("JSON");
    assert!(value.get("typeEnv").is_none());
    assert_eq!(value["admitted"], false);
}

#[test]
fn a_clean_set_whose_env_cannot_be_derived_is_rejected() {
    // `core.label` is a registry value type the checker knows only by name:
    // the model check resolves the `ref`, the environment cannot type it.
    let registry: BTreeSet<PropertyTypeName> = [PropertyTypeName("core.label".to_owned())].into();
    let mut artifacts = registry_layer();
    artifacts[3]["payload"]["objectType"]["properties"][1] =
        json!({ "id": "title", "type": { "kind": "ref", "name": "core.label" } });
    let checked = check_with(
        &artifacts,
        &[],
        &registry,
        None,
        &ResourceLimitProfile::norn_limits_v1(),
    );
    let codes_found: Vec<&str> = checked
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert_eq!(codes_found, [codes::ENVIRONMENT_UNDERIVABLE]);
    assert!(!checked.receipt.admitted);
    assert_eq!(checked.receipt.type_env, None);
}

#[test]
fn stored_receipts_verify_integrity_bindings_and_known_profiles() {
    let artifacts = registry_layer();
    let stored_envelopes = envelopes(&artifacts);
    let limits = ResourceLimitProfile::norn_limits_v1();
    let current = receipt(&artifacts);
    let stored = compact(&current);
    let sha256 = current.content_sha256().expect("receipt hash");

    let version = verify_admission_receipt(&stored, &sha256, &stored_envelopes, &[], &limits)
        .expect("current receipt should verify");
    assert_eq!(version.checker_version(), CHECKER_VERSION);
    assert_eq!(version.expr_profile(), EXPR_PROFILE);
    assert_eq!(version.receipt_format(), RECEIPT_FORMAT);
    assert_eq!(version.identifier_profile(), IDENTIFIER_ADMISSION_PROFILE);
    assert_eq!(
        admission_receipt_sha256(&stored).expect("stored hash"),
        sha256
    );

    for profile in ["norn-zen/3", "norn-zen/4", "norn-zen/5"] {
        let mut historical = current.clone();
        historical.checker_version = "0.1.0".to_owned();
        historical.expr_profile = profile.to_owned();
        historical.diagnostics = empty_diagnostics_for("0.1.0");
        let (historical_body, historical_hash) = as_legacy_receipt(&historical);
        let recovered =
            recover_legacy_admission_receipt(&historical_hash, &stored_envelopes, &[], &limits)
                .expect("known hash-only receipt should recover");
        assert_eq!(recovered, historical_body, "{profile}");
        let verified = verify_admission_receipt(
            &recovered,
            &historical_hash,
            &stored_envelopes,
            &[],
            &limits,
        )
        .expect("known historical receipt should verify");
        assert_eq!(verified.checker_version(), "0.1.0");
        assert_eq!(verified.expr_profile(), profile);
        assert_eq!(verified.receipt_format(), "norn-checker/receipt@1");
        assert_eq!(verified.identifier_profile(), "norn-identifiers/legacy");
    }
}

#[test]
fn stored_receipts_refuse_tampering_unknown_profiles_and_definition_drift() {
    let artifacts = registry_layer();
    let stored_envelopes = envelopes(&artifacts);
    let limits = ResourceLimitProfile::norn_limits_v1();
    let current = receipt(&artifacts);
    let stored = compact(&current);
    let sha256 = current.content_sha256().expect("receipt hash");

    let mut tampered: Value = serde_json::from_str(&stored).expect("receipt JSON");
    tampered["checker"]["version"] = json!("0.1.0");
    tampered["exprProfile"] = json!("norn-zen/5");
    let tampered = compact_value(&tampered);
    assert!(matches!(
        verify_admission_receipt(&tampered, &sha256, &stored_envelopes, &[], &limits),
        Err(norn_checker::ReceiptError::ReceiptHashMismatch { .. })
    ));

    let mut unknown = current;
    unknown.expr_profile = "norn-zen/99".to_owned();
    let unknown_body = compact(&unknown);
    let unknown_hash = unknown.content_sha256().expect("unknown hash");
    assert!(matches!(
        verify_admission_receipt(
            &unknown_body,
            &unknown_hash,
            &stored_envelopes,
            &[],
            &limits
        ),
        Err(norn_checker::ReceiptError::UnsupportedReceiptVersion { .. })
    ));

    let mut changed = artifacts;
    changed[3]["payload"]["objectType"]["description"] = json!("Definition drift.");
    let changed = envelopes(&changed);
    assert!(matches!(
        verify_admission_receipt(&stored, &sha256, &changed, &[], &limits),
        Err(norn_checker::ReceiptError::InvalidStoredReceipt { .. })
    ));
}

#[test]
fn historical_receipt_verification_does_not_rerun_current_admission() {
    let candidate_values = [json_of(FUNCTION_NODE_RULE)];
    let base_values = registry_layer();
    let candidate_envelopes = envelopes(&candidate_values);
    let base_envelopes = envelopes(&base_values);
    let limits = ResourceLimitProfile::norn_limits_v1();
    let registry = BTreeSet::new();
    let current_check = check_with(&candidate_values, &base_values, &registry, None, &limits);
    assert!(
        current_check.receipt.diagnostics.errors > 0,
        "fixture must be rejected by current admission"
    );

    // This body stands in for an older admitted receipt already authenticated
    // by the cell's stored digest. Verification checks its integrity and
    // bindings; it must not use the current checker's refusal as a reason to
    // reinterpret that historical attestation.
    let type_env = derive_type_env(&candidate_envelopes, &base_envelopes, &limits)
        .expect("the entity base still supplies its type environment")
        .reference()
        .expect("environment hash");
    let historical = CheckReceipt {
        checker: CHECKER_NAME.to_owned(),
        checker_version: "0.1.0".to_owned(),
        artifact_set: ArtifactSetRef::of_legacy(&candidate_envelopes, &base_envelopes)
            .expect("artifact set hash"),
        limit_profile: limits.reference().expect("limit hash"),
        expr_profile: "norn-zen/4".to_owned(),
        identifier_profile: "norn-identifiers/legacy".to_owned(),
        type_env: Some(type_env),
        diagnostics: empty_diagnostics_for("0.1.0"),
        admitted: true,
        flow_source_map: None,
    };
    let mut historical_value: Value =
        serde_json::from_slice(&historical.canonical_json().expect("canonical receipt"))
            .expect("historical receipt JSON");
    historical_value["format"] = json!("norn-checker/receipt@1");
    historical_value
        .as_object_mut()
        .expect("receipt object")
        .remove("identifierProfile");
    let stored = compact_value(&historical_value);
    let digest = admission_receipt_sha256(&stored).expect("receipt hash");
    let recovered =
        recover_legacy_admission_receipt(&digest, &candidate_envelopes, &base_envelopes, &limits)
            .expect("hash-only historical receipt should recover without re-admission");
    assert_eq!(recovered, stored);
    let verified = verify_admission_receipt(
        &stored,
        &digest,
        &candidate_envelopes,
        &base_envelopes,
        &limits,
    )
    .expect("stored historical receipt should be verified without re-admission");
    assert_eq!(verified.expr_profile(), "norn-zen/4");
}

#[test]
fn retained_warning_evidence_verifies_but_unreconstructable_legacy_hash_is_refused() {
    let candidate_values = [json_of(SKU_V1)];
    let base_values = [json_of(CATALOG), json_of(INTERFACE)];
    let limits = ResourceLimitProfile::norn_limits_v1();
    let checked = check_with(
        &candidate_values,
        &base_values,
        &BTreeSet::new(),
        None,
        &limits,
    );
    assert!(checked.receipt.admitted);
    assert_eq!(checked.receipt.diagnostics.errors, 0);
    let artifacts = envelopes(&candidate_values);
    let base = envelopes(&base_values);
    let warning = norn_checker::entity_v1_deprecation(&artifacts[0])
        .expect("v1 deprecation is a registered advisory finding");

    let mut historical = checked.receipt;
    historical.checker_version = "0.1.0".to_owned();
    historical.expr_profile = "norn-zen/4".to_owned();
    historical.diagnostics =
        diagnostics_for_version(&[norn_checker::Diagnostic::from_legacy(warning)], "0.1.0");
    let (historical_body, digest) = as_legacy_receipt(&historical);
    verify_admission_receipt(&historical_body, &digest, &artifacts, &base, &limits)
        .expect("retained warning digest is integrity evidence independent of current findings");
    // Artifact-set admission does not emit the BAC deprecation warning. A
    // hash alone cannot recover that historical finding, so it fails closed.
    assert!(recover_legacy_admission_receipt(&digest, &artifacts, &base, &limits).is_err());
}
