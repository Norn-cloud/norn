#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test suite"
)]

//! Two-domain correspondence and commit-time enforcement for `core.time`,
//! `core.TimeInterval`, and `norn-zen/7` temporal interval / DST builtins
//! (#1584): HR shift scheduling and equipment maintenance booking share the
//! exact same core interface and kernel invariant evaluation with zero
//! domain-specific kernel code.

use std::collections::{BTreeMap, BTreeSet};

use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate};
use norn_expr::{EvaluationBudget, Value};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, Decimal, InterfaceDef, InterfaceName, InvariantOwner,
    ObjectTypeDef, PropertyTypeCatalogRootV2, PropertyTypeDef, RuleRoot, RuntimeObject,
    RuntimeValue,
};
use norn_kernel::{FactKey, TypeCatalog, evaluate_invariants, invariant_evaluation_budget};
use serde_json::json;

#[path = "../../../registry/core/src/time.rs"]
mod core_time;

const TIME_INTERVAL_ARTIFACT: &str =
    include_str!("../../../registry/core/artifacts/core.TimeInterval.interface.json");

fn core_base() -> (
    Vec<ArtifactEnvelope>,
    Vec<PropertyTypeDef>,
    Vec<InterfaceDef>,
) {
    let time_catalog = core_time::pack().expect("core.time pack");
    let time_interval_env: ArtifactEnvelope =
        serde_json::from_str(TIME_INTERVAL_ARTIFACT).expect("core.TimeInterval envelope");

    let ArtifactPayload::Entity(ref payload) = time_catalog.payload else {
        panic!("expected entity catalog payload");
    };
    let root: PropertyTypeCatalogRootV2 =
        serde_json::from_str(&serde_json::to_string(payload).expect("serialize"))
            .expect("decode catalog");

    let ArtifactPayload::Entity(ref iface_payload) = time_interval_env.payload else {
        panic!("expected entity interface payload");
    };
    let iface_json = serde_json::to_value(iface_payload).expect("serialize iface");
    let interface: InterfaceDef = serde_json::from_value(
        iface_json
            .get("interface")
            .expect("interface field")
            .clone(),
    )
    .expect("decode core.TimeInterval");

    (
        vec![time_catalog, time_interval_env],
        root.property_types,
        vec![interface],
    )
}

fn domain_entity_envelope(
    namespace: &str,
    type_name: &str,
    key_field: &str,
    owner_field: &str,
) -> (ArtifactEnvelope, ObjectTypeDef) {
    let object_json = json!({
        "name": type_name,
        "role": "happening",
        "implements": ["core.TimeInterval"],
        "properties": [
            { "id": key_field, "type": { "kind": "text" } },
            { "id": owner_field, "type": { "kind": "text" } },
            { "id": "starts_at", "type": { "kind": "ref", "name": "core.time.instant" } },
            { "id": "ends_at", "type": { "kind": "ref", "name": "core.time.instant" } },
            {
                "id": "timezone",
                "type": { "kind": "ref", "name": "core.time.iana_timezone" },
                "constraints": { "nullable": true }
            }
        ],
        "identity": [{ "name": "primary", "properties": [key_field] }]
    });
    let object_def: ObjectTypeDef =
        serde_json::from_value(object_json.clone()).expect("object def");
    let envelope: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": format!("{type_name}.entity"),
        "kind": "entity",
        "namespace": namespace,
        "name": type_name,
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": object_json
        }
    }))
    .expect("entity envelope");
    (envelope, object_def)
}

fn object_values(
    key_field: &str,
    key_val: &str,
    owner_field: &str,
    owner_val: &str,
    starts_at: &str,
    ends_at: &str,
    timezone: Option<&str>,
) -> RuntimeObject {
    let mut map = RuntimeObject::new();
    map.insert(key_field.to_owned(), RuntimeValue::from(key_val));
    map.insert(owner_field.to_owned(), RuntimeValue::from(owner_val));
    map.insert("starts_at".to_owned(), RuntimeValue::from(starts_at));
    map.insert("ends_at".to_owned(), RuntimeValue::from(ends_at));
    map.insert(
        "timezone".to_owned(),
        timezone.map_or(RuntimeValue::Null, RuntimeValue::from),
    );
    map
}

