//! PR-A structural admission proofs:
//! - ACTION-CLOCK-1 (`clock.nowIso` / `clock.nowMillis` in `derive`)
//! - ACTION-SLICE-DERIVE-1 (post-slice `derive` + no-cycle refusal into slice keys)
//! - TRANSITION-PATCH-1 (`fire_transition` `setFields`)
//! - ORIGIN-DATAFLOW-1 (`origin` and `immutableAfterCreate`, including subtype tightening)

use std::collections::BTreeSet;

use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use serde_json::{Value, json};

fn envelope(kind: &str, artifact_id: &str, name: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": "test",
        "name": name,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_test", "environmentId": "dev" },
        "provenance": { "source": "authoring_sdk", "sourcePath": "test/pr_a.ts" },
        "dependencies": [],
        "payload": payload
    }))
    .expect("envelope decodes")
}

fn payout_entity() -> ArtifactEnvelope {
    envelope(
        "entity",
        "test.Payout.entity",
        "Payout",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.Payout",
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "payslip_id", "type": { "kind": "text" }, "immutableAfterCreate": true },
                    { "id": "generation", "type": { "kind": "integer" }, "origin": "derived", "default": "1" },
                    { "id": "stamped_at", "type": { "kind": "timestamp", "timezone": "utc" }, "origin": "clock" },
                    { "id": "last_evidence_digest", "type": { "kind": "text" }, "origin": "receipt", "default": "" },
                    { "id": "provider_tx", "type": { "kind": "text" }, "origin": "receipt", "constraints": { "nullable": true } },
                    { "id": "state", "type": { "kind": "enum", "values": [{ "value": "requested" }, { "value": "failed" }, { "value": "succeeded" }] }, "default": "requested" },
                    { "id": "created_by", "type": { "kind": "text" }, "protected": true, "origin": "actor" }
                ],
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "lifecycles": [{
                    "property": "state",
                    "initial": ["requested"],
                    "terminal": ["succeeded"],
                    "transitions": [
                        { "id": "confirm", "from": ["requested"], "to": "succeeded" },
                        { "id": "fail", "from": ["requested"], "to": "failed" },
                        { "id": "retry", "from": ["failed"], "to": "requested" }
                    ]
                }]
            }
        }),
    )
}

fn check(artifacts: &[ArtifactEnvelope]) -> Vec<String> {
    let registry = BTreeSet::<PropertyTypeName>::new();
    check_artifact_set(
        artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    )
    .into_iter()
    .map(|d| format!("{}: {}", d.code.as_str(), d.message))
    .collect()
}

fn create_payout_action() -> ArtifactEnvelope {
    envelope(
        "action",
        "test.payout.create.action",
        "Create payout",
        &json!({
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
        }),
    )
}

fn retry_payout_action() -> ArtifactEnvelope {
    envelope(
        "action",
        "test.payout.retry.action",
        "Retry failed payout",
        &json!({
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
            "preconditions": ["input.next_generation > 1"],
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
        }),
    )
}

fn confirm_payout_action() -> ArtifactEnvelope {
    envelope(
        "action",
        "test.payout.confirm.action",
        "Confirm payout",
        &json!({
            "actionId": "test.payout.confirm",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "string" },
                    "receipt": { "type": "string" }
                },
                "required": ["id", "receipt"]
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
                "transition": "confirm",
                "acceptReceipt": {
                    "referenceField": "receipt",
                    "manifestDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "digestProperty": "last_evidence_digest",
                    "resultBindings": { "provider_tx": "tx_id" }
                }
            }
        }),
    )
}

#[test]
fn admits_clock_origin_post_slice_derive_and_transition_set_fields() {
    assert_eq!(
        check(&[
            payout_entity(),
            create_payout_action(),
            retry_payout_action(),
            confirm_payout_action(),
        ]),
        Vec::<String>::new()
    );
}

