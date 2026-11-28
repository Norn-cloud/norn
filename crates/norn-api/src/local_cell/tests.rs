//! Unit tests for `LocalCell` actor scoping, fail-closed `AccessIR` policy
//! evaluation, definitions/policy epoch atomicity, and base/app layer isolation.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, RwLock};

use norn_ir::access::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, AccessScope, CompareOp, ContextCondition,
    EntitlementMatchKind, MarkingCondition, PolicyRoot, PrincipalAttributeOperand, RelCondition,
    RelEndpoint, ResourceCompareOp, ResourceCondition, ResourceOperand, RoleCondition,
    ScopeDimension,
};
use norn_ir::{MarkingId, ObjectTypeName, RuntimeObject, RuntimeValue};
use norn_kernel::{ActionAuthzRequest, ActionDenial, FactKey};
use serde_json::json;
use serde_json::value::RawValue;

use crate::actions::ActionQuery;
use crate::authz::{AccessRequest, Authorizer as _, Operation};
use crate::refusal::Refusal;
use crate::scope::{CellScope, ScopeQuery};

use super::access_eval::PolicySnapshot;
use super::policy::{LocalPolicy, current_actor, with_actor};
use super::{DEFAULT_LOCAL_CLOCK_MS, LocalActor, LocalCell, LocalCellPacks};

type TestResult = Result<(), Box<dyn Error>>;

fn refusal_err(refusal: Refusal) -> String {
    format!(
        "{}: {} ({})",
        refusal.code, refusal.message, refusal.explanation
    )
}

fn demo_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".to_owned(),
        environment_id: "dev".to_owned(),
    }
}

fn demo_scope_map() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), "org_demo".to_owned()),
        ("environmentId".to_owned(), "dev".to_owned()),
    ])
}

fn raw_artifact(value: &serde_json::Value) -> Result<Box<RawValue>, Box<dyn Error>> {
    Ok(RawValue::from_string(value.to_string())?)
}

fn sample_entity_json() -> serde_json::Value {
    json!({
        "artifactId": "demo.Item.entity",
        "dependencies": [],
        "irVersion": "0.1",
        "kind": "entity",
        "lifecycle": "draft",
        "name": "Item",
        "namespace": "demo",
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "identity": [{ "name": "primary", "properties": ["item_id"] }],
                "name": "demo.Item",
                "properties": [
                    { "id": "item_id", "type": { "kind": "text" } },
                    { "id": "tier", "type": { "kind": "text" } },
                    { "id": "priority", "type": { "kind": "integer" } }
                ]
            }
        },
        "provenance": { "source": "authoring_sdk", "sourcePath": "demo.ts" },
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "version": "0.1.0"
    })
}

fn sample_action_json(access_artifact_id: Option<&str>) -> serde_json::Value {
    let mut payload = json!({
        "actionId": "demo.item.upsert",
        "effectProgram": {
            "entityArtifactId": "demo.Item.entity",
            "entityArtifactVersion": "0.1.0",
            "kind": "upsert_input_object",
            "objectIdField": "item_id",
            "objectKind": "demo.Item"
        },
        "idempotent": true,
        "inputSchema": {
            "additionalProperties": false,
            "properties": {
                "item_id": { "type": "string" },
                "tier": { "type": "string" },
                "priority": { "type": "integer" }
            },
            "required": ["item_id", "tier", "priority"],
            "type": "object"
        },
        "ledgered": true,
        "outputSchema": {
            "additionalProperties": false,
            "properties": { "item_id": { "type": "string" } },
            "required": ["item_id"],
            "type": "object"
        },
        "scopeRequired": ["orgId", "environmentId"],
        "title": "Upsert item",
        "writes": [
            { "change": "created", "entityKind": "demo.Item" },
            { "change": "modified", "entityKind": "demo.Item" }
        ]
    });
    if let (Some(policy_id), Some(obj)) = (access_artifact_id, payload.as_object_mut()) {
        obj.insert(
            "access".to_owned(),
            json!({ "artifactId": policy_id, "version": "0.1.0" }),
        );
    }
    json!({
        "artifactId": "demo.item.upsert.action",
        "dependencies": [],
        "irVersion": "0.1",
        "kind": "action",
        "lifecycle": "draft",
        "name": "Upsert item",
        "namespace": "demo",
        "payload": payload,
        "provenance": { "source": "authoring_sdk", "sourcePath": "demo.ts" },
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "version": "0.1.0"
    })
}

