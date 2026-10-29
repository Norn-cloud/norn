//! `AccessIR` → Cedar compile goldens and T2 admission codes (T5-AZ-1 #2576,
//! T5-AZ-2 #2577).

#![allow(
    clippy::expect_used,
    reason = "test helpers outside #[test] fns fail loudly on a broken fixture"
)]

use norn_authz::{AuthzCode, AuthzSchema, compile};
use norn_ir::access::AccessIr;
use serde_json::{Value, json};

const PROJECTED: &str = include_str!("fixtures/order.cedarschema");
const ORDER_ACCESS: &str = include_str!("fixtures/order_access.json");
const ORDER_ACCESS_GOLDEN: &str = include_str!("goldens/order_access.cedar");
const MARKING_SCHEMA: &str = r#"
namespace norn {
  entity Marking = {
    scope: Norn::Scope,
    id: String
  };
}
namespace Norn {
  action "norn.reveal" appliesTo {
    principal: [Actor],
    resource: [norn::Marking],
    context: { scope: Scope }
  };
  action "norn.declassify" appliesTo {
    principal: [Actor],
    resource: [norn::Marking],
    context: { scope: Scope }
  };
}
"#;

fn schema() -> AuthzSchema {
    AuthzSchema::from_projected_cedarschema(PROJECTED).expect("projected schema loads")
}

fn marking_schema() -> AuthzSchema {
    AuthzSchema::from_projected_cedarschema_fragments(&[PROJECTED, MARKING_SCHEMA])
        .expect("projected marking schema loads")
}

fn access(value: Value) -> AccessIr {
    serde_json::from_value(value).expect("AccessIR decodes")
}

fn rule(overrides: &Value) -> Value {
    let mut rule = json!({
        "ruleId": "r",
        "actions": ["commerce.order.view"],
        "resourceType": "commerce.SalesOrder",
        "scope": { "dimensions": ["org"] }
    });
    if let (Some(rule), Some(overrides)) = (rule.as_object_mut(), overrides.as_object()) {
        for (key, value) in overrides {
            rule.insert(key.clone(), value.clone());
        }
    }
    rule
}

fn policy(rules: &[Value]) -> AccessIr {
    access(json!({
        "accessVersion": "0.1",
        "policyId": "p",
        "rules": rules,
    }))
}

fn codes(access: &AccessIr) -> Vec<AuthzCode> {
    let error = compile(&schema(), access).expect_err("must be rejected");
    error.diagnostics().iter().map(|d| d.code).collect()
}

#[test]
fn order_access_matches_golden() {
    let access: AccessIr = serde_json::from_str(ORDER_ACCESS).expect("fixture decodes");
    let compiled = compile(&schema(), &access).expect("fixture compiles and validates");
    assert_eq!(compiled.to_cedar(), ORDER_ACCESS_GOLDEN);
    assert_eq!(
        compiled.policy_ids().collect::<Vec<_>>(),
        [
            "commerce.order_access/view_own_or_watched",
            "commerce.order_access/approve",
            "commerce.order_access/view_any_in_org",
        ]
    );
}

#[test]
fn compile_is_deterministic() {
    let access: AccessIr = serde_json::from_str(ORDER_ACCESS).expect("fixture decodes");
    let first = compile(&schema(), &access).expect("compiles").to_cedar();
    let second = compile(&schema(), &access).expect("compiles").to_cedar();
    assert_eq!(first, second);
}

#[test]
fn every_compiled_policy_carries_the_scope_conjunct() {
    let access: AccessIr = serde_json::from_str(ORDER_ACCESS).expect("fixture decodes");
    let cedar = compile(&schema(), &access).expect("compiles").to_cedar();
    let permits = cedar.matches("permit (").count();
    assert_eq!(permits, 3);
    assert_eq!(
        cedar
            .matches("principal.scope == context.scope &&\n  resource.scope == context.scope")
            .count(),
        permits
    );
    assert!(!cedar.contains("forbid"));
}

