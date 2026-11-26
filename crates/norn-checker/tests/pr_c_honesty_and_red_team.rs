//! PR-C verification:
//! - LAW-HONESTY-1 (`Enforced` vs `DeclaredOnly` matching `governing_laws::supported`)
//! - RED-TEAM-1 (`norn-datalog` Scope Confinement, Unauthenticated External Settlement,
//!   Self-Ratifiable Transitions, and Unconstrained Caller Writes)

use norn_checker::law_honesty::{LawEnforcementStatus, LawProofStatus, classify_law_honesty};
use norn_checker::red_team::analyze_red_team;
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

fn envelope(kind: &str, artifact_id: &str, name: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": "hr",
        "name": name,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": { "orgId": "soliman", "environmentId": "prod" },
        "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
        "dependencies": [],
        "payload": payload
    }))
    .expect("envelope decodes")
}

fn law_envelope(
    id: &str,
    subject: &str,
    root_kind: &str,
    root_id: &str,
    modality: &str,
    extra: Value,
) -> ArtifactEnvelope {
    let mut payload = json!({
        "lawVersion": "0.1",
        "id": id,
        "owner": "hr",
        "severity": "error",
        "rationale": "Test law",
        "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
        "subject": subject,
        "scope": { "root": { "kind": root_kind, "id": root_id }, "objectTypes": [subject] },
        "modality": { "kind": modality },
        "response": { "kind": "predicate", "expression": "self.ok == true" }
    });
    if let (Some(dst), Some(src)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    envelope("law", &format!("{id}.law"), id, &payload)
}

fn transition_action(
    action_id: &str,
    object_kind: &str,
    lifecycle: &str,
    transition: &str,
    independent_of: &[&str],
) -> ArtifactEnvelope {
    let mut payload = json!({
        "actionId": action_id,
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        },
        "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        "writes": [{ "entityKind": object_kind, "change": "modified" }],
        "effectProgram": {
            "kind": "fire_transition",
            "objectKind": object_kind,
            "objectIdField": "id",
            "entityArtifactId": format!("{object_kind}.entity"),
            "entityArtifactVersion": "1.0.0",
            "lifecycle": lifecycle,
            "transition": transition
        }
    });
    if !independent_of.is_empty() {
        payload["independentOf"] = json!(independent_of);
    }
    envelope(
        "action",
        &format!("{action_id}.action"),
        action_id,
        &payload,
    )
}

fn payout_entity() -> ArtifactEnvelope {
    envelope(
        "entity",
        "hr.Payout.entity",
        "Payout",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "hr.Payout",
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "amount", "type": { "kind": "integer" } },
                    { "id": "late_minutes", "type": { "kind": "integer" } },
                    { "id": "state", "type": { "kind": "enum", "values": [{ "value": "requested" }, { "value": "dispatched" }, { "value": "succeeded" }] }, "default": "requested" },
                    { "id": "created_by", "type": { "kind": "text" }, "protected": true }
                ],
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "lifecycles": [{
                    "property": "state",
                    "initial": ["requested"],
                    "terminal": ["succeeded"],
                    "immutable": ["succeeded"],
                    "endings": [{ "state": "succeeded", "outcome": "completed" }],
                    "events": [{ "id": "confirm_succeeded", "ignoredIn": [{ "states": ["requested"], "reason": "not dispatched" }] }],
                    "transitions": [
                        { "id": "dispatch", "from": ["requested"], "to": "dispatched" },
                        { "id": "confirm_succeeded", "from": ["dispatched"], "to": "succeeded", "mode": "fact", "trigger": { "kind": "event", "event": "confirm_succeeded" } }
                    ]
                }]
            }
        }),
    )
}

fn payout_create_action() -> ArtifactEnvelope {
    envelope(
        "action",
        "hr.payout.create.action",
        "Create payout",
        &json!({
            "actionId": "hr.payout.create",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "string" },
                    "amount": { "type": "integer" },
                    "late_minutes": { "type": "integer" },
                    "created_by": { "type": "string" }
                },
                "required": ["id", "amount", "late_minutes", "created_by"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "derive": [{ "field": "created_by", "expression": "actor.subjectId", "inputRequired": false }],
            "preconditions": ["input.amount > 0"],
            "writes": [{ "entityKind": "hr.Payout", "change": "created" }],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectKind": "hr.Payout",
                "objectIdField": "id",
                "entityArtifactId": "hr.Payout.entity",
                "entityArtifactVersion": "1.0.0",
                "writeMode": { "kind": "create_only" }
            }
        }),
    )
}