fn assert_proper_interval_enforced(
    catalog: &TypeCatalog<'_>,
    type_name: &str,
    key_field: &str,
    key_val: &str,
    owner_field: &str,
    owner_val: &str,
    tz: Option<&str>,
) {
    let valid = object_values(
        key_field,
        key_val,
        owner_field,
        owner_val,
        "2025-03-09T14:00:00Z",
        "2025-03-09T22:00:00Z",
        tz,
    );
    let mut budget = invariant_evaluation_budget();
    assert_eq!(
        evaluate_invariants(
            catalog,
            &FactKey::new(type_name, key_val),
            &valid,
            &mut budget
        )
        .expect("valid interval evaluates cleanly"),
        []
    );

    for (starts_at, ends_at) in [
        ("2025-03-09T14:00:00Z", "2025-03-09T14:00:00Z"),
        ("2025-03-09T22:00:00Z", "2025-03-09T14:00:00Z"),
    ] {
        let invalid = object_values(
            key_field,
            key_val,
            owner_field,
            owner_val,
            starts_at,
            ends_at,
            tz,
        );
        let mut budget = invariant_evaluation_budget();
        let failures = evaluate_invariants(
            catalog,
            &FactKey::new(type_name, key_val),
            &invalid,
            &mut budget,
        )
        .expect("invariant evaluates");
        assert_eq!(failures.len(), 1);
        let failure = failures.first().expect("single invariant failure");
        assert_eq!(
            failure.owner,
            InvariantOwner::Interface(InterfaceName("core.TimeInterval".to_owned()))
        );
        assert_eq!(failure.id.as_str(), "proper_interval");
    }
}

#[test]
fn two_unrelated_domains_share_core_time_interval_admission_and_commit_invariant() {
    let (base, property_types, interfaces) = core_base();
    let (shift_env, shift_def) = domain_entity_envelope(
        "workforce",
        "workforce.ShiftAssignment",
        "shift_id",
        "worker_id",
    );
    let (maint_env, maint_def) = domain_entity_envelope(
        "plant",
        "plant.MaintenanceWindow",
        "booking_id",
        "machine_id",
    );

    let diagnostics = norn_checker::check_artifact_set(
        &[shift_env, maint_env],
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert_eq!(diagnostics, [], "both domains must admit cleanly");

    let types = [shift_def, maint_def];
    let catalog = TypeCatalog::new(&types)
        .with_interfaces(&interfaces)
        .with_property_types(&property_types);

    assert_proper_interval_enforced(
        &catalog,
        "workforce.ShiftAssignment",
        "shift_id",
        "shift-1",
        "worker_id",
        "worker-42",
        Some("America/New_York"),
    );
    assert_proper_interval_enforced(
        &catalog,
        "plant.MaintenanceWindow",
        "booking_id",
        "maint-7",
        "machine_id",
        "press-3",
        Some("Europe/London"),
    );
}

fn shift_rest_rule() -> RuleRoot {
    serde_json::from_value(json!({
        "ruleVersion": "2.0",
        "ruleId": "workforce.check_shift_rest",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["slice", "event"],
            "properties": {
                "slice": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["a_start", "a_end", "b_start", "b_end"],
                    "properties": {
                        "a_start": { "type": "string" },
                        "a_end": { "type": "string" },
                        "b_start": { "type": "string" },
                        "b_end": { "type": "string" }
                    }
                },
                "event": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {}
                }
            }
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["facts", "commands", "timers"],
            "properties": {
                "facts": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {} } },
                "commands": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["overlaps", "gap_seconds", "overlap_seconds"],
                        "properties": {
                            "overlaps": { "type": "boolean" },
                            "gap_seconds": { "type": "number" },
                            "overlap_seconds": { "type": "number" }
                        }
                    }
                },
                "timers": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {} } }
            }
        },
        "graph": {
            "nodes": [
                { "id": "in", "name": "input", "type": "inputNode" },
                {
                    "id": "calc",
                    "name": "calc",
                    "type": "expressionNode",
                    "content": {
                        "expressions": [
                            { "id": "e1", "key": "facts", "value": "[]" },
                            {
                                "id": "e2",
                                "key": "commands",
                                "value": "[{ overlaps: intervals_overlap(timestamp(slice.a_start), timestamp(slice.a_end), timestamp(slice.b_start), timestamp(slice.b_end)), gap_seconds: interval_gap_seconds(timestamp(slice.a_start), timestamp(slice.a_end), timestamp(slice.b_start), timestamp(slice.b_end)), overlap_seconds: interval_intersection_seconds(timestamp(slice.a_start), timestamp(slice.a_end), timestamp(slice.b_start), timestamp(slice.b_end)) }]"
                            },
                            { "id": "e3", "key": "timers", "value": "[]" }
                        ]
                    }
                },
                { "id": "out", "name": "output", "type": "outputNode" }
            ],
            "edges": [
                { "id": "ed1", "sourceId": "in", "targetId": "calc" },
                { "id": "ed2", "sourceId": "calc", "targetId": "out" }
            ]
        }
    }))
    .expect("shift rule")
}