#[test]
fn json_projected_schema_loads() {
    let projected = json!({
        "commerce": {
            "entityTypes": {
                "SalesOrder": {
                    "shape": {
                        "type": "Record",
                        "attributes": {
                            "scope": { "type": "Norn::Scope" },
                            "customer": { "type": "Entity", "name": "Norn::Actor" }
                        }
                    }
                }
            },
            "actions": {}
        },
        "Norn": {
            "entityTypes": {
                "Actor": {
                    "memberOfTypes": ["Role"],
                    "shape": {
                        "type": "Record",
                        "attributes": { "scope": { "type": "Scope" } }
                    }
                }
            },
            "actions": {
                "commerce.order.view": {
                    "appliesTo": {
                        "principalTypes": ["Actor"],
                        "resourceTypes": ["commerce::SalesOrder"],
                        "context": {
                            "type": "Record",
                            "attributes": { "scope": { "type": "Scope" } }
                        }
                    }
                }
            }
        }
    });
    let schema = AuthzSchema::from_projected_json(&projected.to_string()).expect("JSON loads");
    let compiled = compile(&schema, &policy(&[rule(&json!({}))])).expect("compiles");
    assert_eq!(compiled.len(), 1);
}

#[test]
fn malformed_schema_fails_with_stable_codes() {
    let error = AuthzSchema::from_projected_cedarschema("namespace {").expect_err("parse error");
    assert!(error.has(AuthzCode::SchemaParse), "{error}");

    let error = AuthzSchema::from_projected_json("{ not json").expect_err("parse error");
    assert!(error.has(AuthzCode::SchemaParse), "{error}");

    // References an undeclared type: parses, does not compose.
    let error = AuthzSchema::from_projected_cedarschema(
        "namespace commerce { entity Order = { scope: Norn::Missing }; }",
    )
    .expect_err("does not compose");
    assert!(error.has(AuthzCode::SchemaInvalid), "{error}");

    // A projected principal type without `scope` cannot satisfy the scope
    // conjunct.
    let unscoped_actor = AuthzSchema::from_projected_cedarschema(
        "namespace commerce { entity SalesOrder = { scope: Norn::Scope }; }
         namespace Norn {
           entity Actor in [Role];
           action \"commerce.order.view\" appliesTo {
             principal: [Actor],
             resource: [commerce::SalesOrder],
             context: { scope: Scope }
           };
         }",
    )
    .expect("loads");
    let error = compile(&unscoped_actor, &policy(&[rule(&json!({}))]))
        .expect_err("principal.scope is undeclared");
    assert!(error.has(AuthzCode::PolicyInvalid), "{error}");

    // Redeclares a base type.
    let error = AuthzSchema::from_projected_cedarschema("namespace Norn { entity Role; }")
        .expect_err("duplicate");
    assert!(error.has(AuthzCode::SchemaInvalid), "{error}");
}

// T5-AZ-1 acceptance: a policy that is invalid against the schema fails T2
// with a stable code.
#[test]
fn policy_invalid_against_schema_fails_t2() {
    // Unknown action.
    let unknown_action = policy(&[rule(&json!({ "actions": ["commerce.order.delete"] }))]);
    assert!(codes(&unknown_action).contains(&AuthzCode::PolicyInvalid));

    // Unknown resource type.
    let unknown_type = policy(&[rule(&json!({ "resourceType": "commerce.Invoice" }))]);
    assert!(codes(&unknown_type).contains(&AuthzCode::PolicyInvalid));

    // A resource type without `scope` cannot satisfy the scope conjunct.
    let unscoped = policy(&[rule(&json!({
        "actions": ["commerce.note.read"],
        "resourceType": "commerce.Note"
    }))]);
    assert!(codes(&unscoped).contains(&AuthzCode::PolicyInvalid));

    // Unknown link role.
    let unknown_rel = policy(&[rule(&json!({
        "allOf": [{ "rel": { "from": "resource", "role": "approver" } }]
    }))]);
    assert!(
        codes(&unknown_rel)
            .iter()
            .any(|code| matches!(code, AuthzCode::PolicyInvalid | AuthzCode::PolicyLint))
    );

    // A principal-side link the projected principal type does not declare.
    let principal_rel = policy(&[rule(&json!({
        "allOf": [{ "rel": { "from": "principal", "role": "managers" } }]
    }))]);
    assert!(
        codes(&principal_rel)
            .iter()
            .any(|code| matches!(code, AuthzCode::PolicyInvalid | AuthzCode::PolicyLint))
    );

    // Ill-typed context comparison (`mfa` is Bool).
    let ill_typed = policy(&[rule(&json!({
        "actions": ["commerce.order.approve"],
        "allOf": [{ "context": { "name": "mfa", "op": "lt", "value": 3 } }]
    }))]);
    assert!(codes(&ill_typed).contains(&AuthzCode::PolicyInvalid));
}