fn payout_policy() -> ArtifactEnvelope {
    envelope(
        "policy",
        "hr.access.policy",
        "HR access",
        &json!({
            "accessVersion": "0.1",
            "policyId": "hr.access",
            "rules": [
                {
                    "ruleId": "bm_roster_scoped",
                    "actions": ["hr.roster.publish"],
                    "resourceType": "hr.Roster",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [
                        { "role": { "anyOf": ["branch_manager"] } },
                        { "resource": { "name": "branch_id", "op": "in", "value": { "principalAttribute": "managed_branches" } } }
                    ]
                },
                {
                    "ruleId": "bm_roster_unscoped_duplicate",
                    "actions": ["hr.roster.publish"],
                    "resourceType": "hr.Roster",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [{ "role": { "anyOf": ["branch_manager"] } }]
                },
                {
                    "ruleId": "bm_safe_departure_unscoped",
                    "actions": ["hr.safe_departure.set"],
                    "resourceType": "hr.SafeDepartureRule",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [{ "role": { "anyOf": ["branch_manager"] } }]
                },
                {
                    "ruleId": "owner_payout_all",
                    "actions": ["hr.payout.create", "hr.payout.dispatch"],
                    "resourceType": "hr.Payout",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [{ "role": { "anyOf": ["owner"] } }]
                }
            ]
        }),
    )
}

fn payout_fixture() -> Vec<ArtifactEnvelope> {
    let dispatch = transition_action("hr.payout.dispatch", "hr.Payout", "state", "dispatch", &[]);
    let confirm = transition_action(
        "hr.payout.confirm_succeeded",
        "hr.Payout",
        "state",
        "confirm_succeeded",
        &[],
    );
    vec![
        payout_entity(),
        payout_create_action(),
        dispatch,
        confirm,
        payout_policy(),
    ]
}

fn multi_lifecycle_order_entity() -> ArtifactEnvelope {
    envelope(
        "entity",
        "hr.Order.entity",
        "Order",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "hr.Order",
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "created_by", "type": { "kind": "text" }, "protected": true },
                    { "id": "requester", "type": { "kind": "text" } },
                    { "id": "approval_state", "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "submitted" }, { "value": "approved" }] }, "default": "draft" },
                    { "id": "fulfillment_state", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "done" }] }, "default": "open" }
                ],
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "lifecycles": [
                    {
                        "property": "approval_state",
                        "initial": ["draft"],
                        "immutable": ["approved"],
                        "endings": [{ "state": "approved", "outcome": "completed" }],
                        "transitions": [
                            { "id": "submit", "from": ["draft"], "to": "submitted" },
                            { "id": "auto_close", "from": ["submitted"], "to": "approved", "trigger": { "kind": "event", "event": "auto_approved" } }
                        ],
                        "events": [{ "id": "auto_approved", "payload": [] }]
                    },
                    {
                        "property": "fulfillment_state",
                        "initial": ["open"],
                        "transitions": [{ "id": "auto_close", "from": ["open"], "to": "done" }]
                    }
                ]
            }
        }),
    )
}

fn order_create_action() -> ArtifactEnvelope {
    envelope(
        "action",
        "hr.order.create.action",
        "Create order",
        &json!({
            "actionId": "hr.order.create",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "writes": [{ "entityKind": "hr.Order", "change": "created" }],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectKind": "hr.Order",
                "objectIdField": "id",
                "entityArtifactId": "hr.Order.entity",
                "entityArtifactVersion": "1.0.0",
                "writeMode": { "kind": "create_only" }
            }
        }),
    )
}