fn maintenance_dst_rule() -> RuleRoot {
    serde_json::from_value(json!({
        "ruleVersion": "2.0",
        "ruleId": "plant.check_maintenance_dst",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["slice", "event"],
            "properties": {
                "slice": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["local_start", "local_end", "tz", "dst_policy", "job_start", "job_end"],
                    "properties": {
                        "local_start": { "type": "string" },
                        "local_end": { "type": "string" },
                        "tz": { "type": "string" },
                        "dst_policy": { "type": "string" },
                        "job_start": { "type": "string" },
                        "job_end": { "type": "string" }
                    }
                },
                "event": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {}
                }
            }
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["facts", "commands", "timers"],
            "properties": {
                "facts": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {} } },
                "commands": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["conflicts", "downtime_seconds"],
                        "properties": {
                            "conflicts": { "type": "boolean" },
                            "downtime_seconds": { "type": "number" }
                        }
                    }
                },
                "timers": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {} } }
            }
        },
        "graph": {
            "nodes": [
                { "id": "in", "name": "input", "type": "inputNode" },
                {
                    "id": "calc",
                    "name": "calc",
                    "type": "expressionNode",
                    "content": {
                        "expressions": [
                            { "id": "e1", "key": "facts", "value": "[]" },
                            {
                                "id": "e2",
                                "key": "commands",
                                "value": "[{ conflicts: intervals_overlap(resolve_local_timestamp(slice.local_start, slice.tz, slice.dst_policy), resolve_local_timestamp(slice.local_end, slice.tz, slice.dst_policy), timestamp(slice.job_start), timestamp(slice.job_end)), downtime_seconds: interval_intersection_seconds(resolve_local_timestamp(slice.local_start, slice.tz, slice.dst_policy), resolve_local_timestamp(slice.local_end, slice.tz, slice.dst_policy), timestamp(slice.job_start), timestamp(slice.job_end)) }]"
                            },
                            { "id": "e3", "key": "timers", "value": "[]" }
                        ]
                    }
                },
                { "id": "out", "name": "output", "type": "outputNode" }
            ],
            "edges": [
                { "id": "ed1", "sourceId": "in", "targetId": "calc" },
                { "id": "ed2", "sourceId": "calc", "targetId": "out" }
            ]
        }
    }))
    .expect("maintenance rule")
}

