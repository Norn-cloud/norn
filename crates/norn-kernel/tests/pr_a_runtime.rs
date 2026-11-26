//! Runtime tests for PR-A:
//! - ACTION-CLOCK-1 (`clock.nowIso` / `clock.nowMillis`, spoofed clock refusal)
//! - ACTION-SLICE-DERIVE-1 (post-slice `derive` on upsert and transition)
//! - TRANSITION-PATCH-1 (`fire_transition` `setFields` atomic update)

use std::collections::BTreeMap;

use norn_ir::{ActionRoot, ObjectTypeDef, RuntimeValue};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, ActionRefusal, CellHost, DecisionTurn, FactEvent, FactKey,
    LogicalTime, TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use norn_testkit::pack_mbt::action_call_object_type;
use serde_json::json;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

struct Allow;

impl norn_kernel::ActionAuthorizer for Allow {
    fn authorize(&self, _request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read(&self, _request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read_type(
        &self,
        _request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn payout_types() -> Vec<ObjectTypeDef> {
    let mut types: Vec<ObjectTypeDef> = serde_json::from_value(json!([{
        "name": "test.Payout",
        "properties": [
            { "id": "id", "type": { "kind": "text" } },
            { "id": "payslip_id", "type": { "kind": "text" }, "immutableAfterCreate": true },
            { "id": "generation", "type": { "kind": "integer" }, "origin": "derived", "default": "1" },
            { "id": "stamped_at", "type": { "kind": "timestamp", "timezone": "utc" }, "origin": "clock" },
            { "id": "state", "type": { "kind": "enum", "values": [{ "value": "requested" }, { "value": "failed" }] }, "default": "requested" },
            { "id": "created_by", "type": { "kind": "text" }, "protected": true, "origin": "actor" }
        ],
        "identity": [{ "name": "primary", "properties": ["id"] }],
        "lifecycles": [{
            "property": "state",
            "initial": ["requested"],
            "transitions": [
                { "id": "fail", "from": ["requested"], "to": "failed" },
                { "id": "retry", "from": ["failed"], "to": "requested" }
            ]
        }]
    }]))
    .unwrap();
    types.push(action_call_object_type());
    types
}

fn call<'a>(
    action: &'a ActionRoot,
    artifact_id: &'a str,
    input: JsonObject,
    idempotency_key: &str,
    now_ms: u64,
) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: artifact_id,
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_test".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "usr:owner".to_owned(),
        approved_intent: None,
        input,
        idempotency_key: idempotency_key.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn-{idempotency_key}"),
            now: LogicalTime::from_millis(now_ms),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

fn create_payout_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "test.payout.create",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string" },
                "payslip_id": { "type": "string" },
                "generation": { "type": "integer" },
                "stamped_at": { "type": "string" },
                "created_by": { "type": "string" }
            },
            "required": ["id", "payslip_id", "generation", "stamped_at", "created_by"]
        },
        "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        "derive": [
            { "field": "created_by", "expression": "actor.subjectId", "inputRequired": false },
            { "field": "stamped_at", "expression": "clock.nowIso", "inputRequired": false },
            { "field": "generation", "expression": "1", "inputRequired": false }
        ],
        "writes": [{ "entityKind": "test.Payout", "change": "created" }],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectKind": "test.Payout",
            "objectIdField": "id",
            "entityArtifactId": "test.Payout.entity",
            "entityArtifactVersion": "1.0.0",
            "writeMode": { "kind": "create_only" }
        }
    }))
    .unwrap()
}

fn fail_payout_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "test.payout.fail",
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
        "writes": [{ "entityKind": "test.Payout", "change": "modified" }],
        "effectProgram": {
            "kind": "fire_transition",
            "objectKind": "test.Payout",
            "objectIdField": "id",
            "entityArtifactId": "test.Payout.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "state",
            "transition": "fail"
        }
    }))
    .unwrap()
}

fn retry_payout_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "test.payout.retry",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string" },
                "next_generation": { "type": "integer" }
            },
            "required": ["id", "next_generation"]
        },
        "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        "slice": [{
            "name": "instruction",
            "objectType": "test.Payout",
            "key": { "property": { "from": "input", "property": "id" } }
        }],
        "derive": [
            { "field": "next_generation", "expression": "slice.instruction.generation + 1", "inputRequired": false }
        ],
        "preconditions": ["input.next_generation == 2"],
        "writes": [{ "entityKind": "test.Payout", "change": "modified" }],
        "effectProgram": {
            "kind": "fire_transition",
            "objectKind": "test.Payout",
            "objectIdField": "id",
            "entityArtifactId": "test.Payout.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "state",
            "transition": "retry",
            "setFields": { "generation": "next_generation" }
        }
    }))
    .unwrap()
}

fn tamper_payslip_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "test.payout.tamper_payslip",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string" },
                "payslip_id": { "type": "string" }
            },
            "required": ["id", "payslip_id"]
        },
        "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        "writes": [{ "entityKind": "test.Payout", "change": "modified" }],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectKind": "test.Payout",
            "objectIdField": "id",
            "entityArtifactId": "test.Payout.entity",
            "entityArtifactVersion": "1.0.0",
            "writeMode": { "kind": "update_only" }
        }
    }))
    .unwrap()
}