#[test]
fn structural_findings_have_stable_codes() {
    let no_org = policy(&[rule(&json!({ "scope": { "dimensions": ["tenant"] } }))]);
    assert_eq!(codes(&no_org), [AuthzCode::AccessScopeOrgRequired]);

    let no_actions = policy(&[rule(&json!({ "actions": [] }))]);
    assert_eq!(codes(&no_actions), [AuthzCode::AccessActionsEmpty]);

    let duplicate = policy(&[rule(&json!({})), rule(&json!({}))]);
    assert_eq!(codes(&duplicate), [AuthzCode::AccessRuleDuplicate]);

    let empty_roles = policy(&[rule(&json!({ "allOf": [{ "role": { "anyOf": [] } }] }))]);
    assert_eq!(codes(&empty_roles), [AuthzCode::AccessRolesEmpty]);

    let reserved = policy(&[rule(&json!({
        "allOf": [{ "context": { "name": "scope", "op": "eq", "value": "x" } }]
    }))]);
    assert_eq!(codes(&reserved), [AuthzCode::AccessContextReserved]);

    let bad_type = policy(&[rule(&json!({ "resourceType": "commerce::SalesOrder" }))]);
    assert_eq!(codes(&bad_type), [AuthzCode::AccessNameInvalid]);

    let reserved_segment = policy(&[rule(&json!({ "resourceType": "in.SalesOrder" }))]);
    assert_eq!(codes(&reserved_segment), [AuthzCode::AccessNameInvalid]);

    let empty_id = access(json!({ "accessVersion": "0.1", "policyId": " ", "rules": [] }));
    assert_eq!(codes(&empty_id), [AuthzCode::AccessIdEmpty]);
}

#[test]
fn marking_condition_compiles_only_for_scoped_marking_actions() {
    let access = policy(&[rule(&json!({
        "actions": ["norn.reveal"],
        "resourceType": "norn.Marking",
        "allOf": [{ "marking": { "anyOf": ["pii.contact", "pii.health"] } }]
    }))]);
    let compiled = compile(&marking_schema(), &access).expect("marking policy compiles");
    assert!(compiled.to_cedar().contains(
        r#"(resource has "id" && (resource["id"] == "pii.contact" || resource["id"] == "pii.health"))"#
    ));

    let wrong_resource = policy(&[rule(&json!({
        "actions": ["norn.reveal"],
        "resourceType": "commerce.SalesOrder",
        "allOf": [{ "marking": { "anyOf": ["pii.contact"] } }]
    }))]);
    let error = compile(&marking_schema(), &wrong_resource).expect_err("wrong resource refused");
    assert!(error.has(AuthzCode::AccessMarkingResource), "{error}");

    let wrong_action = policy(&[rule(&json!({
        "actions": ["commerce.order.view"],
        "resourceType": "norn.Marking",
        "allOf": [{ "marking": { "anyOf": ["pii.contact"] } }]
    }))]);
    let error = compile(&marking_schema(), &wrong_action).expect_err("wrong action refused");
    assert!(error.has(AuthzCode::AccessMarkingAction), "{error}");

    let empty = policy(&[rule(&json!({
        "actions": ["norn.reveal"],
        "resourceType": "norn.Marking",
        "allOf": [{ "marking": { "anyOf": [] } }]
    }))]);
    let error = compile(&marking_schema(), &empty).expect_err("empty set refused");
    assert!(error.has(AuthzCode::AccessMarkingsEmpty), "{error}");

    let wildcard = policy(&[rule(&json!({
        "actions": ["norn.declassify"],
        "resourceType": "norn.Marking",
        "allOf": [{ "marking": { "anyOf": ["*"] } }]
    }))]);
    let error = compile(&marking_schema(), &wildcard).expect_err("wildcard refused");
    assert!(error.has(AuthzCode::AccessMarkingIdInvalid), "{error}");

    let spaced = policy(&[rule(&json!({
        "actions": ["norn.reveal"],
        "resourceType": "norn.Marking",
        "allOf": [{ "marking": { "anyOf": [" pii.contact"] } }]
    }))]);
    let error = compile(&marking_schema(), &spaced).expect_err("non-canonical id refused");
    assert!(error.has(AuthzCode::AccessMarkingIdInvalid), "{error}");
}