#[test]
fn refuses_origin_violations_and_immutable_updates() {
    let bad_upsert = envelope(
        "action",
        "test.payout.bad_update.action",
        "Bad update",
        &json!({
            "actionId": "test.payout.bad_update",
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
                    "last_evidence_digest": { "type": "string" }
                },
                "required": ["id", "payslip_id", "generation", "stamped_at", "last_evidence_digest"]
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
        }),
    );
    let errs = check(&[payout_entity(), bad_upsert]);
    assert!(
        errs.iter().any(|e| e.contains("immutableAfterCreate")),
        "{errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.contains("origin `clock`")),
        "{errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.contains("origin `derived`")),
        "{errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.contains("origin `receipt`")),
        "{errs:?}"
    );
}

#[test]
fn refuses_slice_key_cycles_in_post_slice_derive() {
    let cyclic = envelope(
        "action",
        "test.payout.cyclic.action",
        "Cyclic slice derive",
        &json!({
            "actionId": "test.payout.cyclic",
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
            "slice": [{
                "name": "instruction",
                "objectType": "test.Payout",
                "key": { "property": { "from": "input", "property": "id" } }
            }],
            "derive": [
                { "field": "id", "expression": "slice.instruction.payslip_id", "inputRequired": true }
            ],
            "writes": [{ "entityKind": "test.Payout", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "test.Payout",
                "objectIdField": "id",
                "entityArtifactId": "test.Payout.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "state",
                "transition": "retry"
            }
        }),
    );
    let cycle_errs = check(&[payout_entity(), cyclic]);
    assert!(
        cycle_errs
            .iter()
            .any(|e| e.contains("post-slice derivation cannot write input field")),
        "{cycle_errs:?}"
    );
}

#[test]
fn enforces_subtype_origin_inheritance_and_caller_taint() {
    let wire_subtype = envelope(
        "entity",
        "test.WirePayout.entity",
        "WirePayout",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.WirePayout",
                "extends": "test.Payout",
                "properties": [
                    { "id": "payslip_id", "type": { "kind": "text" }, "constraints": { "minLength": 1 } },
                    { "id": "stamped_at", "type": { "kind": "timestamp", "timezone": "utc" } }
                ]
            }
        }),
    );
    let subtype_spoof = envelope(
        "action",
        "test.wire.spoof.action",
        "Subtype spoof",
        &json!({
            "actionId": "test.wire.spoof",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "string" },
                    "payslip_id": { "type": "string" },
                    "stamped_at": { "type": "string" }
                },
                "required": ["id", "payslip_id", "stamped_at"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "writes": [{ "entityKind": "test.WirePayout", "change": "modified" }],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectKind": "test.WirePayout",
                "objectIdField": "id",
                "entityArtifactId": "test.WirePayout.entity",
                "entityArtifactVersion": "1.0.0",
                "writeMode": { "kind": "update_only" }
            }
        }),
    );
    let subtype_errs = check(&[payout_entity(), wire_subtype, subtype_spoof]);
    assert!(
        subtype_errs
            .iter()
            .any(|e| e.contains("immutableAfterCreate")),
        "{subtype_errs:?}"
    );
    assert!(
        subtype_errs.iter().any(|e| e.contains("origin `clock`")),
        "{subtype_errs:?}"
    );
}

#[test]
fn refuses_caller_passthrough_reserved_clock_and_bad_set_fields_type() {
    let passthrough_derived = envelope(
        "action",
        "test.payout.passthrough.action",
        "Passthrough derived",
        &json!({
            "actionId": "test.payout.passthrough",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "string" },
                    "caller_gen": { "type": "integer" },
                    "generation": { "type": "integer" }
                },
                "required": ["id", "caller_gen", "generation"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "derive": [
                { "field": "generation", "expression": "input.caller_gen", "inputRequired": false }
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
        }),
    );
    let passthrough_errs = check(&[payout_entity(), passthrough_derived]);
    assert!(
        passthrough_errs
            .iter()
            .any(|e| e.contains("origin `derived`")),
        "{passthrough_errs:?}"
    );

    let bad_clock_and_patch = envelope(
        "action",
        "test.payout.bad_patch.action",
        "Bad clock and patch",
        &json!({
            "actionId": "test.payout.bad_patch",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "id": { "type": "string" },
                    "clock": { "type": "string" },
                    "wrong_type": { "type": "string" }
                },
                "required": ["id", "wrong_type"]
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
                "transition": "retry",
                "setFields": { "generation": "wrong_type" }
            }
        }),
    );
    let patch_errs = check(&[payout_entity(), bad_clock_and_patch]);
    assert!(
        patch_errs
            .iter()
            .any(|e| e.contains("reserves actor, approved, and clock")),
        "{patch_errs:?}"
    );
    assert!(
        patch_errs
            .iter()
            .any(|e| e.contains("incompatible with the target property")),
        "{patch_errs:?}"
    );
}