fn seed_payout(cell: &mut Cell, catalog: &TypeCatalog<'_>, create_action: &ActionRoot) {
    let raw_create: JsonObject =
        serde_json::from_value(json!({ "id": "po_1", "payslip_id": "ps_1" })).unwrap();
    let outcome = commit_action(
        cell,
        catalog,
        &call(
            create_action,
            "test.payout.create.action",
            raw_create,
            "k-create",
            1_764_115_200_000,
        ),
        &Allow,
    )
    .unwrap();
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));
}

#[test]
fn clock_stamp_and_post_slice_generation_bump_commit_atomically() {
    let types = payout_types();
    let catalog = TypeCatalog::new(&types).with_transition_guards(&[]);
    let mut cell = Cell::new(|_| JsonObject::new());
    let create_action = create_payout_action();

    let spoofed: JsonObject =
        serde_json::from_value(json!({ "id": "po_1", "stamped_at": "2020-01-01T00:00:00Z" }))
            .unwrap();
    let err = commit_action(
        &mut cell,
        &catalog,
        &call(
            &create_action,
            "test.payout.create.action",
            spoofed,
            "k-spoof",
            1_764_115_200_000,
        ),
        &Allow,
    )
    .unwrap_err();
    assert!(
        matches!(err, ActionRefusal::InvalidInput { ref field } if field == "stamped_at"),
        "{err:?}"
    );

    seed_payout(&mut cell, &catalog, &create_action);
    let key = FactKey::new("test.Payout", "po_1");
    let created = cell.load(&key).unwrap();
    let created_vals = created.values.as_ref().unwrap();
    assert_eq!(
        created_vals.get("stamped_at"),
        Some(&RuntimeValue::String("2025-11-26T00:00:00Z".to_owned()))
    );
    assert_eq!(
        created_vals.get("generation"),
        Some(&RuntimeValue::from(1_i64))
    );

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &fail_payout_action(),
            "test.payout.fail.action",
            serde_json::from_value(json!({ "id": "po_1" })).unwrap(),
            "k-fail",
            1_764_115_260_000,
        ),
        &Allow,
    )
    .unwrap();

    let retry_action = retry_payout_action();
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &retry_action,
            "test.payout.retry.action",
            serde_json::from_value(json!({ "id": "po_1" })).unwrap(),
            "k-retry",
            1_764_115_320_000,
        ),
        &Allow,
    )
    .unwrap();
    let retried = cell.load(&key).unwrap();
    let retried_vals = retried.values.as_ref().unwrap();
    assert_eq!(
        retried_vals.get("state"),
        Some(&RuntimeValue::String("requested".to_owned()))
    );
    assert_eq!(
        retried_vals.get("generation"),
        Some(&RuntimeValue::from(2_i64))
    );
}

#[test]
fn refuses_immutable_updates_malformed_pre_slice_and_spoofed_server_derived() {
    let types = payout_types();
    let catalog = TypeCatalog::new(&types).with_transition_guards(&[]);
    let mut cell = Cell::new(|_| JsonObject::new());
    seed_payout(&mut cell, &catalog, &create_payout_action());

    let err = commit_action(
        &mut cell,
        &catalog,
        &call(
            &tamper_payslip_action(),
            "test.payout.tamper_payslip.action",
            serde_json::from_value(json!({ "id": "po_1", "payslip_id": "ps_tampered" })).unwrap(),
            "k-tamper",
            1_764_115_400_000,
        ),
        &Allow,
    )
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("immutableAfterCreate"),
        "expected immutableAfterCreate refusal, got: {err:?}"
    );

    let retry_action = retry_payout_action();
    let malformed: JsonObject =
        serde_json::from_value(json!({ "id": "po_1", "unexpected_extra": "boom" })).unwrap();
    let prep_err = norn_kernel::prepare_action_input_for_call::<std::convert::Infallible>(
        &retry_action,
        &malformed,
        "test.payout.retry.action",
        "1.0.0",
        "usr:owner",
    )
    .unwrap_err();
    assert!(
        matches!(prep_err, ActionRefusal::InvalidInput { .. }),
        "expected pre-slice InvalidInput refusal, got: {prep_err:?}"
    );

    let spoof_derived: JsonObject =
        serde_json::from_value(json!({ "id": "po_1", "next_generation": 999 })).unwrap();
    let derived_err = commit_action(
        &mut cell,
        &catalog,
        &call(
            &retry_action,
            "test.payout.retry.action",
            spoof_derived,
            "k-spoof-derived",
            1_764_115_450_000,
        ),
        &Allow,
    )
    .unwrap_err();
    assert!(
        matches!(derived_err, ActionRefusal::InvalidInput { ref field } if field == "next_generation"),
        "expected InvalidInput on next_generation, got: {derived_err:?}"
    );
}