// T5-AZ-2 acceptance: an arbitrary ZEN Expression is rejected from AccessIR;
// only typed context values are admitted.
#[test]
fn arbitrary_expressions_are_rejected() {
    // No expression node exists in the closed grammar.
    for node in [
        json!({ "expression": "principal.role == 'admin' or true" }),
        json!({ "zen": "true" }),
        json!({ "cedar": "true" }),
        json!({ "context": { "name": "mfa", "op": "eq", "value": true, "expression": "true" } }),
        json!({ "context": { "name": "mfa", "op": "eq", "value": { "expression": "true" } } }),
        json!({ "context": { "name": "mfa", "op": "eq", "value": 1.5 } }),
        json!({ "context": { "name": "mfa", "op": "matches", "value": true } }),
    ] {
        let document = json!({
            "accessVersion": "0.1",
            "policyId": "p",
            "rules": [rule(&json!({ "allOf": [node] }))],
        });
        assert!(
            serde_json::from_value::<AccessIr>(document).is_err(),
            "{node} must not decode"
        );
    }

    // An expression smuggled into a context name is not an identifier.
    let smuggled = policy(&[rule(&json!({
        "actions": ["commerce.order.approve"],
        "allOf": [{ "context": { "name": "mfa == true || true", "op": "eq", "value": true } }]
    }))]);
    assert_eq!(codes(&smuggled), [AuthzCode::AccessNameInvalid]);

    // The scope conjunct is mandatory on the wire.
    let unscoped = json!({
        "accessVersion": "0.1",
        "policyId": "p",
        "rules": [{
            "ruleId": "r",
            "actions": ["commerce.order.view"],
            "resourceType": "commerce.SalesOrder"
        }]
    });
    assert!(serde_json::from_value::<AccessIr>(unscoped).is_err());
}

#[test]
fn empty_policy_compiles_to_an_empty_deny_all_set() {
    let compiled = compile(&schema(), &policy(&[])).expect("empty compiles");
    assert!(compiled.is_empty());
    assert_eq!(compiled.to_cedar(), "");
}

// The scope conjunct is a record comparison; it must not degrade into an
// equality of some other type a projection chose for `scope`.
#[test]
fn mistyped_scope_fails_admission() {
    for scope_type in ["String", "{ org: String }", "Long"] {
        let projected = format!(
            "namespace commerce {{ entity SalesOrder = {{ scope: {scope_type} }}; }}
             namespace Norn {{
               entity Actor in [Role] = {{ scope: {scope_type} }};
               action \"commerce.order.view\" appliesTo {{
                 principal: [Actor],
                 resource: [commerce::SalesOrder],
                 context: {{ scope: {scope_type} }}
               }};
             }}"
        );
        let schema = AuthzSchema::from_projected_cedarschema(&projected).expect("loads");
        let error = compile(&schema, &policy(&[rule(&json!({}))])).expect_err(scope_type);
        assert!(
            error
                .diagnostics()
                .iter()
                .all(|d| d.code == AuthzCode::SchemaScopeType),
            "{scope_type}: {error}"
        );
        assert!(
            error.has(AuthzCode::SchemaScopeType),
            "{scope_type}: {error}"
        );
    }
}

#[test]
fn directly_constructed_marking_conditions_require_the_canonical_id_grammar() {
    for id in [
        "PII/Contact",
        "PII.contact",
        "pii..contact",
        ".pii",
        "pii.",
        "1pii",
        "pii.1contact",
        "pii-contact",
        "pii.é",
        "pii contact",
    ] {
        let mut access = policy(&[rule(
            &json!({"actions":["norn.reveal"], "resourceType":"norn.Marking"}),
        )]);
        access.rules.first_mut().expect("rule").all_of =
            vec![norn_ir::access::AccessCondition::Marking(
                norn_ir::access::MarkingCondition {
                    any_of: vec![norn_ir::MarkingId(id.to_owned())],
                },
            )];
        let error = compile(&marking_schema(), &access).expect_err("noncanonical id refused");
        assert!(
            error.has(AuthzCode::AccessMarkingIdInvalid),
            "{id}: {error}"
        );
    }
    for id in ["pii", "pii.contact", "pii.contact_2", "a0"] {
        let access = policy(&[rule(
            &json!({"actions":["norn.reveal"], "resourceType":"norn.Marking", "allOf":[{"marking":{"anyOf":[id]}}]}),
        )]);
        compile(&marking_schema(), &access).expect("canonical id compiles");
    }
}