fn policy_artifact_json(policy: &AccessIr) -> Result<serde_json::Value, Box<dyn Error>> {
    Ok(json!({
        "artifactId": "demo.item.access.policy",
        "dependencies": [],
        "irVersion": "0.1",
        "kind": "policy",
        "lifecycle": "draft",
        "name": "Item access",
        "namespace": "demo",
        "payload": serde_json::to_value(PolicyRoot::Access(policy.clone()))?,
        "provenance": { "source": "authoring_sdk", "sourcePath": "demo.ts" },
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "version": "0.1.0"
    }))
}

fn build_policy_from_rule(
    rule: AccessRule,
) -> Result<(LocalPolicy, norn_ir::ActionRoot), Box<dyn Error>> {
    let access_ir = AccessIr::with_rules("demo.item.access", vec![rule]);
    let action_val = sample_action_json(Some("demo.item.access.policy"));
    let policy_val = policy_artifact_json(&access_ir)?;
    let canonical = serde_json::to_string(&json!([sample_entity_json(), action_val, policy_val]))?;
    let snapshot = PolicySnapshot::from_canonical_set(&canonical).map_err(refusal_err)?;
    let action_root: norn_ir::ActionRoot = serde_json::from_value(action_val["payload"].clone())?;
    let local_policy = LocalPolicy {
        scope: demo_scope(),
        policies: Arc::new(RwLock::new(snapshot)),
    };
    Ok((local_policy, action_root))
}

fn base_rule(all_of: Vec<AccessCondition>) -> AccessRule {
    AccessRule {
        rule_id: "r1".to_owned(),
        actions: vec!["demo.item.upsert".to_owned()],
        resource_type: ObjectTypeName("demo.Item".to_owned()),
        scope: AccessScope {
            dimensions: vec![ScopeDimension::Org, ScopeDimension::Environment],
        },
        all_of,
    }
}

#[test]
fn with_actor_swaps_and_restores_some_and_none() {
    let first = LocalActor::new("alice", vec!["member".to_owned()]);
    let second = LocalActor::new("bob", vec!["admin".to_owned()]);
    assert!(current_actor().is_none());
    with_actor(Some(&first), || {
        assert_eq!(current_actor(), Some(first.clone()));
        with_actor(Some(&second), || {
            assert_eq!(current_actor(), Some(second.clone()));
            with_actor(None, || {
                assert!(current_actor().is_none());
            });
            assert_eq!(current_actor(), Some(second.clone()));
        });
        assert_eq!(current_actor(), Some(first.clone()));
    });
    assert!(current_actor().is_none());
}

#[test]
fn policy_enforces_resource_type_and_scope_dimensions_fail_closed() -> TestResult {
    let actor = LocalActor::new("u1", vec!["editor".to_owned()]);
    let role_only = vec![AccessCondition::Role(RoleCondition {
        any_of: vec!["editor".to_owned()],
    })];

    let mut wrong_type_rule = base_rule(role_only.clone());
    wrong_type_rule.resource_type = ObjectTypeName("demo.Other".to_owned());
    let (policy, action) = build_policy_from_rule(wrong_type_rule)?;
    let scope_map = demo_scope_map();
    let input = RuntimeObject::new();
    let target = FactKey::new("demo.Item", "i-1");
    let req = ActionAuthzRequest {
        action: &action,
        scope: &scope_map,
        actor: "u1",
        input: &input,
        target: &target,
        target_values: None,
    };
    let verdict = with_actor(Some(&actor), || policy.action(&req));
    assert!(matches!(verdict, Err(ActionDenial::Forbidden { .. })));

    let mut tenant_rule = base_rule(role_only);
    tenant_rule.scope.dimensions.push(ScopeDimension::Tenant);
    let (tenant_policy, tenant_action) = build_policy_from_rule(tenant_rule)?;
    let pre = with_actor(Some(&actor), || {
        tenant_policy.authorize(&AccessRequest {
            scope: &demo_scope(),
            actor: Some("u1"),
            operation: Operation::CallAction("demo.item.upsert".to_owned()),
        })
    });
    assert!(
        pre.is_err(),
        "pre-storage must deny unsatisfied tenant scope"
    );
    let kernel_req = ActionAuthzRequest {
        action: &tenant_action,
        scope: &scope_map,
        actor: "u1",
        input: &input,
        target: &target,
        target_values: None,
    };
    let kernel = with_actor(Some(&actor), || tenant_policy.action(&kernel_req));
    assert!(matches!(kernel, Err(ActionDenial::Forbidden { .. })));
    Ok(())
}

