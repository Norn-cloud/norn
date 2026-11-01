#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "conformance assertions"
)]
use norn_checker::{EntityArtifactSetContext, check_artifact_set_with_receipt};
use norn_ir::{ArtifactEnvelope, ControlStatus, Reachability, ResourceLimitProfile};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn artifact(kind: &str, id: &str, payload: Value) -> ArtifactEnvelope {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"
    ))
    .expect("fixture");
    value["kind"] = json!(kind);
    value["artifactId"] = json!(id);
    value["payload"] = payload;
    serde_json::from_value(value).expect("envelope")
}
fn fixture(same_role: bool, guarded: bool, witness: bool) -> Vec<ArtifactEnvelope> {
    let schema =
        json!({"type":"object","properties":{},"required":[],"additionalProperties":false});
    let mut out = vec![artifact(
        "entity",
        "demo.Order",
        json!({
            "entityVersion":"2.0", "objectType": {"name":"demo.Order", "controls": {
                "format":"norn.controls/1", "sensitivePaths":[{
                    "objectType":"demo.Order", "actions":["demo.approve", "demo.pay"], "requireWitness": witness
                }]
            }}
        }),
    )];
    for action in ["demo.approve", "demo.pay"] {
        out.push(artifact(
            "action",
            action,
            json!({
                "actionId":action, "scopeRequired":["orgId"], "idempotent":true,
                "ledgered":false, "inputSchema":schema, "outputSchema":schema,
                "preconditions": if guarded {vec!["false"]} else {vec![]}
            }),
        ));
    }
    out.push(artifact("policy", "demo.access", json!({
        "accessVersion":"0.1", "policyId":"demo.access", "rules":[
            {"ruleId":"approve", "actions":["demo.approve"], "resourceType":"demo.Order",
             "scope":{"dimensions":["org"]}, "allOf":[{"role":{"anyOf":["reviewer"]}}]},
            {"ruleId":"pay", "actions":["demo.pay"], "resourceType":"demo.Order",
             "scope":{"dimensions":["org"]}, "allOf":[{"role":{"anyOf":[if same_role {"reviewer"} else {"payer"}]}}]}
        ]
    })));
    out
}
fn check(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> norn_checker::CheckedArtifactSet {
    check_artifact_set_with_receipt(
        artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("receipt")
}
#[test]
fn concrete_refutation_refuses_install() {
    let checked = check(&fixture(true, false, false), &[]);
    assert_eq!(checked.controls[0].verdict, ControlStatus::Refuted);
    assert_eq!(checked.controls[0].reachability, Reachability::Witnessed);
    assert!(!checked.controls[0].trace.is_empty());
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code.as_str() == "controls.validation.refuted")
    );
    assert!(!checked.receipt.admitted);
}
#[test]
fn abstract_counterexample_is_unknown_and_cannot_supply_required_witness() {
    let checked = check(&fixture(true, true, true), &[]);
    assert_eq!(checked.controls[0].verdict, ControlStatus::Unknown);
    assert_eq!(checked.controls[0].reachability, Reachability::Unknown);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code.as_str() == "controls.validation.witness_required")
    );
    assert!(!checked.receipt.admitted);
}
#[test]
fn separation_does_not_promote_abstract_reachability() {
    let checked = check(&fixture(false, true, false), &[]);
    assert_eq!(checked.controls[0].verdict, ControlStatus::Proven);
    assert_eq!(checked.controls[0].reachability, Reachability::Unknown);
}
#[test]
fn installed_controls_are_rechecked_when_grants_change() {
    let base = fixture(false, false, false);
    let mut broadened = fixture(true, false, false).pop().expect("policy");
    broadened.artifact_id = "demo.extra_access".into();
    let checked = check(&[broadened], &base);
    assert_eq!(checked.controls[0].verdict, ControlStatus::Refuted);
}

#[test]
fn subtype_cannot_drop_inherited_controls() {
    let mut artifacts = fixture(false, true, false);
    artifacts.push(artifact(
        "entity",
        "demo.Child",
        json!({
            "entityVersion":"2.0", "objectType":{"name":"demo.Child", "extends":"demo.Order"}
        }),
    ));
    let checked = check(&artifacts, &[]);
    assert_eq!(checked.controls.len(), 2);
    assert_eq!(checked.controls[0].object_type.as_str(), "demo.Child");
    assert_eq!(checked.controls[0].actions, checked.controls[1].actions);
    assert_eq!(checked.controls[0].verdict, ControlStatus::Proven);
}
