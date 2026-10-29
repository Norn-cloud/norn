#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! The payload-root members that were generic JSON until T1-IR-2 (#2430,
//! listed on #2384) are typed, and the checker reports a malformed value at
//! the member under the kind's existing `*.validation.invalid_payload` code:
//!
//! | Kind | Member | Type |
//! | --- | --- | --- |
//! | entity (v1) | `overlaySources` | list of non-empty strings (may be empty) |
//! | setting | `value` | tagged by `valueType` (unchanged diagnostics) |
//! | rule | `sideEffects` | the literal `false` |
//! | rule | `requiredServices` | the empty list |
//! | policy (obligations form) | `appliesTo`, `reviewRequiredWhen` | non-empty string |
//! | policy (obligations form) | `approvalRoles` | non-empty list of non-empty strings |
//! | derived | `scopeRequired` | non-empty list of non-empty strings |
//!
//! Every fixture that was valid stays valid; see
//! `crates/norn-ir/tests/typed_payload_roots_fixtures.rs` for the pinned
//! hashes.

use norn_checker::validate_artifact;
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

const ENTITY_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-sku.entity.json");
const RULE_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-reorder-routing.rule.json");
const POLICY_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-procurement.policy.json");
const DERIVED_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-low-stock.derived.json");
const SETTING_FIXTURE: &str =
    include_str!("../../../fixtures/ir/retail-reorder-threshold.setting.json");

fn fixture(source: &str) -> Value {
    serde_json::from_str(source).expect("fixture should be JSON")
}

/// `(code, path, message)` of every diagnostic, or none when valid.
fn findings(artifact: Value) -> Vec<(String, String, String)> {
    let envelope: ArtifactEnvelope =
        serde_json::from_value(artifact).expect("envelope should decode leniently");
    validate_artifact(&envelope)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|diagnostic| {
            (
                diagnostic.code,
                diagnostic.path.unwrap_or_default(),
                diagnostic.message,
            )
        })
        .collect()
}

fn finding(code: &str, path: &str, message: &str) -> Vec<(String, String, String)> {
    vec![(code.to_owned(), path.to_owned(), message.to_owned())]
}

#[test]
fn fixtures_stay_valid() {
    for source in [
        ENTITY_FIXTURE,
        RULE_FIXTURE,
        POLICY_FIXTURE,
        DERIVED_FIXTURE,
        SETTING_FIXTURE,
    ] {
        assert_eq!(findings(fixture(source)), []);
    }
}

#[test]
fn entity_overlay_sources_are_customization_ids() {
    const CODE: &str = "entity.validation.invalid_payload";
    let mut entity = fixture(ENTITY_FIXTURE);
    for valid in [json!([]), json!(["customization.test.add_field"])] {
        entity["payload"]["overlaySources"] = valid;
        assert_eq!(findings(entity.clone()), []);
    }
    let cases = [
        (
            json!("customization.test.add_field"),
            "payload.overlaySources",
            "payload.overlaySources must be an array",
        ),
        (
            Value::Null,
            "payload.overlaySources",
            "payload.overlaySources must be an array",
        ),
        (
            json!(["customization.test.add_field", " "]),
            "payload.overlaySources[1]",
            "payload.overlaySources[1] must be a non-empty string",
        ),
    ];
    for (value, path, message) in cases {
        entity["payload"]["overlaySources"] = value;
        assert_eq!(findings(entity.clone()), finding(CODE, path, message));
    }
}