#[test]
fn policy_evaluates_resource_literal_predicates_against_target_values() -> TestResult {
    let actor = LocalActor::new("u1", vec!["editor".to_owned()]);
    let rule = base_rule(vec![
        AccessCondition::Role(RoleCondition {
            any_of: vec!["editor".to_owned()],
        }),
        AccessCondition::Resource(ResourceCondition {
            name: "tier".to_owned(),
            op: ResourceCompareOp::Eq,
            value: ResourceOperand::Literal(AccessLiteral::String("gold".to_owned())),
        }),
        AccessCondition::Resource(ResourceCondition {
            name: "priority".to_owned(),
            op: ResourceCompareOp::Ge,
            value: ResourceOperand::Literal(AccessLiteral::Long(5)),
        }),
    ]);
    let (policy, action) = build_policy_from_rule(rule)?;
    let scope_map = demo_scope_map();
    let input = RuntimeObject::new();
    let target = FactKey::new("demo.Item", "i-1");

    let mut matching = RuntimeObject::new();
    matching.insert("tier".to_owned(), RuntimeValue::String("gold".to_owned()));
    matching.insert("priority".to_owned(), RuntimeValue::from(5_i64));
    let ok_req = ActionAuthzRequest {
        action: &action,
        scope: &scope_map,
        actor: "u1",
        input: &input,
        target: &target,
        target_values: Some(&matching),
    };
    assert!(with_actor(Some(&actor), || policy.action(&ok_req)).is_ok());

    let mut wrong = RuntimeObject::new();
    wrong.insert("tier".to_owned(), RuntimeValue::String("silver".to_owned()));
    wrong.insert("priority".to_owned(), RuntimeValue::from(5_i64));
    let bad_req = ActionAuthzRequest {
        action: &action,
        scope: &scope_map,
        actor: "u1",
        input: &input,
        target: &target,
        target_values: Some(&wrong),
    };
    assert!(matches!(
        with_actor(Some(&actor), || policy.action(&bad_req)),
        Err(ActionDenial::Forbidden { .. })
    ));

    let absent_req = ActionAuthzRequest {
        action: &action,
        scope: &scope_map,
        actor: "u1",
        input: &input,
        target: &target,
        target_values: None,
    };
    assert!(matches!(
        with_actor(Some(&actor), || policy.action(&absent_req)),
        Err(ActionDenial::Forbidden { .. })
    ));
    Ok(())
}

#[test]
fn policy_fails_closed_on_unverifiable_predicates() -> TestResult {
    let actor = LocalActor::new("u1", vec!["editor".to_owned()]);
    let role = AccessCondition::Role(RoleCondition {
        any_of: vec!["editor".to_owned()],
    });
    let link_id = serde_json::from_value(json!("owner"))?;
    let unverifiable = vec![
        (
            "rel",
            AccessCondition::Rel(RelCondition {
                from: RelEndpoint::Resource,
                role: link_id,
            }),
        ),
        (
            "context",
            AccessCondition::Context(ContextCondition {
                name: "risk_score".to_owned(),
                op: CompareOp::Lt,
                value: AccessLiteral::Long(10),
            }),
        ),
        (
            "marking",
            AccessCondition::Marking(MarkingCondition {
                any_of: vec![MarkingId("pii.email".to_owned())],
            }),
        ),
        (
            "entitlement",
            AccessCondition::Entitlement(EntitlementMatchKind::OwnerOnly),
        ),
        (
            "principalAttribute",
            AccessCondition::Resource(ResourceCondition {
                name: "tier".to_owned(),
                op: ResourceCompareOp::Eq,
                value: ResourceOperand::PrincipalAttribute(PrincipalAttributeOperand {
                    principal_attribute: "allowed_tier".to_owned(),
                }),
            }),
        ),
    ];
    let scope_map = demo_scope_map();
    let input = RuntimeObject::new();
    let target = FactKey::new("demo.Item", "i-1");
    for (label, extra_cond) in unverifiable {
        let rule = base_rule(vec![role.clone(), extra_cond]);
        let (policy, action) = build_policy_from_rule(rule)?;
        let pre = with_actor(Some(&actor), || {
            policy.authorize(&AccessRequest {
                scope: &demo_scope(),
                actor: Some("u1"),
                operation: Operation::CallAction("demo.item.upsert".to_owned()),
            })
        });
        assert!(pre.is_err(), "pre-storage must fail closed for {label}");
        let req = ActionAuthzRequest {
            action: &action,
            scope: &scope_map,
            actor: "u1",
            input: &input,
            target: &target,
            target_values: None,
        };
        let kernel = with_actor(Some(&actor), || policy.action(&req));
        assert!(
            matches!(kernel, Err(ActionDenial::Forbidden { .. })),
            "kernel callback must fail closed for {label}"
        );
    }
    Ok(())
}