#[test]
fn shift_rest_decision_evaluates_and_refuses_improper_intervals() {
    let binding = DecisionBinding {
        profile_id: "norn-zen/7".to_owned(),
        environment_hash: "sha256:test".to_owned(),
    };
    let admitted =
        AdmittedDecision::admit(&shift_rest_rule(), &BTreeMap::new(), binding).expect("admit");
    let event = admitted
        .type_event(Value::object::<&str>([]))
        .expect("event");

    let slice = admitted
        .validate_slice(Value::object([
            ("a_start", Value::from("2025-03-09T08:00:00Z")),
            ("a_end", Value::from("2025-03-09T16:00:00Z")),
            ("b_start", Value::from("2025-03-10T03:00:00Z")),
            ("b_end", Value::from("2025-03-10T11:00:00Z")),
        ]))
        .expect("slice");
    let out = evaluate(&admitted, &slice, &event, EvaluationBudget::default())
        .expect("shift rest evaluates");
    assert_eq!(out.output.commands.len(), 1);
    let command = out.output.commands.first().expect("command");
    assert_eq!(command.get("overlaps"), Some(&RuntimeValue::Bool(false)));
    assert_eq!(
        command.get("gap_seconds"),
        Some(&RuntimeValue::Number(Decimal::parse("39600").unwrap()))
    );
    assert_eq!(
        command.get("overlap_seconds"),
        Some(&RuntimeValue::Number(Decimal::parse("0").unwrap()))
    );

    let bad_shift = admitted
        .validate_slice(Value::object([
            ("a_start", Value::from("2025-03-09T08:00:00Z")),
            ("a_end", Value::from("2025-03-09T08:00:00Z")),
            ("b_start", Value::from("2025-03-10T03:00:00Z")),
            ("b_end", Value::from("2025-03-10T11:00:00Z")),
        ]))
        .expect("slice");
    assert!(evaluate(&admitted, &bad_shift, &event, EvaluationBudget::default()).is_err());
}

#[test]
fn maintenance_dst_decision_resolves_and_refuses_dst_anomalies() {
    let binding = DecisionBinding {
        profile_id: "norn-zen/7".to_owned(),
        environment_hash: "sha256:test".to_owned(),
    };
    let admitted =
        AdmittedDecision::admit(&maintenance_dst_rule(), &BTreeMap::new(), binding).expect("admit");
    let event = admitted
        .type_event(Value::object::<&str>([]))
        .expect("event");

    // Fall-back ambiguity refused under "reject".
    let ambiguous_reject = admitted
        .validate_slice(Value::object([
            ("local_start", Value::from("2025-11-02T01:30:00")),
            ("local_end", Value::from("2025-11-02T03:30:00")),
            ("tz", Value::from("America/New_York")),
            ("dst_policy", Value::from("reject")),
            ("job_start", Value::from("2025-11-02T06:00:00Z")),
            ("job_end", Value::from("2025-11-02T09:00:00Z")),
        ]))
        .expect("slice");
    assert!(
        evaluate(
            &admitted,
            &ambiguous_reject,
            &event,
            EvaluationBudget::default()
        )
        .is_err()
    );

    // Fall-back ambiguity resolved under "earlier": [05:30Z, 08:30Z) ∩ [06:00Z, 09:00Z) = 9000s.
    let ambiguous_earlier = admitted
        .validate_slice(Value::object([
            ("local_start", Value::from("2025-11-02T01:30:00")),
            ("local_end", Value::from("2025-11-02T03:30:00")),
            ("tz", Value::from("America/New_York")),
            ("dst_policy", Value::from("earlier")),
            ("job_start", Value::from("2025-11-02T06:00:00Z")),
            ("job_end", Value::from("2025-11-02T09:00:00Z")),
        ]))
        .expect("slice");
    let out = evaluate(
        &admitted,
        &ambiguous_earlier,
        &event,
        EvaluationBudget::default(),
    )
    .expect("maintenance DST resolves");
    assert_eq!(out.output.commands.len(), 1);
    let command = out.output.commands.first().expect("command");
    assert_eq!(command.get("conflicts"), Some(&RuntimeValue::Bool(true)));
    assert_eq!(
        command.get("downtime_seconds"),
        Some(&RuntimeValue::Number(Decimal::parse("9000").unwrap()))
    );

    // Spring-forward nonexistent wall time refused under ALL policies.
    for policy in ["reject", "earlier", "later"] {
        let gap_slice = admitted
            .validate_slice(Value::object([
                ("local_start", Value::from("2025-03-09T02:30:00")),
                ("local_end", Value::from("2025-03-09T04:00:00")),
                ("tz", Value::from("America/New_York")),
                ("dst_policy", Value::from(policy)),
                ("job_start", Value::from("2025-03-09T07:00:00Z")),
                ("job_end", Value::from("2025-03-09T09:00:00Z")),
            ]))
            .expect("slice");
        assert!(
            evaluate(&admitted, &gap_slice, &event, EvaluationBudget::default()).is_err(),
            "nonexistent spring-forward time must fail under policy {policy}"
        );
    }
}
