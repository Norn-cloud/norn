#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! T1-CHK-5 (#2387): the artifact-set resolver and the T3 entry point that
//! runs the per-kind set checks over the resolved set.
//!
//! The cases are data: `fixtures/ir/artifact-set/*.case.json` each name an
//! installed base, an optional tenant layer, and candidate artifacts (a
//! fixture path under `fixtures/`, optionally with top-level envelope members
//! replaced by `patch`), and list the exact `(artifactId, code, pointer)`
//! findings of [`check_artifact_set`]. Every resolution code has at least one
//! negative case, and valid sets have an empty expectation.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use norn_checker::artifact_set::codes;
use norn_checker::{
    EntityArtifactSetContext, TenantLayer, check_artifact_set, may_reference, resolve_artifact_set,
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, ExtensionNamespace, PropertyTypeName};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("fixture should be readable"))
        .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()))
}

/// A case artifact: a fixture path, or `{fixture, patch}`.
fn artifact(entry: &Value) -> ArtifactEnvelope {
    let value = match entry {
        Value::String(path) => read_json(&fixtures().join(path)),
        Value::Object(object) => {
            let path = object["fixture"].as_str().expect("fixture path");
            let mut value = read_json(&fixtures().join(path));
            if let Some(Value::Object(patch)) = object.get("patch") {
                for (key, member) in patch {
                    value[key] = member.clone();
                }
            }
            value
        }
        other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_)) => {
            panic!("unexpected case artifact {other}")
        }
    };
    serde_json::from_value(value).expect("envelope should decode")
}

fn artifacts(case: &Value, key: &str) -> Vec<ArtifactEnvelope> {
    case.get(key)
        .and_then(Value::as_array)
        .map(|entries| entries.iter().map(artifact).collect())
        .unwrap_or_default()
}

fn located(finding: norn_checker::Diagnostic) -> Vec<String> {
    vec![
        finding.artifact_id.unwrap_or_default(),
        finding.code.as_str().to_owned(),
        finding.span.map(|span| span.pointer).unwrap_or_default(),
    ]
}