#[test]
fn apply_definitions_refreshes_policy_atomically_for_newly_governed_action() -> TestResult {
    let initial_app = vec![
        raw_artifact(&sample_entity_json())?,
        raw_artifact(&sample_action_json(None))?,
    ];
    let cell = LocalCell::open_in_memory(
        demo_scope(),
        DEFAULT_LOCAL_CLOCK_MS,
        &LocalCellPacks {
            base_artifacts: &[],
            app_artifacts: &initial_app,
        },
    )
    .map_err(refusal_err)?;
    let member = LocalActor::new("m1", vec!["member".to_owned()]);
    let admin = LocalActor::new("a1", vec!["admin".to_owned()]);
    let query = ActionQuery {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
        version: None,
    };
    let body_1 = json!({
        "input": { "item_id": "i-1", "tier": "gold", "priority": 5 },
        "idempotencyKey": "k-1"
    })
    .to_string();
    assert!(
        cell.call_action(&query, Some(&member), "demo.item.upsert", &body_1)
            .is_ok()
    );

    let restrictive = AccessIr::with_rules(
        "demo.item.access",
        vec![base_rule(vec![AccessCondition::Role(RoleCondition {
            any_of: vec!["approver".to_owned()],
        })])],
    );
    let expected = cell
        .active_definitions_version()
        .map_err(refusal_err)?
        .unwrap_or(1);
    let apply_body = json!({
        "artifacts": [
            sample_entity_json(),
            sample_action_json(Some("demo.item.access.policy")),
            policy_artifact_json(&restrictive)?
        ],
        "expectedVersion": expected
    })
    .to_string();
    let scope_query = ScopeQuery {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
    };
    let _applied = cell
        .apply_definitions(&scope_query, Some(&admin), &apply_body)
        .map_err(refusal_err)?;

    let body_2 = json!({
        "input": { "item_id": "i-2", "tier": "gold", "priority": 5 },
        "idempotencyKey": "k-2"
    })
    .to_string();
    let denied = cell.call_action(&query, Some(&member), "demo.item.upsert", &body_2);
    assert!(
        denied.is_err(),
        "member must be denied immediately after apply"
    );

    let approver = LocalActor::new("ap1", vec!["approver".to_owned()]);
    let allowed = cell.call_action(&query, Some(&approver), "demo.item.upsert", &body_2);
    assert!(
        allowed.is_ok(),
        "approver must be allowed immediately after apply"
    );
    Ok(())
}

#[test]
fn local_cell_rejects_duplicate_base_ids_and_app_reserved_spoofing() -> TestResult {
    let reserved = json!({
        "artifactId": "core.spoof.entity",
        "dependencies": [],
        "irVersion": "0.1",
        "kind": "entity",
        "lifecycle": "active",
        "name": "Spoof",
        "namespace": "core",
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "name": "core.Spoof",
                "properties": [{ "id": "id", "type": { "kind": "text" } }]
            }
        },
        "provenance": { "source": "authoring_sdk", "sourcePath": "core.ts" },
        "scope": {},
        "version": "0.1.0"
    });
    let raw_reserved = raw_artifact(&reserved)?;

    let dup_base = vec![raw_reserved.clone(), raw_reserved.clone()];
    let dup_result = LocalCell::open_in_memory(
        demo_scope(),
        DEFAULT_LOCAL_CLOCK_MS,
        &LocalCellPacks {
            base_artifacts: &dup_base,
            app_artifacts: &[],
        },
    );
    assert!(dup_result.is_err(), "duplicate base artifactId must refuse");

    let spoof_app = vec![raw_reserved];
    let spoof_result = LocalCell::open_in_memory(
        demo_scope(),
        DEFAULT_LOCAL_CLOCK_MS,
        &LocalCellPacks {
            base_artifacts: &[],
            app_artifacts: &spoof_app,
        },
    );
    assert!(
        spoof_result.is_err(),
        "app artifact with reserved core.* id must never enter trusted base"
    );
    Ok(())
}