#[test]
fn rule_side_effect_members_admit_only_their_empty_values() {
    const CODE: &str = "rule.validation.invalid_payload";
    const SIDE_EFFECTS: &str = "sideEffects is not allowed in deterministic RuleIR";
    const SERVICES: &str = "requiredServices are not allowed in deterministic RuleIR";
    let legacy = json!({
        "irVersion": "0.1",
        "artifactId": "commerce.rule.discount_cap",
        "kind": "rule",
        "namespace": "commerce",
        "name": "Discount cap",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "ruleId": "commerce.discount_cap",
            "appliesTo": "order",
            "expression": "discount <= 0.3",
            "severity": "error"
        }
    });
    for (form, base) in [("modern", fixture(RULE_FIXTURE)), ("legacy", legacy)] {
        assert_eq!(findings(base.clone()), [], "{form}");
        let mut rule = base.clone();
        rule["payload"]["sideEffects"] = json!(false);
        rule["payload"]["requiredServices"] = json!([]);
        assert_eq!(findings(rule), [], "{form}: empty values");

        for value in [json!(true), json!("no"), Value::Null] {
            let mut rule = base.clone();
            rule["payload"]["sideEffects"] = value;
            assert_eq!(
                findings(rule),
                finding(CODE, "payload.sideEffects", SIDE_EFFECTS),
                "{form}"
            );
        }
        for value in [json!(["norn.stock"]), json!("norn.stock"), Value::Null] {
            let mut rule = base.clone();
            rule["payload"]["requiredServices"] = value;
            assert_eq!(
                findings(rule),
                finding(CODE, "payload.requiredServices", SERVICES),
                "{form}"
            );
        }
    }
}

#[test]
fn obligations_policy_types_the_approval_form_members() {
    const CODE: &str = "policy.validation.invalid_payload";
    let base = fixture(POLICY_FIXTURE);
    let mut policy = base.clone();
    policy["payload"]["appliesTo"] = json!("purchase_order");
    policy["payload"]["reviewRequiredWhen"] = json!("amount > 1000");
    policy["payload"]["approvalRoles"] = json!(["procurement_manager"]);
    assert_eq!(findings(policy), []);

    let cases = [
        (
            "appliesTo",
            json!(7),
            "payload.appliesTo",
            "payload.appliesTo must be a non-empty string",
        ),
        (
            "reviewRequiredWhen",
            Value::Null,
            "payload.reviewRequiredWhen",
            "payload.reviewRequiredWhen must be a non-empty string",
        ),
        (
            "approvalRoles",
            json!([]),
            "payload.approvalRoles",
            "payload.approvalRoles must be a non-empty array",
        ),
        (
            "approvalRoles",
            json!([""]),
            "payload.approvalRoles[0]",
            "payload.approvalRoles[0] must be a non-empty string",
        ),
    ];
    for (member, value, path, message) in cases {
        let mut policy = base.clone();
        policy["payload"][member] = value;
        assert_eq!(findings(policy), finding(CODE, path, message), "{member}");
    }
}

#[test]
fn derived_scope_required_is_scope_dimensions() {
    const CODE: &str = "derived.validation.invalid_payload";
    let mut view = fixture(DERIVED_FIXTURE);
    view["payload"]
        .as_object_mut()
        .expect("payload object")
        .remove("scopeRequired");
    assert_eq!(findings(view.clone()), [], "scopeRequired stays optional");

    let cases = [
        (
            Value::Null,
            "payload.scopeRequired",
            "payload.scopeRequired must be an array",
        ),
        (
            json!([]),
            "payload.scopeRequired",
            "payload.scopeRequired must be a non-empty array",
        ),
        (
            json!(["orgId", 7]),
            "payload.scopeRequired[1]",
            "payload.scopeRequired[1] must be a non-empty string",
        ),
    ];
    for (value, path, message) in cases {
        view["payload"]["scopeRequired"] = value;
        assert_eq!(findings(view.clone()), finding(CODE, path, message));
    }
}

/// The setting value was already checked against `valueType`; typing it as a
/// tagged union leaves those diagnostics byte-identical.
#[test]
fn setting_value_diagnostics_are_unchanged() {
    const CODE: &str = "setting.validation.invalid_payload";
    let cases = [
        (
            "text",
            json!(5),
            "payload.value must be a string for text settings",
        ),
        (
            "number",
            json!("5"),
            "payload.value must be a finite number for number settings",
        ),
        (
            "boolean",
            Value::Null,
            "payload.value must be a boolean for boolean settings",
        ),
    ];
    for (value_type, value, message) in cases {
        let mut setting = fixture(SETTING_FIXTURE);
        setting["payload"]["valueType"] = json!(value_type);
        setting["payload"]["value"] = value;
        assert_eq!(
            findings(setting),
            finding(CODE, "payload.value", message),
            "{value_type}"
        );
    }
    let mut setting = fixture(SETTING_FIXTURE);
    setting["payload"]["valueType"] = json!("json");
    setting["payload"]["value"] = json!({ "quietHours": null });
    assert_eq!(findings(setting), []);
}