#[test]
fn law_honesty_distinguishes_enforced_from_declared_only() {
    let enforced = law_envelope(
        "hr.Payslip.exact_net",
        "hr.Payslip",
        "entity",
        "hr.Payslip",
        "always",
        json!({}),
    );
    let eventually = law_envelope(
        "hr.Payout.eventually_settled",
        "hr.Payout",
        "entity",
        "hr.Payout",
        "eventually",
        json!({}),
    );
    let with_exception = law_envelope(
        "hr.Shift.max_span",
        "hr.Shift",
        "entity",
        "hr.Shift",
        "always",
        json!({
            "exceptions": [{
                "id": "ramadan",
                "overrides": "hr.Shift.max_span",
                "condition": "self.is_ramadan == true",
                "rationale": "Seasonal rule",
                "source": { "artifactId": "hr.Shift.max_span.law", "pointer": "/payload/exceptions/0" }
            }]
        }),
    );
    let app_root = law_envelope(
        "hr.App.cross_entity",
        "hr.Payslip",
        "app",
        "hr",
        "always",
        json!({}),
    );

    let statuses = classify_law_honesty(&[enforced, eventually, with_exception, app_root]);
    assert_eq!(statuses.len(), 4);
    let by_id = |id: &str| statuses.iter().find(|s| s.law_id == id).expect("status");

    assert_eq!(
        by_id("hr.Payslip.exact_net").enforcement,
        LawEnforcementStatus::Enforced
    );
    assert_eq!(by_id("hr.Payslip.exact_net").proof, LawProofStatus::Unknown);
    assert_eq!(by_id("hr.Payslip.exact_net").reason, None);

    assert_eq!(
        by_id("hr.Payout.eventually_settled").enforcement,
        LawEnforcementStatus::DeclaredOnly
    );
    assert!(
        by_id("hr.Payout.eventually_settled")
            .reason
            .as_deref()
            .unwrap()
            .contains("temporal")
    );

    assert_eq!(
        by_id("hr.Shift.max_span").enforcement,
        LawEnforcementStatus::DeclaredOnly
    );
    assert!(
        by_id("hr.Shift.max_span")
            .reason
            .as_deref()
            .unwrap()
            .contains("exception")
    );

    assert_eq!(
        by_id("hr.App.cross_entity").enforcement,
        LawEnforcementStatus::DeclaredOnly
    );
    assert!(
        by_id("hr.App.cross_entity")
            .reason
            .as_deref()
            .unwrap()
            .contains("single-entity")
    );
}

#[test]
fn datalog_red_team_catches_branch_hop_unauth_settlement_self_ratify_and_unconstrained_write() {
    let report = analyze_red_team(&payout_fixture());
    assert!(
        report.is_complete(),
        "{:?} {:?}",
        report.evaluation_errors,
        report.decode_errors
    );

    assert_eq!(
        report.scope_confinement.len(),
        2,
        "{:?}",
        report.scope_confinement
    );
    assert!(
        report
            .scope_confinement
            .iter()
            .any(|f| f.rule_id == "bm_roster_unscoped_duplicate" && f.resource_type == "hr.Roster")
    );
    assert!(
        report
            .scope_confinement
            .iter()
            .any(|f| f.rule_id == "bm_safe_departure_unscoped"
                && f.resource_type == "hr.SafeDepartureRule")
    );

    assert_eq!(
        report.unauthenticated_settlements.len(),
        1,
        "{:?}",
        report.unauthenticated_settlements
    );
    assert_eq!(
        report.unauthenticated_settlements[0].action_id,
        "hr.payout.confirm_succeeded"
    );

    assert_eq!(
        report.self_ratifiable_transitions.len(),
        1,
        "{:?}",
        report.self_ratifiable_transitions
    );
    assert_eq!(
        report.self_ratifiable_transitions[0].action_id,
        "hr.payout.dispatch"
    );
    assert_eq!(report.self_ratifiable_transitions[0].role, "owner");

    assert_eq!(
        report.unconstrained_writes.len(),
        1,
        "{:?}",
        report.unconstrained_writes
    );
    assert_eq!(report.unconstrained_writes[0].property, "late_minutes");
}

