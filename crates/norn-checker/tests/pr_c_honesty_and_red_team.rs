#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "focused PR-C honesty and datalog red-team tests"
)]

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

#[test]
fn law_honesty_distinguishes_enforced_from_declared_only() {
    let enforced = envelope(
        "law",
        "hr.Payslip.exact_net.law",
        "Exact net",
        &json!({
            "lawVersion": "0.1",
            "id": "hr.Payslip.exact_net",
            "owner": "hr",
            "severity": "error",
            "rationale": "Exact math",
            "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
            "subject": "hr.Payslip",
            "scope": { "root": { "kind": "entity", "id": "hr.Payslip" }, "objectTypes": ["hr.Payslip"] },
            "modality": { "kind": "always" },
            "response": { "kind": "predicate", "expression": "self.net == self.base" }
        }),
    );
    let eventually = envelope(
        "law",
        "hr.Payout.eventually_settled.law",
        "Eventually settled",
        &json!({
            "lawVersion": "0.1",
            "id": "hr.Payout.eventually_settled",
            "owner": "hr",
            "severity": "error",
            "rationale": "Liveness",
            "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
            "subject": "hr.Payout",
            "scope": { "root": { "kind": "entity", "id": "hr.Payout" }, "objectTypes": ["hr.Payout"] },
            "modality": { "kind": "eventually" },
            "response": { "kind": "predicate", "expression": "self.state == 'succeeded'" }
        }),
    );
    let with_exception = envelope(
        "law",
        "hr.Shift.max_span.law",
        "Max span with exception",
        &json!({
            "lawVersion": "0.1",
            "id": "hr.Shift.max_span",
            "owner": "hr",
            "severity": "error",
            "rationale": "Labor law",
            "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
            "subject": "hr.Shift",
            "scope": { "root": { "kind": "entity", "id": "hr.Shift" }, "objectTypes": ["hr.Shift"] },
            "modality": { "kind": "always" },
            "response": { "kind": "predicate", "expression": "self.hours <= 10" },
            "exceptions": [{
                "id": "ramadan",
                "overrides": "hr.Shift.max_span",
                "condition": "self.is_ramadan == true",
                "rationale": "Seasonal rule",
                "source": { "artifactId": "hr.Shift.max_span.law", "pointer": "/payload/exceptions/0" }
            }]
        }),
    );

    let app_root = envelope(
        "law",
        "hr.App.cross_entity.law",
        "App root law",
        &json!({
            "lawVersion": "0.1",
            "id": "hr.App.cross_entity",
            "owner": "hr",
            "severity": "error",
            "rationale": "App-scoped rule",
            "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
            "subject": "hr.Payslip",
            "scope": { "root": { "kind": "app", "id": "hr" }, "objectTypes": ["hr.Payslip"] },
            "modality": { "kind": "always" },
            "response": { "kind": "predicate", "expression": "self.net == self.base" }
        }),
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
    let payout = envelope(
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
    );
    let create_payout = envelope(
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
    );
    // Missing `independentOf` -> self-ratifiable by `owner`!
    let dispatch_payout = envelope(
        "action",
        "hr.payout.dispatch.action",
        "Dispatch payout",
        &json!({
            "actionId": "hr.payout.dispatch",
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
            "writes": [{ "entityKind": "hr.Payout", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "hr.Payout",
                "objectIdField": "id",
                "entityArtifactId": "hr.Payout.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "state",
                "transition": "dispatch"
            }
        }),
    );
    // Fires `mode: "fact"` without `acceptReceipt` -> unauthenticated external settlement!
    let confirm_payout = envelope(
        "action",
        "hr.payout.confirm_succeeded.action",
        "Confirm payout",
        &json!({
            "actionId": "hr.payout.confirm_succeeded",
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
            "writes": [{ "entityKind": "hr.Payout", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "hr.Payout",
                "objectIdField": "id",
                "entityArtifactId": "hr.Payout.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "state",
                "transition": "confirm_succeeded"
            }
        }),
    );
    // `branch_manager` is scoped by `managed_branches` on `hr.Roster`, but unscoped on `hr.SafeDepartureRule`!
    let policy = envelope(
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
    );

    let report = analyze_red_team(&[
        payout,
        create_payout,
        dispatch_payout,
        confirm_payout,
        policy,
    ]);
    assert!(
        report.is_complete(),
        "{:?} {:?}",
        report.evaluation_errors,
        report.decode_errors
    );

    // 1. Scope confinement catches BOTH branch hopping AND an unscoped duplicate rule (no masking by rule A)!
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

    // 2. Unauthenticated external settlement catches B2!
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

    // 3. Recursive lifecycle SoD catches self-ratifiable dispatch!
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

    // 4. Unconstrained-write report flags `late_minutes` (while `amount` is in preconditions & `created_by` is protected)!
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
    let order = envelope(
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
                        "transitions": [
                            { "id": "auto_close", "from": ["open"], "to": "done" }
                        ]
                    }
                ]
            }
        }),
    );

    let create = envelope(
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
    );
    let submit = envelope(
        "action",
        "hr.order.submit.action",
        "Submit order",
        &json!({
            "actionId": "hr.order.submit",
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
            "writes": [{ "entityKind": "hr.Order", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "hr.Order",
                "objectIdField": "id",
                "entityArtifactId": "hr.Order.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "approval_state",
                "transition": "submit"
            }
        }),
    );
    // Independent action on DIFFERENT lifecycle (`fulfillment_state`) with same transition ID `auto_close`
    // MUST NOT mask `approval_state`'s unguarded automatic `auto_close` transition!
    let fulfill_close = envelope(
        "action",
        "hr.order.fulfill_close.action",
        "Close fulfillment",
        &json!({
            "actionId": "hr.order.fulfill_close",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "independentOf": ["created_by"],
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "writes": [{ "entityKind": "hr.Order", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "hr.Order",
                "objectIdField": "id",
                "entityArtifactId": "hr.Order.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "fulfillment_state",
                "transition": "auto_close"
            }
        }),
    );
    // Action firing event-triggered transition (without explicit `mode: "fact"`) without acceptReceipt
    // MUST still be caught as an unauthenticated settlement.
    let unauth_auto = envelope(
        "action",
        "hr.order.force_auto_approve.action",
        "Force auto approve",
        &json!({
            "actionId": "hr.order.force_auto_approve",
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
            "writes": [{ "entityKind": "hr.Order", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "hr.Order",
                "objectIdField": "id",
                "entityArtifactId": "hr.Order.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "approval_state",
                "transition": "auto_close"
            }
        }),
    );
    // Policy with bogus non-identity SoD (`created_by != principal.department`) must NOT suppress SoD finding!
    let policy = envelope(
        "policy",
        "hr.order.policy",
        "Order policy",
        &json!({
            "policyId": "hr.order.policy",
            "rules": [
                {
                    "ruleId": "creator_submit_bogus_sod",
                    "actions": ["hr.order.create", "hr.order.submit"],
                    "resourceType": "hr.Order",
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [
                        { "role": { "anyOf": ["clerk"] } },
                        { "resource": { "name": "created_by", "op": "ne", "value": { "principalAttribute": "department" } } }
                    ]
                }
            ]
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
            .any(|f| f.action_id == "hr.order.force_auto_approve"),
        "expected event-triggered fact transition to be flagged: {:?}",
        report.unauthenticated_settlements
    );
    assert!(
        report
            .self_ratifiable_transitions
            .iter()
            .any(|f| f.action_id == "hr.order.submit" && f.role == "clerk"),
        "expected cross-lifecycle isolation + non-identity SoD rejection to flag submit: {:?}",
        report.self_ratifiable_transitions
    );
}