/// `(artifactId, code, pointer)` of every [`check_artifact_set`] finding.
fn run(case: &Value) -> Vec<Vec<String>> {
    let base = artifacts(case, "base");
    let candidates = artifacts(case, "artifacts");
    // Registry (Norn Core) value type names the case installs.
    let registry: BTreeSet<PropertyTypeName> = case
        .get("registry")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .map(|name| PropertyTypeName(name.as_str().expect("registry name").to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let tenant = case.get("tenant").map(|tenant| {
        (
            tenant["tenantId"].as_str().expect("tenantId").to_owned(),
            ExtensionNamespace(tenant["namespace"].as_str().expect("namespace").to_owned()),
        )
    });
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &base,
        tenant: tenant.as_ref().map(|(tenant_id, namespace)| TenantLayer {
            tenant_id,
            namespace,
        }),
    };
    check_artifact_set(&candidates, &context)
        .into_iter()
        .map(located)
        .collect()
}

fn cases() -> Vec<(String, Value)> {
    let dir = fixtures().join("ir/artifact-set");
    let mut out: Vec<(String, Value)> = fs::read_dir(&dir)
        .expect("case directory should be readable")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.to_string_lossy().ends_with(".case.json"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            (name, read_json(&path))
        })
        .collect();
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

#[test]
fn every_case_reports_exactly_its_expected_findings() {
    let cases = cases();
    assert!(cases.len() >= 12, "expected the artifact-set case fixtures");
    for (name, case) in &cases {
        let expected: Vec<Vec<String>> =
            serde_json::from_value(case["expect"].clone()).expect("expect is a list of triples");
        assert_eq!(run(case), expected, "{name}");
    }
}

#[test]
fn every_resolution_code_has_a_negative_case() {
    let covered: BTreeSet<String> = cases()
        .iter()
        .flat_map(|(_, case)| case["expect"].as_array().cloned().unwrap_or_default())
        .filter_map(|triple| triple[1].as_str().map(str::to_owned))
        .collect();
    for code in [
        codes::DUPLICATE_ARTIFACT,
        codes::CONFLICTING_VERSION,
        codes::SHADOWS_BASE,
        codes::MISSING_DEPENDENCY,
        codes::VERSION_MISMATCH,
        codes::KIND_MISMATCH,
        codes::KIND_INCOMPATIBLE,
        codes::DEPENDENCY_CYCLE,
    ] {
        assert!(covered.contains(code), "no negative case for {code}");
        assert!(
            norn_checker::codes::lookup(code)
                .is_some_and(|entry| entry.tier == norn_checker::Tier::T3),
            "{code} is not a registered T3 code"
        );
    }
}

fn case_named(name: &str) -> Value {
    read_json(&fixtures().join("ir/artifact-set").join(name))
}

#[test]
fn a_valid_set_resolves_in_installation_order() {
    let case = case_named("valid-retail-set.case.json");
    let candidates = artifacts(&case, "artifacts");
    let resolved = resolve_artifact_set(&candidates, &[]).expect("the set resolves");
    let order: Vec<&str> = resolved
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    assert_eq!(order.len(), candidates.len());
    let position = |id: &str| {
        order
            .iter()
            .position(|other| *other == id)
            .unwrap_or_else(|| panic!("{id} is resolved"))
    };
    // Dependencies come first.
    for dependent in [
        "retail.low_stock.view",
        "retail.sku.reorder_point.customization",
        "retail.pack.core",
    ] {
        assert!(
            position("retail.Sku.entity") < position(dependent),
            "{dependent}"
        );
    }
    assert!(position("retail.reorder.action") < position("retail.pack.core"));
    assert!(resolved.base_dependencies.is_empty());

    // Deterministic regardless of input order.
    let mut reversed = candidates.clone();
    reversed.reverse();
    let again = resolve_artifact_set(&reversed, &[]).expect("the set resolves");
    let again: Vec<&str> = again
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    assert_eq!(again, order);
}

#[test]
fn base_dependencies_are_reported_once_and_the_base_is_not_resolved() {
    let case = case_named("valid-tenant-layer.case.json");
    let base = artifacts(&case, "base");
    let mut candidates = artifacts(&case, "artifacts");
    // A second reference to the same base artifact.
    let mut again = candidates[0].clone();
    again.artifact_id = "acme.retail.Sku.extension.labels.v2".to_owned();
    candidates.push(again);
    let resolved = resolve_artifact_set(&candidates, &base).expect("the layer resolves");
    let base_ids: Vec<&str> = resolved
        .base_dependencies
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    assert_eq!(base_ids, ["retail.Sku.entity.v2"]);
}

#[test]
fn per_kind_set_checks_run_only_on_a_resolved_set() {
    let registry_layer_with = |extra: Value| -> Vec<Vec<String>> {
        let case = json!({
            "base": [],
            "artifacts": [
                "ir/entity-v2/artifact-set/commerce.property-types.entity.json",
                "ir/entity-v2/artifact-set/party.interface.entity.json",
                "ir/entity-v2/artifact-set/purchasing-supplier.entity.json",
                "ir/entity-v2/artifact-set/retail-sku.entity.json",
                "ir/entity-v2/artifact-set/invalid/unknown-supertype.entity.json",
                extra,
            ],
        });
        run(&case)
    };
    // Resolved: the entity whole-model check reports the unknown supertype.
    let resolved = registry_layer_with(json!("ir/retail-reorder.action.json"));
    assert!(
        resolved
            .iter()
            .any(|finding| finding[0] == "retail.SeasonalSku.entity.v2"
                && finding[1].starts_with("object_type.")),
        "{resolved:?}"
    );
    // Unresolved (a dangling dependency): only resolution and per-artifact
    // findings, no set findings.
    let unresolved = registry_layer_with(json!("ir/retail-low-stock.derived.json"));
    assert_eq!(
        unresolved,
        [vec![
            "retail.low_stock.view".to_owned(),
            codes::MISSING_DEPENDENCY.to_owned(),
            "/dependencies/0".to_owned(),
        ]]
    );
}

#[test]
fn per_artifact_findings_are_reported_when_resolution_fails() {
    let case = json!({
        "artifacts": [
            {"fixture": "ir/retail-low-stock.derived.json", "patch": {"irVersion": "9.9"}},
        ],
    });
    let findings = run(&case);
    assert!(
        findings
            .iter()
            .any(|finding| finding[1] == "ir.validation.unsupported_version"),
        "{findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding[1] == codes::MISSING_DEPENDENCY),
        "{findings:?}"
    );
}

#[test]
fn a_longer_cycle_is_reported_once_and_its_dependents_are_not() {
    let dep = |id: &str, kind: &str| json!([{"artifactId": id, "kind": kind, "version": "0.1.0", "required": true}]);
    let case = json!({
        "artifacts": [
            {"fixture": "ir/retail-reorder.action.json", "patch": {"dependencies": dep("retail.procurement.policy", "policy")}},
            {"fixture": "ir/retail-procurement.policy.json", "patch": {"dependencies": dep("retail.reorder_routing.rule", "rule")}},
            {"fixture": "ir/retail-reorder-routing.rule.json", "patch": {"dependencies": dep("retail.reorder_routing.rule.next", "rule")}},
            {"fixture": "ir/retail-reorder-routing.rule.json", "patch": {
                "artifactId": "retail.reorder_routing.rule.next",
                "dependencies": dep("retail.reorder.action", "action"),
            }},
            // Behind the cycle, not on it.
            {"fixture": "ir/retail-replenishment.widget.json", "patch": {"dependencies": dep("retail.reorder.action", "action")}},
        ],
    });
    let findings = run(&case);
    let cycles: Vec<&Vec<String>> = findings
        .iter()
        .filter(|finding| finding[1] == codes::DEPENDENCY_CYCLE)
        .collect();
    assert_eq!(cycles.len(), 1, "{findings:?}");
    assert_eq!(cycles[0][0], "retail.procurement.policy");
}

#[test]
fn kind_compatibility_is_layered() {
    use ArtifactKind::{
        Action, BusinessPack, Connector, Customization, Derived, Entity, Policy, Relation, Rule,
        Setting, Widget,
    };
    let all = [
        Entity,
        Relation,
        Derived,
        Setting,
        Rule,
        Action,
        Connector,
        Policy,
        Widget,
        Customization,
        BusinessPack,
    ];
    for to in &all {
        assert!(may_reference(&BusinessPack, to), "business_pack -> {to:?}");
        assert_eq!(
            may_reference(&Customization, to),
            *to != BusinessPack,
            "customization -> {to:?}"
        );
        // Every kind may reference the data model's entities.
        assert!(may_reference(to, &Entity), "{to:?} -> entity");
    }
    // The data model references only the data model.
    for to in &all {
        assert_eq!(
            may_reference(&Entity, to),
            *to == Entity,
            "entity -> {to:?}"
        );
    }
    assert!(!may_reference(&Relation, &Action));
    assert!(!may_reference(&Derived, &Action));
    // Decisions emit commands that invoke actions (2026-09-25 §3/§4). A rule
    // may also bind a connector for explicitly declared guarded source commands.
    assert!(may_reference(&Rule, &Action));
    assert!(may_reference(&Rule, &Connector));
    assert!(!may_reference(&Rule, &Policy));
    assert!(!may_reference(&Widget, &Customization));
    assert!(!may_reference(&Action, &Widget));
}

/// T3-DEC-1: each `RuleIR` v2 sub-decision set code has a negative case.
#[test]
fn rule_ir_v2_sub_decision_codes_have_negative_cases() {
    use norn_checker::jdm_profile::codes as profile;
    let covered: BTreeSet<String> = cases()
        .iter()
        .flat_map(|(_, case)| case["expect"].as_array().cloned().unwrap_or_default())
        .filter_map(|triple| triple[1].as_str().map(str::to_owned))
        .collect();
    for code in [profile::SUB_DECISION_TARGET, profile::SUB_DECISION_BOUND] {
        assert!(covered.contains(code), "{code} has no negative case");
    }
}

/// T1-CHK-3 (#2526, #2417): each scope, identity, effect-target, and
/// write-set code of the set check has a negative case, and is a registered
/// T3 code.
#[test]
fn set_check_codes_have_negative_cases() {
    use norn_checker::action_set::codes as action;
    let mut covered: BTreeSet<String> = cases()
        .iter()
        .flat_map(|(_, case)| case["expect"].as_array().cloned().unwrap_or_default())
        .filter_map(|triple| triple[1].as_str().map(str::to_owned))
        .collect();
    // The generated-id create contract has a focused dynamic negative case in
    // `lifecycle_writes.rs`, where the entity and action schemas are built in
    // memory rather than represented by an artifact-set fixture.
    covered.insert(action::NAMING_SERIES_TARGET_MISMATCH.to_owned());
    for code in [
        codes::INCOMPATIBLE_SCOPE,
        action::DUPLICATE_OBJECT_TYPE,
        action::DUPLICATE_ACTION_ID,
        action::UNRESOLVED_EFFECT_ENTITY,
        action::EFFECT_KIND_MISMATCH,
        action::INVALID_WRITE_MODE,
        action::NAMING_SERIES_TARGET_MISMATCH,
        action::UNKNOWN_WRITE_KIND,
        action::TARGET_NOT_WRITABLE,
        action::UNDECLARED_EFFECT_WRITE,
        action::WRITE_WITHOUT_EFFECT,
    ] {
        assert!(covered.contains(code), "{code} has no negative case");
        assert!(
            norn_checker::codes::lookup(code)
                .is_some_and(|entry| entry.tier == norn_checker::Tier::T3),
            "{code} is not a registered T3 code"
        );
    }
}

/// T3-LIFE-2 (#2673): every lifecycle-write code of a decision or an action
/// has a negative artifact-set case, and the write hook's
/// `lifecycle.validation.undeclared_write` is reached from a decide graph.
#[test]
fn lifecycle_write_codes_have_negative_cases() {
    use norn_checker::lifecycle_writes::codes as writes;
    let covered: BTreeSet<String> = cases()
        .iter()
        .flat_map(|(_, case)| case["expect"].as_array().cloned().unwrap_or_default())
        .filter_map(|triple| triple[1].as_str().map(str::to_owned))
        .collect();
    for code in [
        writes::UNKNOWN_FACT_OBJECT_TYPE,
        writes::DIRECT_WRITE,
        writes::NOT_A_LIFECYCLE,
        writes::UNKNOWN_TRANSITION,
        writes::UNCONSTRAINED_STATE,
        writes::DELETE_LIFECYCLE_OBJECT,
        writes::FACT_LITERAL_OUTSIDE_SCHEMA,
        writes::UNDECLARED_TRANSITION_LITERAL,
        writes::UNRESOLVED_LIFECYCLES,
        norn_checker::lifecycles::codes::UNDECLARED_WRITE,
    ] {
        assert!(covered.contains(code), "{code} has no negative case");
        assert!(
            norn_checker::codes::lookup(code)
                .is_some_and(|entry| entry.tier == norn_checker::Tier::T3),
            "{code} is not a registered T3 code"
        );
    }
    let direct_writers: BTreeSet<String> = cases()
        .iter()
        .flat_map(|(_, case)| case["expect"].as_array().cloned().unwrap_or_default())
        .filter(|triple| triple[1] == writes::DIRECT_WRITE)
        .filter_map(|triple| triple[0].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        direct_writers,
        BTreeSet::from([
            "commerce.purchase_order_approval.rule".to_owned(),
            "commerce.purchase_order.upsert.action".to_owned(),
            "commerce.sales_order.submit_invoice.action".to_owned(),
        ]),
        "a direct lifecycle write is rejected from a decision, an upsert, and a transition action"
    );
    // T3-LIFE-3 (#2867): a transition action's lifecycle and transition id
    // are checked like a decision's, each with its own negative case.
    let transition_actions: BTreeSet<(String, String)> = cases()
        .iter()
        .flat_map(|(_, case)| case["expect"].as_array().cloned().unwrap_or_default())
        .filter(|triple| triple[0] == "commerce.sales_order.submit_invoice.action")
        .filter_map(|triple| {
            Some((
                triple[1].as_str()?.to_owned(),
                triple[2].as_str()?.to_owned(),
            ))
        })
        .collect();
    for (code, pointer) in [
        (writes::NOT_A_LIFECYCLE, "/payload/effectProgram/lifecycle"),
        (
            writes::UNKNOWN_TRANSITION,
            "/payload/effectProgram/transition",
        ),
        (
            writes::DIRECT_WRITE,
            "/payload/inputSchema/properties/status",
        ),
    ] {
        assert!(
            transition_actions.contains(&(code.to_owned(), pointer.to_owned())),
            "{code} at {pointer} has no transition-action case"
        );
    }
}

#[test]
fn a_narrower_scope_sees_a_broader_one_but_not_the_reverse() {
    use norn_checker::scope_conflict;
    use norn_ir::ScopeRef;
    let registry = ScopeRef {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
        ..ScopeRef::default()
    };
    let tenant = ScopeRef {
        tenant_id: Some("tenant_acme".to_owned()),
        ..registry.clone()
    };
    let other_tenant = ScopeRef {
        tenant_id: Some("tenant_beta".to_owned()),
        ..registry.clone()
    };
    let other_org = ScopeRef {
        org_id: Some("org_other".to_owned()),
        ..registry.clone()
    };
    assert_eq!(scope_conflict(&tenant, &registry), None);
    assert_eq!(scope_conflict(&registry, &registry), None);
    assert_eq!(
        scope_conflict(&registry, &tenant),
        Some(("tenantId", "tenant_acme", None))
    );
    assert_eq!(
        scope_conflict(&other_tenant, &tenant),
        Some(("tenantId", "tenant_acme", Some("tenant_beta")))
    );
    assert_eq!(
        scope_conflict(&tenant, &other_org),
        Some(("orgId", "org_other", Some("org_demo")))
    );
    // An unscoped artifact sees only unscoped ones.
    assert!(scope_conflict(&ScopeRef::default(), &registry).is_some());
}