#[test]
fn red_team_handles_multi_lifecycle_fact_trigger_and_non_identity_sod_fail_closed() {
    let order = multi_lifecycle_order_entity();
    let create = order_create_action();
    let submit = transition_action(
        "hr.order.submit",
        "hr.Order",
        "approval_state",
        "submit",
        &[],
    );
    let fulfill_close = transition_action(
        "hr.order.fulfill_close",
        "hr.Order",
        "fulfillment_state",
        "auto_close",
        &["created_by"],
    );
    let unauth_auto = transition_action(
        "hr.order.force_auto_approve",
        "hr.Order",
        "approval_state",
        "auto_close",
        &[],
    );
    let policy = envelope(
        "policy",
        "hr.order.policy",
        "Order policy",
        &json!({
            "accessVersion": "0.1",
            "policyId": "hr.order.policy",
            "rules": [{
                "ruleId": "creator_submit_bogus_sod",
                "actions": ["hr.order.create", "hr.order.submit"],
                "resourceType": "hr.Order",
                "scope": { "dimensions": ["org", "environment"] },
                "allOf": [
                    { "role": { "anyOf": ["clerk"] } },
                    { "resource": { "name": "created_by", "op": "ne", "value": { "principalAttribute": "department" } } }
                ]
            }]
        }),
    );

    let report = analyze_red_team(&[order, create, submit, fulfill_close, unauth_auto, policy]);
    assert!(
        report.is_complete(),
        "{:?} {:?}",
        report.evaluation_errors,
        report.decode_errors
    );
    assert!(
        report
            .unauthenticated_settlements
            .iter()
            .any(|f| f.action_id == "hr.order.force_auto_approve")
    );
    assert!(
        report
            .self_ratifiable_transitions
            .iter()
            .any(|f| f.action_id == "hr.order.submit" && f.role == "clerk")
    );
}

#[test]
fn red_team_rejects_unprotected_field_as_sod() {
    let order = multi_lifecycle_order_entity();
    let create = order_create_action();
    let submit = transition_action(
        "hr.order.submit",
        "hr.Order",
        "approval_state",
        "submit",
        &[],
    );
    // `requester` is NOT protected (caller-writable); `requester != principal.id` MUST NOT count as SoD!
    let policy = envelope(
        "policy",
        "hr.order.unprotected_sod.policy",
        "Unprotected SoD policy",
        &json!({
            "accessVersion": "0.1",
            "policyId": "hr.order.unprotected_sod",
            "rules": [{
                "ruleId": "clerk_unprotected_requester_sod",
                "actions": ["hr.order.create", "hr.order.submit"],
                "resourceType": "hr.Order",
                "scope": { "dimensions": ["org", "environment"] },
                "allOf": [
                    { "role": { "anyOf": ["clerk"] } },
                    { "resource": { "name": "requester", "op": "ne", "value": { "principalAttribute": "id" } } }
                ]
            }]
        }),
    );

    let report = analyze_red_team(&[order, create, submit, policy]);
    assert!(
        report.is_complete(),
        "{:?} {:?}",
        report.evaluation_errors,
        report.decode_errors
    );
    assert!(
        report
            .self_ratifiable_transitions
            .iter()
            .any(|f| f.action_id == "hr.order.submit" && f.role == "clerk"),
        "unprotected `requester` ne principal.id must not suppress self_ratifiable: {:?}",
        report.self_ratifiable_transitions
    );
}

#[test]
fn red_team_accepts_protected_creator_sod_predicate() {
    let order = multi_lifecycle_order_entity();
    let create = order_create_action();
    let submit = transition_action(
        "hr.order.submit",
        "hr.Order",
        "approval_state",
        "submit",
        &[],
    );
    let policy = envelope(
        "policy",
        "hr.order.protected_sod.policy",
        "Protected SoD policy",
        &json!({
            "accessVersion": "0.1",
            "policyId": "hr.order.protected_sod",
            "rules": [
                {
                    "ruleId": "clerk_create",
                    "actions": ["hr.order.create"],
                    "resourceType": "hr.Order",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [{ "role": { "anyOf": ["clerk"] } }]
                },
                {
                    "ruleId": "clerk_submit_with_protected_sod",
                    "actions": ["hr.order.submit"],
                    "resourceType": "hr.Order",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [
                        { "role": { "anyOf": ["clerk"] } },
                        { "resource": { "name": "created_by", "op": "ne", "value": { "principalAttribute": "id" } } }
                    ]
                }
            ]
        }),
    );

    let report = analyze_red_team(&[order, create, submit, policy]);
    assert!(
        report.is_complete(),
        "{:?} {:?}",
        report.evaluation_errors,
        report.decode_errors
    );
    assert!(
        report.self_ratifiable_transitions.is_empty(),
        "protected `created_by` ne principal.id should satisfy SoD: {:?}",
        report.self_ratifiable_transitions
    );
}