#[test]
fn refuses_base_action_writing_subtype_tightened_origin() {
    let base = envelope(
        "entity",
        "test.BaseDoc.entity",
        "BaseDoc",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.BaseDoc",
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "stamped_at", "type": { "kind": "timestamp", "timezone": "utc" } },
                    { "id": "state", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }, "default": "open" }
                ],
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "lifecycles": [{
                    "property": "state",
                    "initial": ["open"],
                    "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
                }]
            }
        }),
    );
    let sub = envelope(
        "entity",
        "test.AuditedDoc.entity",
        "AuditedDoc",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.AuditedDoc",
                "extends": "test.BaseDoc",
                "properties": [
                    { "id": "stamped_at", "type": { "kind": "timestamp", "timezone": "utc" }, "origin": "clock" }
                ]
            }
        }),
    );
    let base_close = envelope(
        "action",
        "test.base_doc.close.action",
        "Close base doc",
        &json!({
            "actionId": "test.base_doc.close",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "id": { "type": "string" }, "caller_ts": { "type": "string" } },
                "required": ["id", "caller_ts"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "writes": [{ "entityKind": "test.BaseDoc", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "test.BaseDoc",
                "objectIdField": "id",
                "entityArtifactId": "test.BaseDoc.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "state",
                "transition": "close",
                "setFields": { "stamped_at": "caller_ts" }
            }
        }),
    );
    let errs = check(&[base, sub, base_close]);
    assert!(
        errs.iter()
            .any(|e| e.contains("origin `clock`") && e.contains("test.AuditedDoc")),
        "base action writing subtype-tightened clock property must fail closed: {errs:?}"
    );
}

#[test]
fn refuses_base_action_writing_subtype_protected_property() {
    let base = envelope(
        "entity",
        "test.BaseNote.entity",
        "BaseNote",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.BaseNote",
                "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "memo", "type": { "kind": "text" } },
                    { "id": "state", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }, "default": "open" }
                ],
                "identity": [{ "name": "primary", "properties": ["id"] }],
                "lifecycles": [{
                    "property": "state",
                    "initial": ["open"],
                    "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
                }]
            }
        }),
    );
    let sub = envelope(
        "entity",
        "test.SealedNote.entity",
        "SealedNote",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.SealedNote",
                "extends": "test.BaseNote",
                "properties": [
                    { "id": "memo", "type": { "kind": "text" }, "protected": true }
                ]
            }
        }),
    );
    let base_close = envelope(
        "action",
        "test.base_note.close.action",
        "Close base note",
        &json!({
            "actionId": "test.base_note.close",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "id": { "type": "string" }, "memo_in": { "type": "string" } },
                "required": ["id", "memo_in"]
            },
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "writes": [{ "entityKind": "test.BaseNote", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition",
                "objectKind": "test.BaseNote",
                "objectIdField": "id",
                "entityArtifactId": "test.BaseNote.entity",
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "state",
                "transition": "close",
                "setFields": { "memo": "memo_in" }
            }
        }),
    );
    let errs = check(&[base, sub, base_close]);
    assert!(
        errs.iter()
            .any(|e| e.contains("protected property") && e.contains("test.SealedNote")),
        "base action writing subtype-tightened protected property must fail closed: {errs:?}"
    );
}
