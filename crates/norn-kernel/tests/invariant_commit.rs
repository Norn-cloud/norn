#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "integration-test fixture helpers use concise assertions"
)]

//! Shared commit admission for object and interface invariants, including the
//! command refusal ledger and the authorized observed-data path.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::EvaluationBudget;
use norn_ir::provenance::{
    FactRef, InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp,
};
use norn_ir::{
    ActionRoot, DataQualityFinding, DataQualityFindingStatus, DataQualityLineage,
    DataQualityOutcome, DataQualityRuleRef, DataQualitySeverity, DataQualitySubject,
    DecisionOutput, InterfaceDef, InvariantId, InvariantOwner, ObjectTypeDef, ObjectTypeName,
    PropertyTypeDef, RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::snapshot_observe::{
    ObservationAuthorization, ObservationEvent, ObservationRefusal, ObservedSnapshot,
    commit_observed_snapshot, prepare_observed_snapshot,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionRefusal, CellCommit,
    CellHost, CommitError, DecisionRefusal, DecisionTurn, FactEvent, FactExpectation, FactKey,
    FactMutation, FactVersion, FactWrite, LinkKey, LinkMutation, LinkWrite, LogicalTime,
    ObservationId, Observed, Provenance, TypeCatalog, World, commit_action, commit_decision,
    load_slice,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

const ITEM: &str = "invariant.Item";
const SUBTYPE: &str = "invariant.SpecialItem";
const INTERFACE: &str = "invariant.Named";
const FUEL_ITEM: &str = "invariant.FuelItem";
const NUMERIC_STRING_BASE: &str = "invariant.NumericStringBase";
const NUMERIC_STRING_ITEM: &str = "invariant.NumericStringItem";
const TEMPORAL_ITEM: &str = "invariant.TemporalItem";
const SEASON_ITEM: &str = "invariant.SeasonWindow";

type CommandCell = MemoryCell<FactEvent, JsonObject, JsonObject>;
type ObservationCell = MemoryCell<ObservationEvent, JsonObject, JsonObject>;

fn definitions() -> (Vec<ObjectTypeDef>, Vec<InterfaceDef>) {
    let item = serde_json::from_value(json!({
        "name": ITEM,
        "implements": [INTERFACE],
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "amount", "type": {"kind": "integer"}},
            {"id": "status", "type": {"kind": "enum", "values": [
                {"value": "draft"}, {"value": "approved"}
            ]}, "default": "draft"}
        ],
        "lifecycles": [{
            "property": "status", "initial": ["draft"], "terminal": ["approved"],
            "transitions": [{"id": "approve", "from": ["draft"], "to": "approved"}]
        }],
        "invariants": [
            {"id": "positive_amount", "expression": "self.amount > 0",
             "message": "amount must be positive"},
            {"id": "not_approved", "expression": r#"self.status != "approved""#,
             "message": "approval requires a separate review"}
        ]
    }))
    .expect("object type");
    let subtype = serde_json::from_value(json!({
        "name": SUBTYPE,
        "extends": ITEM,
        "invariants": [{"id": "below_limit", "expression": "self.amount < 100",
                        "message": "amount must be below one hundred"}]
    }))
    .expect("subtype");
    let named = serde_json::from_value(json!({
        "name": INTERFACE,
        "properties": [{"id": "item_id", "type": {"kind": "text"}}],
        "invariants": [{"id": "named_item", "classification": "integrity",
                        "expression": r#"self.item_id != """#,
                        "message": "item id is required"}]
    }))
    .expect("interface");
    (vec![item, subtype], vec![named])
}

fn negative_default_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": ITEM,
        "properties": [{
            "id": "amount",
            "type": {"kind": "integer"},
            "constraints": {"nullable": true},
            "default": "-1"
        }],
        "invariants": [{
            "id": "nonnegative_amount",
            "expression": "(self.amount ?? 0) >= 0",
            "message": "amount must be nonnegative"
        }]
    }))
    .expect("negative default object type")
}

fn quality_item_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": ITEM,
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "amount", "type": {"kind": "integer"}}
        ],
        "invariants": [{
            "id": "positive_amount",
            "classification": "quality",
            "expression": "self.amount > 0",
            "message": "amount must be positive"
        }]
    }))
    .expect("quality-class invariant object type")
}

fn temporal_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": TEMPORAL_ITEM,
        "properties": [
            {"id": "service_date", "type": {"kind": "date"}},
            {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
        ]
    }))
    .expect("temporal object type")
}

fn season_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": SEASON_ITEM,
        "properties": [
            {"id": "season_start", "type": {"kind": "date"}},
            {"id": "season_end", "type": {"kind": "date"}}
        ],
        "invariants": [{
            "id": "season_window",
            "expression": "date(self.season_end) >= date(self.season_start)",
            "message": "season end must not be before season start"
        }]
    }))
    .expect("season window object type")
}

#[test]
fn raw_reference_host_validates_and_normalizes_temporal_link_defaults() {
    for (date, timestamp, valid) in [
        ("2025-02-29", "2024-02-29T12:30:00+02:30", false),
        ("2024-02-29", "2025-02-29T12:30:00+02:30", false),
        ("2024-02-29", "2024-02-29T12:30:00+02:30", true),
    ] {
        let definition: ObjectTypeDef = serde_json::from_value(json!({
            "name": TEMPORAL_ITEM,
            "links": [{
                "id": "related", "target": TEMPORAL_ITEM,
                "cardinality": {"to": "many", "from": "many"},
                "properties": [
                    {"id": "day", "type": {"kind": "date"}, "default": date},
                    {"id": "at", "type": {"kind": "timestamp", "timezone": "offset"},
                     "default": timestamp}
                ]
            }]
        }))
        .unwrap();
        let mut cell =
            CommandCell::new(|command| command.payload.clone()).with_types(vec![definition]);
        let source = FactKey::new(TEMPORAL_ITEM, "source");
        let target = FactKey::new(TEMPORAL_ITEM, "target");
        cell.commit(CellCommit {
            writes: [source.clone(), target.clone()]
                .into_iter()
                .map(|key| FactWrite {
                    key,
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: JsonObject::new(),
                    },
                })
                .collect(),
            ..CellCommit::empty()
        })
        .unwrap();
        let key = LinkKey::new(source.clone(), "related", target.clone());
        let before = (
            cell.load(&source).unwrap(),
            cell.load(&target).unwrap(),
            cell.ledger().len(),
        );
        let result = cell.commit(CellCommit {
            links: vec![LinkWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: LinkMutation::Add {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        });
        if valid {
            result.expect("valid temporal defaults are admitted");
            let values = cell.load_link(&key).unwrap().values.unwrap();
            assert_eq!(
                values.get("at"),
                Some(&RuntimeValue::String("2024-02-29T10:00:00Z".to_owned()))
            );
            assert_eq!(
                values.get("day"),
                Some(&RuntimeValue::String("2024-02-29".to_owned()))
            );
        } else {
            let refusal = result.expect_err("invalid link defaults refuse atomically");
            assert_eq!(refusal.code(), "commit.effective_values");
            assert!(!cell.load_link(&key).unwrap().exists());
            assert_eq!(
                before,
                (
                    cell.load(&source).unwrap(),
                    cell.load(&target).unwrap(),
                    cell.ledger().len()
                )
            );
        }
    }
}

fn catalog<'a>(types: &'a [ObjectTypeDef], interfaces: &'a [InterfaceDef]) -> TypeCatalog<'a> {
    TypeCatalog::new(types).with_interfaces(interfaces)
}

fn object(value: Value) -> JsonObject {
    serde_json::from_value(value).expect("runtime object")
}

fn slice_with_values(
    slice: &norn_kernel::FactSlice,
    key: &FactKey,
    values: &JsonObject,
) -> norn_kernel::FactSlice {
    let mut updated = slice.clone();
    updated
        .get_mut(key)
        .expect("snapshot key was loaded")
        .values = Some(values.clone());
    updated
}

fn cell() -> CommandCell {
    let (types, interfaces) = definitions();
    CommandCell::new(|command| command.payload.clone())
        .with_types(types)
        .with_interfaces(interfaces)
}

fn observation_cell() -> ObservationCell {
    let (types, interfaces) = definitions();
    MemoryCell::new(|_| JsonObject::new())
        .with_types(types)
        .with_interfaces(interfaces)
}

fn turn(id: &str) -> DecisionTurn {
    DecisionTurn {
        id: id.to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    }
}

fn seed(cell: &mut CommandCell, object_type: &str, key: &str, amount: i64) -> FactKey {
    let key = FactKey::new(object_type, key);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: object(json!({
                    "item_id": key.key,
                    "amount": amount,
                    "status": "draft"
                })),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("valid seed");
    key
}

fn output(facts: &Value, effects: bool) -> DecisionOutput {
    let (commands, timers) = if effects {
        (
            json!([{"kind": "notify", "message": "invariant test"}]),
            json!([{"afterMillis": 50, "timerId": "follow-up"}]),
        )
    } else {
        (json!([]), json!([]))
    };
    serde_json::from_value(json!({"facts": facts, "commands": commands, "timers": timers}))
        .expect("decision output")
}

fn assert_refusal_record(
    cell: &CommandCell,
    prior_entries: usize,
    key: &FactKey,
    owner: &InvariantOwner,
    invariant_id: &str,
    attempt_id: &str,
) {
    assert_eq!(
        cell.ledger().len(),
        prior_entries + 1,
        "one refusal record is appended"
    );
    assert!(
        cell.outbox().is_empty(),
        "a refused command reached the outbox"
    );
    assert!(cell.timers().is_empty(), "a refused timer was scheduled");
    let entry = cell.ledger().last().expect("refusal entry");
    assert!(
        entry.written.is_empty(),
        "refusal entry wrote a business fact"
    );
    assert!(entry.linked.is_empty());
    assert_eq!(entry.events.len(), 1);
    assert!(matches!(
        &entry.events[0],
        FactEvent::InvariantRefused {
            key: refused_key,
            owner: refused_owner,
            id,
            message,
            outcome: DataQualityOutcome::NotSatisfied,
            attempt_id: refused_attempt,
        } if refused_key == key
            && refused_owner == owner
            && id.as_str() == invariant_id
            && !message.is_empty()
            && refused_attempt == attempt_id
    ));
}

fn allow_action() -> impl ActionAuthorizer {
    struct Allow;
    impl ActionAuthorizer for Allow {
        fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
    }
    Allow
}

fn action_call<'a>(
    action: &'a ActionRoot,
    input: Value,
    idempotency_key: &str,
    turn_id: &str,
) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "invariant.test.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "test".to_owned()),
        ]),
        actor: "agent:test".to_owned(),
        input: object(input),
        idempotency_key: idempotency_key.to_owned(),
        object_type: Some(ObjectTypeName(ITEM.to_owned())),
        turn: turn(turn_id),
    }
}

fn action(effect_kind: &str) -> ActionRoot {
    let effect_program = if effect_kind == "fire_transition" {
        json!({
            "kind": "fire_transition",
            "objectIdField": "item_id",
            "objectKind": ITEM,
            "entityArtifactId": "invariant.Item.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": "approve"
        })
    } else {
        json!({
            "kind": "upsert_input_object",
            "objectIdField": "item_id",
            "objectKind": ITEM,
            "entityArtifactId": "invariant.Item.entity",
            "entityArtifactVersion": "1.0.0"
        })
    };
    let properties = if effect_kind == "fire_transition" {
        json!({"item_id": {"type": "string"}})
    } else {
        json!({
            "item_id": {"type": "string"},
            "amount": {"type": "integer"},
            "status": {"type": "string"}
        })
    };
    let writes = if effect_kind == "fire_transition" {
        json!([{"entityKind": ITEM, "change": "modified"}])
    } else {
        json!([
            {"entityKind": ITEM, "change": "created"},
            {"entityKind": ITEM, "change": "modified"}
        ])
    };
    serde_json::from_value(json!({
        "actionId": format!("invariant.{effect_kind}"),
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": false,
        "ledgered": true,
        "ledgerCategory": "invariant.test",
        "inputSchema": {"type": "object", "properties": properties,
                         "required": ["item_id"], "additionalProperties": false},
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "writes": writes,
        "commands": [{
            "connector": "test",
            "operation": "submit",
            "doctype": "Item",
            "name": "input.item_id",
            "fields": {}
        }],
        "effectProgram": effect_program
    }))
    .expect("action")
}

fn numeric_string_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "invariant.upsert_numeric_string",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": false,
        "ledgered": true,
        "ledgerCategory": "invariant.test",
        "inputSchema": {
            "type": "object",
            "properties": {
                "item_id": {"type": "string"},
                "amount": {"type": "string"},
                "rate": {"type": "string"}
            },
            "required": ["item_id", "amount", "rate"],
            "additionalProperties": false
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "writes": [
            {"entityKind": NUMERIC_STRING_ITEM, "change": "created"},
            {"entityKind": NUMERIC_STRING_ITEM, "change": "modified"}
        ],
        "commands": [{
            "connector": "test",
            "operation": "submit",
            "doctype": "NumericStringItem",
            "name": "input.item_id",
            "fields": {}
        }],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "item_id",
            "objectKind": NUMERIC_STRING_ITEM,
            "entityArtifactId": "invariant.NumericStringItem.entity",
            "entityArtifactVersion": "1.0.0"
        }
    }))
    .expect("string-valued numeric action")
}

fn numeric_string_action_call<'a>(
    action: &'a ActionRoot,
    input: Value,
    idempotency_key: &str,
    turn_id: &str,
) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "invariant.NumericStringItem.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "test".to_owned()),
        ]),
        actor: "agent:test".to_owned(),
        input: object(input),
        idempotency_key: idempotency_key.to_owned(),
        object_type: Some(ObjectTypeName(NUMERIC_STRING_ITEM.to_owned())),
        turn: turn(turn_id),
    }
}

#[test]
fn raw_shared_commit_refuses_invalid_final_state_before_events_or_effects() {
    let mut cell = observation_cell();
    let key = FactKey::new(ITEM, "raw-invalid");
    let provenance = source_provenance();
    let finding = DataQualityFinding::v2(
        DataQualityRuleRef::Invariant {
            owner: InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
            invariant_id: InvariantId("positive_amount".to_owned()),
            version: "sha256:test-owner-version".to_owned(),
        },
        vec![DataQualitySubject::new(
            ObjectTypeName(ITEM.to_owned()),
            key.key.clone(),
        )],
        DataQualitySeverity::Error,
        "amount must be positive",
        DataQualityOutcome::NotSatisfied,
        vec![DataQualityLineage::new(
            provenance.clone(),
            vec!["raw-turn".to_owned()],
        )],
        provenance.recorded_at.clone(),
        provenance.recorded_at,
        DataQualityFindingStatus::Open,
    )
    .expect("unified finding encodes canonically");
    let error = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(
                        json!({"item_id": "raw-invalid", "amount": 0, "status": "draft"}),
                    ),
                },
            }],
            events: vec![ObservationEvent::DataQualityFinding { fact: finding }],
            ..CellCommit::empty()
        })
        .unwrap_err();
    assert_eq!(error.code(), "commit.invariant_violation");
    assert!(matches!(
        error,
        CommitError::InvariantViolation {
            owner: InvariantOwner::Object(ref owner),
            ref id,
            outcome,
            ..
        } if owner.as_str() == ITEM
            && id.as_str() == "positive_amount"
            && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
    ));
    assert!(cell.load(&key).unwrap().values.is_none());
    assert!(cell.ledger().is_empty());
    assert!(cell.outbox().is_empty());
    assert!(cell.timers().is_empty());
}

#[test]
fn temporal_properties_are_checked_on_raw_and_decision_writes_and_observations() {
    let types = vec![temporal_type()];
    let catalog = TypeCatalog::new(&types);
    let mut cell = CommandCell::new(|command| command.payload.clone()).with_types(types.clone());

    for (key_name, date, timestamp, expected_id) in [
        (
            "invalid-date",
            "2025-02-29",
            "2024-02-28T10:00:00Z",
            "$property.service_date",
        ),
        (
            "invalid-timestamp",
            "2024-02-29",
            "2025-02-29T10:00:00+00:00",
            "$property.occurred_at",
        ),
    ] {
        let key = FactKey::new(TEMPORAL_ITEM, key_name);
        let error = cell
            .commit(CellCommit {
                writes: vec![FactWrite {
                    key: key.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: object(json!({
                            "service_date": date,
                            "occurred_at": timestamp
                        })),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect_err("raw invalid temporal writes are refused atomically");
        assert!(matches!(
            error,
            CommitError::InvariantViolation {
                owner: InvariantOwner::Object(ref owner),
                ref id,
                outcome,
                ..
            } if owner.as_str() == TEMPORAL_ITEM
                && id.as_str() == expected_id
                && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
        ));
        assert!(cell.load(&key).unwrap().values.is_none());
    }

    let canonical_timestamp = "2024-02-29T10:00:00Z";
    let source_timestamp = "2024-02-29T12:30:00+02:30";
    let accepted = FactKey::new(TEMPORAL_ITEM, "accepted-offset");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: accepted.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: object(json!({
                    "service_date": "2024-02-29",
                    "occurred_at": source_timestamp
                })),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("valid leap day and offset timestamp are admitted");
    assert_eq!(
        cell.load(&accepted)
            .unwrap()
            .values
            .unwrap()
            .get("occurred_at"),
        Some(&RuntimeValue::String(canonical_timestamp.to_owned()))
    );

    let patch_target = FactKey::new(TEMPORAL_ITEM, "decision-patch");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: patch_target.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: object(json!({
                    "service_date": "2024-02-29",
                    "occurred_at": canonical_timestamp
                })),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("seed valid temporal decision target");
    let slice = load_slice(&cell, [patch_target.clone()]).unwrap();
    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("turn-temporal-patch"),
        &output(
            &json!([{
                "op": "set",
                "objectType": TEMPORAL_ITEM,
                "key": patch_target.key,
                "property": "service_date",
                "value": "2025-02-29"
            }]),
            false,
        ),
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        DecisionRefusal::InvariantViolation {
            owner: InvariantOwner::Object(ref owner),
            ref id,
            outcome,
            ..
        } if owner.as_str() == TEMPORAL_ITEM
            && id.as_str() == "$property.service_date"
            && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
    ));
    assert_eq!(
        cell.load(&patch_target)
            .unwrap()
            .values
            .unwrap()
            .get("service_date"),
        Some(&RuntimeValue::String("2024-02-29".to_owned()))
    );

    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    let key = FactKey::new(TEMPORAL_ITEM, "observed-invalid-date");
    let slice = load_slice(&observed, [key.clone()]).unwrap();
    let provenance = source_provenance();
    let values = object(json!({
        "service_date": "2025-02-29",
        "occurred_at": source_timestamp
    }));
    commit_observed_snapshot(
        &mut observed,
        &catalog,
        &slice,
        &turn("turn-temporal-observation"),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .expect("invalid source date is retained with a quality finding");
    let stored = observed.load(&key).unwrap().values.unwrap();
    assert_eq!(
        stored.get("service_date"),
        Some(&RuntimeValue::String("2025-02-29".to_owned()))
    );
    assert_eq!(
        stored.get("occurred_at"),
        Some(&RuntimeValue::String(canonical_timestamp.to_owned()))
    );
    assert!(matches!(
        observed.ledger().last().unwrap().events.iter().find(|event| {
            matches!(event, ObservationEvent::DataQualityFinding { .. })
        }),
        Some(ObservationEvent::DataQualityFinding { fact })
            if fact.schema_version == 2
                && fact.rule_ref == (DataQualityRuleRef::Builtin {
                    id: "norn.temporal.property_validity".to_owned(),
                    version: "1".to_owned(),
                })
                && fact.subjects == vec![DataQualitySubject::new(
                    ObjectTypeName(TEMPORAL_ITEM.to_owned()),
                    key.key.clone(),
                )]
                && fact.outcome == (DataQualityOutcome::InvalidTemporalObservation {
                    property: norn_ir::PropertyId("service_date".to_owned()),
                    value: "2025-02-29".to_owned(),
                    reason: "must be a valid calendar date in YYYY-MM-DD form".to_owned(),
                })
                && fact.provenance
                    == vec![DataQualityLineage::new(
                        provenance.clone(),
                        vec!["turn-temporal-observation".to_owned()],
                    )]
    ));
}

#[test]
fn quality_constraints_allow_commands_and_refresh_findings_for_unchanged_bad_observations() {
    let quality_type = quality_item_type();
    let key = FactKey::new(ITEM, "quality-invalid");
    let invalid_values = object(json!({"item_id": "quality-invalid", "amount": 0}));

    let mut command_cell: CommandCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(vec![quality_type.clone()]);
    command_cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: invalid_values.clone(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("quality expectations do not veto commands");
    assert_eq!(
        command_cell.load(&key).unwrap().values,
        Some(invalid_values.clone())
    );

    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(vec![quality_type.clone()]);
    observed
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: invalid_values.clone(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("fixture starts with an externally reported state");

    let catalog_types = [quality_type];
    let catalog = TypeCatalog::new(&catalog_types);
    let mut first_provenance = source_provenance();
    first_provenance.recorded_at = UtcTimestamp("2026-10-05T00:00:00Z".to_owned());
    let first_turn = turn("quality-observation-first");
    let first_slice = load_slice(&observed, [key.clone()]).unwrap();
    let first_prepared = prepare_observed_snapshot(
        &catalog,
        &first_slice,
        &first_turn,
        ObservedSnapshot {
            key: &key,
            values: &invalid_values,
            lifecycles: &[],
            provenance: &first_provenance,
        },
    )
    .expect("unchanged quality failure is admitted as a finding");
    assert!(first_prepared.writes.is_empty(), "no fact version bump");
    assert!(
        first_prepared.events.is_empty(),
        "finding is appended by sealed authority"
    );
    commit_observed_snapshot(
        &mut observed,
        &catalog,
        &first_slice,
        &first_turn,
        ObservedSnapshot {
            key: &key,
            values: &invalid_values,
            lifecycles: &[],
            provenance: &first_provenance,
        },
    )
    .expect("finding-only observation commits against its read version");
    let object_version = observed.load(&key).unwrap().version;
    let first_finding = observed
        .ledger()
        .last()
        .unwrap()
        .events
        .iter()
        .find_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact.clone()),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .expect("the unchanged bad observation emits a unified finding");
    assert_eq!(first_finding.severity, DataQualitySeverity::Warning);
    assert_eq!(first_finding.first_seen, first_provenance.recorded_at);
    assert_eq!(first_finding.last_seen, first_provenance.recorded_at);
    assert_eq!(
        first_finding.provenance[0].observation_ids,
        vec![first_turn.id.clone()]
    );

    let mut second_provenance = first_provenance.clone();
    second_provenance.recorded_at = UtcTimestamp("2026-10-05T00:01:00Z".to_owned());
    let second_turn = turn("quality-observation-second");
    let second_slice = load_slice(&observed, [key.clone()]).unwrap();
    let second_prepared = prepare_observed_snapshot(
        &catalog,
        &second_slice,
        &second_turn,
        ObservedSnapshot {
            key: &key,
            values: &invalid_values,
            lifecycles: &[],
            provenance: &second_provenance,
        },
    )
    .expect("repeated unchanged quality failure remains observable");
    assert!(
        second_prepared.writes.is_empty(),
        "repeat still does not bump version"
    );
    let (mut tampered_commit, read_finding_authorization) = second_prepared.into_parts();
    tampered_commit.reads.clear();
    let error = observed
        .commit_observed(tampered_commit, read_finding_authorization)
        .unwrap_err();
    assert_eq!(error.code(), "commit.observation_authorization");
    commit_observed_snapshot(
        &mut observed,
        &catalog,
        &second_slice,
        &second_turn,
        ObservedSnapshot {
            key: &key,
            values: &invalid_values,
            lifecycles: &[],
            provenance: &second_provenance,
        },
    )
    .expect("second finding-only observation commits");
    let second_finding = observed
        .ledger()
        .last()
        .unwrap()
        .events
        .iter()
        .find_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .expect("the repeated observation emits a unified finding");
    assert_eq!(second_finding.id, first_finding.id);
    assert_eq!(second_finding.dedupe_key, first_finding.dedupe_key);
    assert_eq!(second_finding.last_seen, second_provenance.recorded_at);
    assert_eq!(second_finding.first_seen, second_provenance.recorded_at);
    assert_eq!(
        second_finding.provenance[0].observation_ids,
        vec![second_turn.id]
    );
    assert_eq!(observed.load(&key).unwrap().version, object_version);

    let changed_invalid_values = object(json!({"item_id": "quality-invalid", "amount": -1}));
    let changed_turn = turn("quality-observation-changed-evidence");
    let changed_slice = load_slice(&observed, [key.clone()]).unwrap();
    commit_observed_snapshot(
        &mut observed,
        &catalog,
        &changed_slice,
        &changed_turn,
        ObservedSnapshot {
            key: &key,
            values: &changed_invalid_values,
            lifecycles: &[],
            provenance: &second_provenance,
        },
    )
    .expect("changed external evidence remains an observation, not a refusal");
    let changed_finding = observed
        .ledger()
        .last()
        .unwrap()
        .events
        .iter()
        .find_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .expect("changed invalid evidence emits a finding");
    assert_ne!(changed_finding.id, first_finding.id);
    assert_ne!(changed_finding.dedupe_key, first_finding.dedupe_key);
}

#[test]
fn aggregated_read_findings_coalesce_only_exact_authorities() {
    let quality_type = quality_item_type();
    let catalog_types = vec![quality_type.clone()];
    let catalog = TypeCatalog::new(&catalog_types);
    let key = FactKey::new(ITEM, "aggregated-quality-read");
    let values = object(json!({"item_id": "aggregated-quality-read", "amount": 0}));
    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(vec![quality_type]);
    observed
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: values.clone(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("fixture retains source state");
    let version = observed.load(&key).expect("seeded fact").version;
    let slice = load_slice(&observed, [key.clone()]).expect("one-key shared slice");

    let mut source_a = source_provenance();
    source_a.mapping_version = MappingVersion("map_a@1".to_owned());
    source_a.transform = Some("map_a.header".to_owned());
    let turn_a = turn("aggregate-map-a");
    let snapshot_a = ObservedSnapshot {
        key: &key,
        values: &values,
        lifecycles: &[],
        provenance: &source_a,
    };
    let first = prepare_observed_snapshot(&catalog, &slice, &turn_a, snapshot_a)
        .expect("first bad source prepares a finding-only authority");
    let duplicate = prepare_observed_snapshot(&catalog, &slice, &turn_a, snapshot_a)
        .expect("same sealed source proof prepares identically");
    let mut source_b = source_a.clone();
    source_b.mapping_version = MappingVersion("map_b@1".to_owned());
    source_b.transform = Some("map_b.header".to_owned());
    let turn_b = turn("aggregate-map-b");
    let snapshot_b = ObservedSnapshot {
        key: &key,
        values: &values,
        lifecycles: &[],
        provenance: &source_b,
    };
    let distinct = prepare_observed_snapshot(&catalog, &slice, &turn_b, snapshot_b)
        .expect("distinct lineage retains its own authority");

    let (commit, mut authorization) = first.into_parts();
    let (_, duplicate_authorization) = duplicate.into_parts();
    let (_, distinct_authorization) = distinct.into_parts();
    authorization.merge(duplicate_authorization);
    authorization.merge(distinct_authorization);
    authorization.retain_read_findings(&commit.reads, &commit.writes, &slice);
    assert!(authorization.validates(&commit));

    observed
        .commit_observed(commit, authorization)
        .expect("shared CAS read admits each distinct sealed proof once");
    assert_eq!(observed.load(&key).expect("fact remains").version, version);
    let findings: Vec<_> = observed
        .ledger()
        .last()
        .expect("finding ledger entry")
        .events
        .iter()
        .filter_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .collect();
    assert_eq!(findings.len(), 2, "exact duplicate was folded only once");
    let mapping_versions: BTreeSet<_> = findings
        .iter()
        .filter_map(|finding| finding.provenance.first())
        .map(|lineage| lineage.provenance.mapping_version.0.as_str())
        .collect();
    assert_eq!(mapping_versions, BTreeSet::from(["map_a@1", "map_b@1"]));
    assert_eq!(
        findings
            .iter()
            .find(|finding| finding.provenance[0].provenance.mapping_version.0 == "map_a@1")
            .expect("map-a finding")
            .provenance[0]
            .observation_ids,
        vec![turn_a.id]
    );
    assert_eq!(
        findings
            .iter()
            .find(|finding| finding.provenance[0].provenance.mapping_version.0 == "map_b@1")
            .expect("map-b finding")
            .provenance[0]
            .observation_ids,
        vec![turn_b.id]
    );
}

#[test]
fn final_write_retention_keeps_the_last_sealed_authority_once() {
    let item_type = quality_item_type();
    let types = vec![item_type];
    let catalog = TypeCatalog::new(&types);
    let key = FactKey::new(ITEM, "final-write-authority");
    let valid = object(json!({"item_id": "final-write-authority", "amount": 1}));
    let invalid = object(json!({"item_id": "final-write-authority", "amount": 0}));
    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    observed
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: valid.clone(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("valid source state seeds");
    let initial_slice = load_slice(&observed, [key.clone()]).expect("base slice");
    let invalid_slice = slice_with_values(&initial_slice, &key, &invalid);
    let valid_slice = slice_with_values(&invalid_slice, &key, &valid);
    let final_slice = slice_with_values(&valid_slice, &key, &invalid);

    let mut first_provenance = source_provenance();
    first_provenance.mapping_version = MappingVersion("map_b_first@1".to_owned());
    let first = prepare_observed_snapshot(
        &catalog,
        &initial_slice,
        &turn("map-b-first"),
        ObservedSnapshot {
            key: &key,
            values: &invalid,
            lifecycles: &[],
            provenance: &first_provenance,
        },
    )
    .expect("first B write is authorized");
    let (_, mut authorization) = first.into_parts();

    let middle = prepare_observed_snapshot(
        &catalog,
        &invalid_slice,
        &turn("map-a-revert"),
        ObservedSnapshot {
            key: &key,
            values: &valid,
            lifecycles: &[],
            provenance: &source_provenance(),
        },
    )
    .expect("middle A write is authorized");
    let (_, middle_authorization) = middle.into_parts();
    authorization.merge(middle_authorization);

    let mut final_provenance = source_provenance();
    final_provenance.mapping_version = MappingVersion("map_b_final@1".to_owned());
    let final_write = prepare_observed_snapshot(
        &catalog,
        &valid_slice,
        &turn("map-b-final"),
        ObservedSnapshot {
            key: &key,
            values: &invalid,
            lifecycles: &[],
            provenance: &final_provenance,
        },
    )
    .expect("final B write is authorized");
    let (commit, final_authorization) = final_write.into_parts();
    authorization.merge(final_authorization);
    authorization.retain_writes(&commit.writes);
    authorization.retain_read_findings(&commit.reads, &commit.writes, &final_slice);

    assert_eq!(authorization.finding_count(), 1);
    assert!(authorization.validates(&commit));
    observed
        .commit_observed(commit, authorization)
        .expect("the aggregate write uses one exact final authority");
    let findings: Vec<_> = observed
        .ledger()
        .last()
        .expect("final write ledger entry")
        .events
        .iter()
        .filter_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .collect();
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].provenance[0].provenance.mapping_version.0,
        "map_b_final@1"
    );
}

#[test]
fn final_read_retention_drops_findings_for_reverted_intermediate_values() {
    let item_type = quality_item_type();
    let types = vec![item_type];
    let catalog = TypeCatalog::new(&types);
    let key = FactKey::new(ITEM, "final-read-authority");
    let valid = object(json!({"item_id": "final-read-authority", "amount": 1}));
    let invalid = object(json!({"item_id": "final-read-authority", "amount": 0}));
    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    observed
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: valid.clone(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("valid source state seeds");
    let initial_slice = load_slice(&observed, [key.clone()]).expect("base slice");
    let invalid_slice = slice_with_values(&initial_slice, &key, &invalid);

    let first = prepare_observed_snapshot(
        &catalog,
        &initial_slice,
        &turn("read-map-b-first"),
        ObservedSnapshot {
            key: &key,
            values: &invalid,
            lifecycles: &[],
            provenance: &source_provenance(),
        },
    )
    .expect("first B write is authorized");
    let (_, mut authorization) = first.into_parts();
    let unchanged_invalid = prepare_observed_snapshot(
        &catalog,
        &invalid_slice,
        &turn("read-map-b-noop"),
        ObservedSnapshot {
            key: &key,
            values: &invalid,
            lifecycles: &[],
            provenance: &source_provenance(),
        },
    )
    .expect("unchanged invalid B prepares a guarded-read finding");
    assert!(unchanged_invalid.writes.is_empty());
    let (_, read_authorization) = unchanged_invalid.into_parts();
    authorization.merge(read_authorization);
    let revert = prepare_observed_snapshot(
        &catalog,
        &invalid_slice,
        &turn("read-map-a-revert"),
        ObservedSnapshot {
            key: &key,
            values: &valid,
            lifecycles: &[],
            provenance: &source_provenance(),
        },
    )
    .expect("A restores the original effective state");
    let (_, revert_authorization) = revert.into_parts();
    authorization.merge(revert_authorization);

    let initial_version = initial_slice.get(&key).expect("initial state").version;
    let mut commit: CellCommit<ObservationEvent, JsonObject, JsonObject> = CellCommit::empty();
    commit.reads.push(FactExpectation {
        key,
        expected: initial_version,
    });
    authorization.retain_writes(&commit.writes);
    authorization.retain_read_findings(&commit.reads, &commit.writes, &initial_slice);

    assert_eq!(authorization.finding_count(), 0);
    assert!(authorization.validates(&commit));
    observed
        .commit_observed(commit, authorization)
        .expect("the final A read has no stale B finding authority");
    assert!(
        observed
            .ledger()
            .iter()
            .flat_map(|entry| &entry.events)
            .all(|event| !matches!(event, ObservationEvent::DataQualityFinding { .. }))
    );
}

#[test]
fn repeated_guarded_read_state_shares_fuel_and_keeps_each_authority() {
    const AUTHORITY_COUNT: usize = 6;
    let text_bytes = usize::try_from(EvaluationBudget::default().units() / 10)
        .expect("fuel fixture fits platform");
    let (quality_type, values) =
        fuel_fixture_with_class(false, norn_ir::ConstraintClass::Quality, text_bytes);
    let types = vec![quality_type];
    let catalog = TypeCatalog::new(&types);
    let key = FactKey::new(FUEL_ITEM, "shared-quality-read");
    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    observed
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: values.clone(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("fixture retains externally reported state");
    let slice = load_slice(&observed, [key.clone()]).expect("one-key shared slice");
    let mut aggregate_commit = None;
    let mut authorization = ObservationAuthorization::empty();

    for index in 0..AUTHORITY_COUNT {
        let mut provenance = source_provenance();
        provenance.mapping_version = MappingVersion(format!("map_{index}@1"));
        provenance.transform = Some(format!("map_{index}.payload"));
        let observation_turn = turn(&format!("shared-read-map-{index}"));
        let prepared = prepare_observed_snapshot(
            &catalog,
            &slice,
            &observation_turn,
            ObservedSnapshot {
                key: &key,
                values: &values,
                lifecycles: &[],
                provenance: &provenance,
            },
        )
        .expect("each expensive read authority fits independently");
        let (part_commit, part_authorization) = prepared.into_parts();
        assert!(part_commit.writes.is_empty());
        if aggregate_commit.is_none() {
            aggregate_commit = Some(part_commit);
        }
        authorization.merge(part_authorization);
    }

    let commit = aggregate_commit.expect("at least one prepared authority");
    authorization.retain_read_findings(&commit.reads, &commit.writes, &slice);
    assert!(authorization.validates(&commit));

    observed
        .commit_observed(commit, authorization)
        .expect("one guarded state is evaluated once for all source authorities");
    let findings: Vec<_> = observed
        .ledger()
        .last()
        .expect("finding ledger entry")
        .events
        .iter()
        .filter_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .collect();
    assert_eq!(findings.len(), AUTHORITY_COUNT);
    let mapping_versions: BTreeSet<_> = findings
        .iter()
        .filter_map(|finding| finding.provenance.first())
        .map(|lineage| lineage.provenance.mapping_version.0.as_str())
        .collect();
    assert_eq!(mapping_versions.len(), AUTHORITY_COUNT);
}

#[test]
fn distinct_guarded_read_states_still_share_and_exhaust_commit_fuel() {
    const STATE_COUNT: usize = 6;
    let fuel_limit = EvaluationBudget::default().units();
    let text_bytes = usize::try_from(fuel_limit / 10).expect("fuel fixture fits platform");
    let (quality_type, values) =
        fuel_fixture_with_class(false, norn_ir::ConstraintClass::Quality, text_bytes);
    let types = vec![quality_type];
    let catalog = TypeCatalog::new(&types);
    let keys: Vec<_> = (0..STATE_COUNT)
        .map(|index| FactKey::new(FUEL_ITEM, format!("independent-read-{index}")))
        .collect();
    let mut observed: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    observed
        .commit(CellCommit {
            writes: keys
                .iter()
                .cloned()
                .map(|key| FactWrite {
                    key,
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: values.clone(),
                    },
                })
                .collect(),
            ..CellCommit::empty()
        })
        .expect("fixture retains all externally reported states");
    let slice = load_slice(&observed, keys.clone()).expect("six-key shared slice");
    let mut aggregate_commit = None;
    let mut authorization = ObservationAuthorization::empty();

    for (index, key) in keys.iter().enumerate() {
        let mut provenance = source_provenance();
        provenance.mapping_version = MappingVersion(format!("independent_{index}@1"));
        provenance.transform = Some(format!("independent_{index}.payload"));
        let observation_turn = turn(&format!("independent-read-{index}"));
        let prepared = prepare_observed_snapshot(
            &catalog,
            &slice,
            &observation_turn,
            ObservedSnapshot {
                key,
                values: &values,
                lifecycles: &[],
                provenance: &provenance,
            },
        )
        .expect("each expensive read authority fits independently");
        let (part_commit, part_authorization) = prepared.into_parts();
        assert!(part_commit.writes.is_empty());
        if aggregate_commit.is_none() {
            aggregate_commit = Some(part_commit);
        }
        authorization.merge(part_authorization);
    }

    let commit = aggregate_commit.expect("at least one prepared authority");
    authorization.retain_read_findings(&commit.reads, &commit.writes, &slice);
    assert!(authorization.validates(&commit));
    let prior_entries = observed.ledger().len();
    let error = observed.commit_observed(commit, authorization).unwrap_err();
    assert_eq!(error.code(), "commit.invariant_budget_exceeded");
    assert!(matches!(
        error,
        CommitError::InvariantBudgetExceeded { limit, .. } if limit == fuel_limit
    ));
    assert_eq!(observed.ledger().len(), prior_entries);
    for key in keys {
        assert_eq!(observed.load(&key).unwrap().values, Some(values.clone()));
    }
}

#[test]
fn read_only_findings_are_revalidated_against_the_commit_time_catalog() {
    let quality_type = quality_item_type();
    let catalog_types = vec![quality_type.clone()];
    let catalog = TypeCatalog::new(&catalog_types);
    let key = FactKey::new(ITEM, "catalog-changed");
    let values = object(json!({"item_id": "catalog-changed", "amount": 0}));
    let mut cell: ObservationCell = MemoryCell::new(|_| JsonObject::new());
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: values.clone(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("an unmodeled host can seed the external value");

    let slice = load_slice(&cell, [key.clone()]).expect("one-key slice");
    let provenance = source_provenance();
    let prepared = prepare_observed_snapshot(
        &catalog,
        &slice,
        &turn("catalog-bound-observation"),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .expect("the quality violation prepares a finding-only observation");
    assert!(prepared.writes.is_empty());
    let (commit, authorization) = prepared.into_parts();

    let mut integrity_type = quality_type;
    integrity_type.invariants[0].classification = norn_ir::ConstraintClass::Integrity;
    cell = cell.with_types(vec![integrity_type]);
    let prior_entries = cell.ledger().len();
    let error = cell.commit_observed(commit, authorization).unwrap_err();

    assert_eq!(error.code(), "commit.observation_authorization");
    assert_eq!(cell.ledger().len(), prior_entries);
}

#[test]
fn stale_read_only_finding_cas_does_not_append_an_event() {
    let quality_type = quality_item_type();
    let catalog_types = vec![quality_type.clone()];
    let catalog = TypeCatalog::new(&catalog_types);
    let key = FactKey::new(ITEM, "stale-finding-cas");
    let invalid_values = object(json!({"item_id": "stale-finding-cas", "amount": 0}));
    let mut cell: ObservationCell = MemoryCell::new(|_| JsonObject::new());
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: invalid_values.clone(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("an unmodeled host can seed the external value");
    cell = cell.with_types(vec![quality_type]);

    let slice = load_slice(&cell, [key.clone()]).expect("one-key slice");
    let provenance = source_provenance();
    let prepared = prepare_observed_snapshot(
        &catalog,
        &slice,
        &turn("stale-finding-observation"),
        ObservedSnapshot {
            key: &key,
            values: &invalid_values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .expect("the unchanged quality violation prepares a finding-only observation");
    assert!(prepared.writes.is_empty());
    let (commit, authorization) = prepared.into_parts();

    let changed_values = object(json!({"item_id": "stale-finding-cas", "amount": -1}));
    let current_version = cell.load(&key).unwrap().version;
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key,
            expected: current_version,
            mutation: FactMutation::Put {
                values: changed_values,
            },
        }],
        ..CellCommit::empty()
    })
    .expect("quality violations remain admissible on commands");
    let prior_entries = cell.ledger().len();

    let error = cell.commit_observed(commit, authorization).unwrap_err();

    assert_eq!(error.code(), "commit.conflict");
    assert_eq!(cell.ledger().len(), prior_entries);
}

#[test]
fn season_window_invariant_admits_leap_and_equal_dates_and_refuses_reversed_dates() {
    let types = vec![season_type()];
    let mut cell: CommandCell = MemoryCell::new(|_| JsonObject::new()).with_types(types);

    let mut accepted = Vec::new();
    for (key_name, start, end) in [
        ("leap-crossing", "2024-02-29", "2024-03-01"),
        ("equal-dates", "2024-02-29", "2024-02-29"),
    ] {
        let key = FactKey::new(SEASON_ITEM, key_name);
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({
                        "season_start": start,
                        "season_end": end
                    })),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("ordered and equal season dates satisfy the invariant");
        accepted.push((key.clone(), cell.load(&key).unwrap()));
    }

    let reversed = FactKey::new(SEASON_ITEM, "reversed-dates");
    let ledger_len = cell.ledger().len();
    let error = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: reversed.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({
                        "season_start": "2024-03-01",
                        "season_end": "2024-02-29"
                    })),
                },
            }],
            ..CellCommit::empty()
        })
        .expect_err("a season end before its start must be refused");

    assert!(matches!(
        error,
        CommitError::InvariantViolation {
            owner: InvariantOwner::Object(ref owner),
            ref id,
            outcome,
            ..
        } if owner.as_str() == SEASON_ITEM
            && id.as_str() == "season_window"
            && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
    ));
    assert!(cell.load(&reversed).unwrap().values.is_none());
    assert_eq!(cell.ledger().len(), ledger_len);
    assert!(cell.outbox().is_empty());
    assert!(cell.timers().is_empty());
    for (key, before) in accepted {
        assert_eq!(cell.load(&key).unwrap(), before);
    }
}

#[test]
fn omitted_nullable_default_is_evaluated_and_observed_value_is_retained() {
    let object_type = negative_default_type();
    let catalog = TypeCatalog::new(std::slice::from_ref(&object_type));
    let key = FactKey::new(ITEM, "negative-default");
    let mut command_cell =
        CommandCell::new(|command| command.payload.clone()).with_types(vec![object_type.clone()]);
    let error = command_cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect_err("the effective -1 default violates the invariant");
    assert_eq!(error.code(), "commit.invariant_violation");
    assert!(matches!(
        error,
        CommitError::InvariantViolation {
            ref id,
            outcome,
            ..
        } if id.as_str() == "nonnegative_amount"
            && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
    ));
    assert!(command_cell.load(&key).unwrap().values.is_none());
    assert!(command_cell.ledger().is_empty());

    // Seed a legacy/incomplete host value before attaching the model. The
    // observation path must materialize the same default, report the finding,
    // and retain exactly that value.
    let mut observation_cell: ObservationCell = MemoryCell::new(|_| JsonObject::new());
    observation_cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("seed incomplete pre-model value");
    observation_cell = observation_cell.with_types(vec![object_type.clone()]);
    let slice = load_slice(&observation_cell, [key.clone()]).expect("loads existing value");
    let provenance = source_provenance();
    let empty_observed = JsonObject::new();
    let snapshot = ObservedSnapshot {
        key: &key,
        values: &empty_observed,
        lifecycles: &[],
        provenance: &provenance,
    };
    commit_observed_snapshot(
        &mut observation_cell,
        &catalog,
        &slice,
        &turn("turn-negative-default-observation"),
        snapshot,
    )
    .expect("observed data is retained with a quality finding");
    assert_eq!(
        observation_cell.load(&key).unwrap().values,
        Some(object(json!({"amount": -1})))
    );
    let entry = observation_cell
        .ledger()
        .last()
        .expect("observation ledger entry");
    assert!(matches!(
        entry.events.iter().find(|event| matches!(event, ObservationEvent::DataQualityFinding { .. })),
        Some(ObservationEvent::DataQualityFinding { fact })
            if matches!(
                    &fact.rule_ref,
                    DataQualityRuleRef::Invariant {
                        owner: InvariantOwner::Object(owner),
                        invariant_id,
                        ..
                    } if owner.as_str() == ITEM && invariant_id.as_str() == "nonnegative_amount"
                )
                && fact.subjects == vec![DataQualitySubject::new(
                    ObjectTypeName(ITEM.to_owned()),
                    key.key.clone(),
                )]
                && fact.severity == DataQualitySeverity::Error
                && fact.outcome == DataQualityOutcome::NotSatisfied
    ));
}

#[test]
fn canonical_numeric_defaults_are_evaluated_and_stored_as_runtime_numbers() {
    let object_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.NumericDefaults",
        "properties": [
            {
                "id": "amount",
                "type": {"kind": "integer"},
                "default": "1"
            },
            {
                "id": "rate",
                "type": {"kind": "decimal", "scale": 2},
                "default": "12.50"
            }
        ],
        "invariants": [
            {
                "id": "positive_amount",
                "expression": "self.amount == 1",
                "message": "amount must equal one"
            },
            {
                "id": "exact_rate",
                "expression": "self.rate == 12.5",
                "message": "rate must equal twelve point five"
            }
        ]
    }))
    .expect("numeric defaults decode");
    let key = FactKey::new("test.NumericDefaults", "defaults");
    let mut cell =
        CommandCell::new(|command| command.payload.clone()).with_types(vec![object_type]);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("the invariant evaluator sees exact numeric defaults");

    let expected = JsonObject::from([
        (
            "amount".to_owned(),
            norn_ir::RuntimeValue::Number(norn_ir::Decimal::parse("1").unwrap()),
        ),
        (
            "rate".to_owned(),
            norn_ir::RuntimeValue::Number(norn_ir::Decimal::parse("12.50").unwrap()),
        ),
    ]);
    assert_eq!(cell.load(&key).unwrap().values, Some(expected));
}

#[test]
fn referenced_numeric_defaults_are_resolved_before_invariant_evaluation() {
    let property_types: Vec<PropertyTypeDef> = [
        json!({"name": "shared.count", "type": {"kind": "integer"}}),
        json!({"name": "shared.rate", "type": {"kind": "decimal", "scale": 2}}),
    ]
    .into_iter()
    .map(|definition| serde_json::from_value(definition).expect("shared type decodes"))
    .collect();
    assert!(norn_checker::check_property_type_defs(&property_types, &BTreeSet::new()).is_empty());

    let object_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.ReferencedNumericDefaults",
        "properties": [
            {
                "id": "amount",
                "type": {"kind": "ref", "name": "shared.count"},
                "default": "1"
            },
            {
                "id": "rate",
                "type": {"kind": "ref", "name": "shared.rate"},
                "default": "12.50"
            }
        ],
        "invariants": [
            {
                "id": "positive_amount",
                "expression": "self.amount == 1",
                "message": "amount must equal one"
            },
            {
                "id": "exact_rate",
                "expression": "self.rate == 12.5",
                "message": "rate must equal twelve point five"
            }
        ]
    }))
    .expect("object type with referenced numeric defaults decodes");
    let types = [object_type];
    let type_name = ObjectTypeName("test.ReferencedNumericDefaults".to_owned());
    let catalog = TypeCatalog::new(&types).with_property_types(&property_types);
    let effective = catalog
        .effective_object_values(&type_name, &JsonObject::new())
        .expect("referenced defaults materialize");
    assert_eq!(
        effective.get("amount"),
        Some(&RuntimeValue::Number(norn_ir::Decimal::parse("1").unwrap()))
    );
    assert_eq!(
        effective.get("rate"),
        Some(&RuntimeValue::Number(
            norn_ir::Decimal::parse("12.50").unwrap()
        ))
    );

    let mut remaining = norn_kernel::invariant_evaluation_budget();
    assert!(
        norn_kernel::evaluate_invariants(
            &catalog,
            &FactKey::new(type_name.as_str(), "referenced"),
            &JsonObject::new(),
            &mut remaining,
        )
        .expect("referenced defaults satisfy their invariants")
        .is_empty()
    );
}

#[test]
fn unresolved_and_cyclic_referenced_defaults_fail_closed() {
    let missing_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.MissingReferenceDefault",
        "properties": [{
            "id": "amount",
            "type": {"kind": "ref", "name": "shared.missing"},
            "default": "1"
        }]
    }))
    .expect("missing reference fixture decodes");
    let missing_catalog = TypeCatalog::new(std::slice::from_ref(&missing_type));
    assert!(
        missing_catalog
            .effective_object_values(
                &ObjectTypeName("test.MissingReferenceDefault".to_owned()),
                &JsonObject::new(),
            )
            .is_err()
    );

    let property_types: Vec<PropertyTypeDef> = [
        json!({"name": "shared.first", "type": {"kind": "ref", "name": "shared.second"}}),
        json!({"name": "shared.second", "type": {"kind": "ref", "name": "shared.first"}}),
    ]
    .into_iter()
    .map(|definition| serde_json::from_value(definition).expect("cyclic type decodes"))
    .collect();
    let cyclic_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.CyclicReferenceDefault",
        "properties": [{
            "id": "amount",
            "type": {"kind": "ref", "name": "shared.first"},
            "default": "1"
        }]
    }))
    .expect("cyclic reference fixture decodes");
    let cyclic_catalog =
        TypeCatalog::new(std::slice::from_ref(&cyclic_type)).with_property_types(&property_types);
    assert!(
        cyclic_catalog
            .effective_object_values(
                &ObjectTypeName("test.CyclicReferenceDefault".to_owned()),
                &JsonObject::new(),
            )
            .is_err()
    );
}

#[test]
fn missing_runtime_interface_metadata_fails_closed() {
    let (types, _) = definitions();
    let mut cell = CommandCell::new(|command| command.payload.clone()).with_types(types);
    let error = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(ITEM, "missing-interface"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({"item_id": "present", "amount": 1, "status": "draft"})),
                },
            }],
            ..CellCommit::empty()
        })
        .unwrap_err();
    assert_eq!(error.code(), "commit.invariant_model");
    assert!(cell.ledger().is_empty());
}

#[test]
fn decisions_refuse_invalid_create_set_transition_and_accept_valid_final_states() {
    let (types, interfaces) = definitions();
    let catalog = catalog(&types, &interfaces);
    let mut cell = cell();
    let created = FactKey::new(ITEM, "decision-create");
    let empty_slice = load_slice(&cell, []).unwrap();
    let before = cell.ledger().len();
    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &empty_slice,
        &turn("turn-create"),
        &output(
            &json!([{"op": "create", "objectType": ITEM, "key": created.key,
                   "values": {"item_id": "decision-create", "amount": 0, "status": "draft"}}]),
            true,
        ),
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        DecisionRefusal::InvariantViolation {
            key: ref refused,
            owner: InvariantOwner::Object(ref owner),
            ref id,
            outcome,
            ..
        } if refused.as_ref() == &created
            && owner.as_str() == ITEM
            && id.as_str() == "positive_amount"
            && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
    ));
    assert!(cell.load(&created).unwrap().values.is_none());
    assert_refusal_record(
        &cell,
        before,
        &created,
        &InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
        "positive_amount",
        "turn-create",
    );

    let existing = seed(&mut cell, ITEM, "decision-set", 12);
    let before = cell.ledger().len();
    let slice = load_slice(&cell, [existing.clone()]).unwrap();
    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("turn-set"),
        &output(
            &json!([{"op": "set", "objectType": ITEM, "key": existing.key,
                   "property": "amount", "value": 0}]),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(refusal.code(), "decision.invariant_violation");
    assert_eq!(
        cell.load(&existing).unwrap().values,
        Some(object(
            json!({"item_id": "decision-set", "amount": 12, "status": "draft"})
        ))
    );
    assert_refusal_record(
        &cell,
        before,
        &existing,
        &InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
        "positive_amount",
        "turn-set",
    );

    let transition_target = seed(&mut cell, ITEM, "decision-transition", 8);
    let before = cell.ledger().len();
    let slice = load_slice(&cell, [transition_target.clone()]).unwrap();
    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("turn-transition"),
        &output(
            &json!([{
                "op": "transition", "objectType": ITEM,
                "key": transition_target.key, "lifecycle": "status",
                "transition": "approve"
            }]),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(refusal.code(), "decision.invariant_violation");
    assert_eq!(
        cell.load(&transition_target).unwrap().values,
        Some(object(json!({
            "item_id": "decision-transition", "amount": 8, "status": "draft"
        })))
    );
    assert_refusal_record(
        &cell,
        before,
        &transition_target,
        &InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
        "not_approved",
        "turn-transition",
    );

    let patched = seed(&mut cell, ITEM, "decision-patch", 12);
    let slice = load_slice(&cell, [patched.clone()]).unwrap();
    let receipt = commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("turn-final-patch"),
        &output(
            &json!([
                {"op": "set", "objectType": ITEM, "key": "decision-patch",
                 "property": "amount", "value": 0},
                {"op": "set", "objectType": ITEM, "key": "decision-patch",
                 "property": "item_id", "value": "decision-patch-updated"},
                {"op": "set", "objectType": ITEM, "key": "decision-patch",
                 "property": "amount", "value": 5}
            ]),
            true,
        ),
    )
    .expect("invariants apply to the final multi-property state");
    assert_eq!(receipt.written.len(), 1);
    assert_eq!(
        cell.load(&patched).unwrap().values,
        Some(object(json!({
            "item_id": "decision-patch-updated", "amount": 5, "status": "draft"
        })))
    );
    assert_eq!(cell.outbox().len(), 1);
    assert_eq!(cell.timers().len(), 1);
    assert!(
        !cell
            .ledger()
            .last()
            .unwrap()
            .events
            .iter()
            .any(|event| matches!(event, FactEvent::InvariantRefused { .. }))
    );
}

#[test]
fn subtype_create_is_checked_against_subtype_and_interface_invariants() {
    let (types, interfaces) = definitions();
    let catalog = catalog(&types, &interfaces);
    let mut cell = cell();
    let key = FactKey::new(SUBTYPE, "interface-empty-id");
    let empty_slice = load_slice(&cell, []).unwrap();
    let before = cell.ledger().len();
    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &empty_slice,
        &turn("turn-interface"),
        &output(
            &json!([{"op": "create", "objectType": SUBTYPE, "key": key.key,
                   "values": {"item_id": "", "amount": 5}}]),
            false,
        ),
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        DecisionRefusal::InvariantViolation {
            owner: InvariantOwner::Interface(ref owner),
            ref id,
            ..
        } if owner.as_str() == INTERFACE && id.as_str() == "named_item"
    ));
    assert_refusal_record(
        &cell,
        before,
        &key,
        &InvariantOwner::Interface(norn_ir::InterfaceName(INTERFACE.to_owned())),
        "named_item",
        "turn-interface",
    );

    let key = FactKey::new(SUBTYPE, "subtype-limit");
    let before = cell.ledger().len();
    let empty_slice = load_slice(&cell, []).unwrap();
    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &empty_slice,
        &turn("turn-subtype-limit"),
        &output(
            &json!([{"op": "create", "objectType": SUBTYPE, "key": key.key,
                   "values": {"item_id": "present", "amount": 100, "status": "draft"}}]),
            false,
        ),
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        DecisionRefusal::InvariantViolation {
            owner: InvariantOwner::Object(ref owner),
            ref id,
            ..
        } if owner.as_str() == SUBTYPE && id.as_str() == "below_limit"
    ));
    assert_refusal_record(
        &cell,
        before,
        &key,
        &InvariantOwner::Object(ObjectTypeName(SUBTYPE.to_owned())),
        "below_limit",
        "turn-subtype-limit",
    );
}

#[test]
fn actions_transition_and_patch_refusals_do_not_write_record_or_enqueue_command() {
    let (types, interfaces) = definitions();
    let catalog = catalog(&types, &interfaces);
    let mut cell = cell();
    let target = seed(&mut cell, ITEM, "action-transition", 5);
    let transition = action("fire_transition");
    let invocation = action_call(
        &transition,
        json!({"item_id": target.key}),
        "transition-once",
        "turn-action-transition",
    );
    let before = cell.ledger().len();
    let refusal = commit_action(&mut cell, &catalog, &invocation, &allow_action()).unwrap_err();
    assert_eq!(refusal.code(), "decision.invariant_violation");
    assert!(matches!(
        &refusal,
        ActionRefusal::Decision(DecisionRefusal::InvariantViolation {
            key,
            owner: InvariantOwner::Object(owner),
            id,
            outcome,
            ..
        }) if key.as_ref() == &target
            && owner.as_str() == ITEM
            && id.as_str() == "not_approved"
            && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
    ));
    assert_eq!(
        cell.load(&target).unwrap().values,
        Some(object(
            json!({"item_id": "action-transition", "amount": 5, "status": "draft"})
        ))
    );
    assert_refusal_record(
        &cell,
        before,
        &target,
        &InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
        "not_approved",
        "invariant.fire_transition/transition-once",
    );

    let create = action("upsert_input_object");
    let invocation = action_call(
        &create,
        json!({"item_id": "action-create", "amount": 0, "status": "draft"}),
        "create-once",
        "turn-action-create",
    );
    let created = FactKey::new(ITEM, "action-create");
    let before = cell.ledger().len();
    let refusal = commit_action(&mut cell, &catalog, &invocation, &allow_action()).unwrap_err();
    assert_eq!(refusal.code(), "decision.invariant_violation");
    assert!(cell.load(&created).unwrap().values.is_none());
    assert_refusal_record(
        &cell,
        before,
        &created,
        &InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
        "positive_amount",
        "invariant.upsert_input_object/create-once",
    );

    let target = seed(&mut cell, ITEM, "action-patch", 7);
    let patch = action("upsert_input_object");
    let invocation = action_call(
        &patch,
        json!({"item_id": target.key, "amount": 0}),
        "patch-once",
        "turn-action-patch",
    );
    let before = cell.ledger().len();
    let refusal = commit_action(&mut cell, &catalog, &invocation, &allow_action()).unwrap_err();
    assert_eq!(refusal.code(), "decision.invariant_violation");
    assert_eq!(
        cell.load(&target).unwrap().values,
        Some(object(
            json!({"item_id": "action-patch", "amount": 7, "status": "draft"})
        ))
    );
    assert_refusal_record(
        &cell,
        before,
        &target,
        &InvariantOwner::Object(ObjectTypeName(ITEM.to_owned())),
        "positive_amount",
        "invariant.upsert_input_object/patch-once",
    );
}

#[test]
fn action_numeric_strings_are_canonical_before_invariants_and_storage() {
    let base_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": NUMERIC_STRING_BASE,
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "amount", "type": {"kind": "integer"}},
            {"id": "rate", "type": {"kind": "decimal", "scale": 2}},
            {
                "id": "status",
                "type": {"kind": "enum", "values": [
                    {"value": "draft"}, {"value": "submitted"}
                ]},
                "default": "draft"
            }
        ],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "transitions": [{"id": "submit", "from": ["draft"], "to": "submitted"}]
        }]
    }))
    .expect("numeric string base type");
    let numeric_string_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": NUMERIC_STRING_ITEM,
        "extends": NUMERIC_STRING_BASE,
        "properties": [{
            "id": "status",
            "type": {"kind": "enum", "values": [{"value": "draft"}]}
        }],
        "invariants": [
            {
                "id": "amount_not_negative_one",
                "expression": "self.amount != -1",
                "message": "amount cannot be negative one"
            },
            {
                "id": "rate_not_negative_one_point_two_five",
                "expression": "self.rate != -1.25",
                "message": "rate cannot be negative one point two five"
            }
        ]
    }))
    .expect("numeric string object type");
    let types = [base_type, numeric_string_type];
    let catalog = TypeCatalog::new(&types);
    let mut cell = CommandCell::new(|command| command.payload.clone()).with_types(types.to_vec());
    let action = numeric_string_action();

    for (key, input, invariant_id) in [
        (
            "negative-integer-string",
            json!({"item_id": "negative-integer-string", "amount": "-1", "rate": "1.25"}),
            "amount_not_negative_one",
        ),
        (
            "negative-decimal-string",
            json!({"item_id": "negative-decimal-string", "amount": "4", "rate": "-1.25"}),
            "rate_not_negative_one_point_two_five",
        ),
    ] {
        let key = FactKey::new(NUMERIC_STRING_ITEM, key);
        let before = cell.ledger().len();
        let refusal = commit_action(
            &mut cell,
            &catalog,
            &numeric_string_action_call(&action, input, &key.key, &key.key),
            &allow_action(),
        )
        .expect_err("a forbidden numeric string must be checked as its persisted number");
        assert_eq!(refusal.code(), "decision.invariant_violation");
        assert!(matches!(
            &refusal,
            ActionRefusal::Decision(DecisionRefusal::InvariantViolation {
                owner: InvariantOwner::Object(owner),
                id,
                outcome,
                ..
            }) if owner.as_str() == NUMERIC_STRING_ITEM
                && id.as_str() == invariant_id
                && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
        ));
        assert!(cell.load(&key).unwrap().values.is_none());
        assert_refusal_record(
            &cell,
            before,
            &key,
            &InvariantOwner::Object(ObjectTypeName(NUMERIC_STRING_ITEM.to_owned())),
            invariant_id,
            &format!("invariant.upsert_numeric_string/{}", key.key),
        );
    }

    let accepted_key = FactKey::new(NUMERIC_STRING_ITEM, "positive-numeric-strings");
    commit_action(
        &mut cell,
        &catalog,
        &numeric_string_action_call(
            &action,
            json!({"item_id": accepted_key.key, "amount": "7", "rate": "12.50"}),
            "positive-numeric-strings",
            "positive-numeric-strings",
        ),
        &allow_action(),
    )
    .expect("valid numeric strings are accepted");
    assert_eq!(
        cell.load(&accepted_key).unwrap().values,
        Some(JsonObject::from([
            (
                "item_id".to_owned(),
                RuntimeValue::String("positive-numeric-strings".to_owned()),
            ),
            (
                "amount".to_owned(),
                RuntimeValue::Number(norn_ir::Decimal::parse("7").unwrap()),
            ),
            (
                "rate".to_owned(),
                RuntimeValue::Number(norn_ir::Decimal::parse("12.50").unwrap()),
            ),
            (
                "status".to_owned(),
                RuntimeValue::String("draft".to_owned()),
            ),
        ]))
    );
}

fn source_provenance() -> InstanceProvenance {
    InstanceProvenance {
        source_fields: Vec::new(),
        markings: Vec::new(),
        source: SourceRecordRef::new("ERPNext", "Item/ITEM-42"),
        mapping_version: MappingVersion("erpnext.item@4".to_owned()),
        transform: Some("item.attributes".to_owned()),
        observed_at: Some(UtcTimestamp("2026-10-04T12:00:00Z".to_owned())),
        recorded_at: UtcTimestamp("2026-10-05T00:00:00Z".to_owned()),
        upstream: vec![FactRef::new("erp.SourceRecord", "source-head", 42)],
    }
}

fn delivery_provenance() -> Provenance {
    Provenance {
        source: "ERPNext".to_owned(),
        source_id: "Item/ITEM-42".to_owned(),
        observed_at: LogicalTime::from_millis(1_000),
        mapping_version: "erpnext.item@4".to_owned(),
    }
}

fn fuel_fixture(should_satisfy: bool) -> (ObjectTypeDef, JsonObject) {
    let fuel_limit = EvaluationBudget::default().units();
    let text_bytes = usize::try_from(fuel_limit / 10).expect("fuel fixture fits platform");
    fuel_fixture_with_class(
        should_satisfy,
        norn_ir::ConstraintClass::Integrity,
        text_bytes,
    )
}

fn fuel_fixture_with_class(
    should_satisfy: bool,
    classification: norn_ir::ConstraintClass,
    text_bytes: usize,
) -> (ObjectTypeDef, JsonObject) {
    let expected_length = text_bytes
        .checked_mul(2)
        .expect("two fuel strings fit in usize");
    let comparison_length = if should_satisfy {
        expected_length
    } else {
        expected_length + 1
    };
    let expression = format!("len(self.payload) + len(self.auxiliary) == {comparison_length}");
    let mut object_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": FUEL_ITEM,
        "properties": [
            {"id": "record_id", "type": {"kind": "text"}},
            {"id": "payload", "type": {"kind": "text"}},
            {"id": "auxiliary", "type": {"kind": "text"}}
        ],
        "invariants": [{
            "id": "payload_lengths",
            "expression": expression,
            "message": "both payloads must have the expected length"
        }]
    }))
    .expect("fuel object type");
    object_type.invariants[0].classification = classification;
    let values = object(json!({
        "record_id": "fuel-fixture",
        "payload": "p".repeat(text_bytes),
        "auxiliary": "a".repeat(text_bytes)
    }));
    (object_type, values)
}

#[test]
fn raw_commit_uses_one_cumulative_invariant_fuel_budget() {
    const BATCH_SIZE: usize = 6;
    let fuel_limit = EvaluationBudget::default().units();
    let (object_type, values) = fuel_fixture(true);
    let types = vec![object_type];
    let mut cell: CommandCell = MemoryCell::new(|_| JsonObject::new()).with_types(types);
    let single = FactKey::new(FUEL_ITEM, "one-evaluation");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: single,
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: values.clone(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("one invariant evaluation fits the per-commit budget");

    let keys: Vec<_> = (0..BATCH_SIZE)
        .map(|index| FactKey::new(FUEL_ITEM, format!("raw-{index}")))
        .collect();
    let prior_entries = cell.ledger().len();
    let error = cell
        .commit(CellCommit {
            writes: keys
                .iter()
                .cloned()
                .map(|key| FactWrite {
                    key,
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: values.clone(),
                    },
                })
                .collect(),
            events: vec![FactEvent::Created {
                key: keys[0].clone(),
                values,
            }],
            effects: vec![norn_kernel::Effect::Schedule(norn_kernel::Timer {
                id: norn_kernel::TimerId("fuel-refusal-timer".to_owned()),
                due: LogicalTime::from_millis(1),
                payload: JsonObject::new(),
            })],
            ..CellCommit::empty()
        })
        .unwrap_err();
    assert_eq!(error.code(), "commit.invariant_budget_exceeded");
    assert!(matches!(
        error,
        CommitError::InvariantBudgetExceeded { limit, .. } if limit == fuel_limit
    ));
    assert_eq!(cell.ledger().len(), prior_entries);
    for key in keys {
        assert!(cell.load(&key).unwrap().values.is_none());
    }
    assert!(cell.outbox().is_empty());
    assert!(cell.timers().is_empty());
}

#[test]
fn quality_invariants_consume_no_command_fuel_across_raw_decision_and_action_writes() {
    const BATCH_SIZE: usize = 6;
    let text_bytes = usize::try_from(EvaluationBudget::default().units() / 10)
        .expect("fuel fixture fits platform");
    let (object_type, values) =
        fuel_fixture_with_class(false, norn_ir::ConstraintClass::Quality, text_bytes);
    let values_json = json!({
        "record_id": "fuel-fixture",
        "payload": "p".repeat(text_bytes),
        "auxiliary": "a".repeat(text_bytes)
    });
    let types = vec![object_type];
    let catalog = TypeCatalog::new(&types);
    let mut cell: CommandCell = MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());

    let raw_keys: Vec<_> = (0..BATCH_SIZE)
        .map(|index| FactKey::new(FUEL_ITEM, format!("quality-raw-{index}")))
        .collect();
    cell.commit(CellCommit {
        writes: raw_keys
            .iter()
            .cloned()
            .map(|key| FactWrite {
                key,
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: values.clone(),
                },
            })
            .collect(),
        ..CellCommit::empty()
    })
    .expect("raw command admits quality-invalid data without quality fuel");
    for key in &raw_keys {
        assert_eq!(cell.load(key).unwrap().values, Some(values.clone()));
    }

    let decision_keys: Vec<_> = (0..BATCH_SIZE)
        .map(|index| FactKey::new(FUEL_ITEM, format!("quality-decision-{index}")))
        .collect();
    let decision_facts: Vec<_> = decision_keys
        .iter()
        .map(|key| {
            json!({
                "op": "create",
                "objectType": FUEL_ITEM,
                "key": key.key.clone(),
                "values": values_json.clone()
            })
        })
        .collect();
    let slice = load_slice(&cell, []).expect("empty decision slice");
    commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("turn-quality-decision-fuel"),
        &output(&Value::Array(decision_facts), false),
    )
    .expect("decision writes admit quality data without quality fuel");
    for key in &decision_keys {
        assert_eq!(cell.load(key).unwrap().values, Some(values.clone()));
    }

    let action: ActionRoot = serde_json::from_value(json!({
        "actionId": "invariant.upsert_fuel_item",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": false,
        "ledgered": true,
        "ledgerCategory": "invariant.test",
        "inputSchema": {
            "type": "object",
            "properties": {
                "record_id": {"type": "string"},
                "payload": {"type": "string"},
                "auxiliary": {"type": "string"}
            },
            "required": ["record_id"],
            "additionalProperties": false
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "writes": [
            {"entityKind": FUEL_ITEM, "change": "created"},
            {"entityKind": FUEL_ITEM, "change": "modified"}
        ],
        "commands": [{
            "connector": "test",
            "operation": "submit",
            "doctype": "FuelItem",
            "name": "input.record_id",
            "fields": {}
        }],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "record_id",
            "objectKind": FUEL_ITEM,
            "entityArtifactId": "invariant.FuelItem.entity",
            "entityArtifactVersion": "1.0.0"
        }
    }))
    .expect("fuel action");
    let invocation = ActionCall {
        action: &action,
        action_artifact_id: "invariant.test.fuel_action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "test".to_owned()),
        ]),
        actor: "agent:test".to_owned(),
        input: object(json!({
            "record_id": "quality-action",
            "payload": "p".repeat(text_bytes),
            "auxiliary": "a".repeat(text_bytes)
        })),
        idempotency_key: "quality-action".to_owned(),
        object_type: Some(ObjectTypeName(FUEL_ITEM.to_owned())),
        turn: turn("turn-quality-action-fuel"),
    };
    commit_action(&mut cell, &catalog, &invocation, &allow_action())
        .expect("action admits quality data without quality fuel");
    assert!(
        cell.load(&FactKey::new(FUEL_ITEM, "quality-action"))
            .unwrap()
            .values
            .is_some()
    );
}

#[test]
fn observed_aggregate_shares_one_invariant_fuel_budget_and_commits_nothing_on_exhaustion() {
    const BATCH_SIZE: usize = 6;
    let fuel_limit = EvaluationBudget::default().units();
    let (object_type, values) = fuel_fixture_with_class(
        false,
        norn_ir::ConstraintClass::Quality,
        usize::try_from(EvaluationBudget::default().units() / 10)
            .expect("fuel fixture fits platform"),
    );
    let types = vec![object_type];
    let catalog = TypeCatalog::new(&types);
    let mut cell: ObservationCell =
        MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    let provenance = source_provenance();
    let keys: Vec<_> = (0..BATCH_SIZE)
        .map(|index| FactKey::new(FUEL_ITEM, format!("observed-{index}")))
        .collect();
    let mut writes = Vec::with_capacity(BATCH_SIZE);
    let mut events = Vec::with_capacity(BATCH_SIZE);
    let mut authorization = ObservationAuthorization::empty();

    for key in &keys {
        let slice = load_slice(&cell, [key.clone()]).expect("one-key slice");
        let prepared = prepare_observed_snapshot(
            &catalog,
            &slice,
            &turn("turn-observed-fuel"),
            ObservedSnapshot {
                key,
                values: &values,
                lifecycles: &[],
                provenance: &provenance,
            },
        )
        .expect("each object fits the per-commit invariant fuel budget");
        let (prepared, part_authorization) = prepared.into_parts();
        assert_eq!(prepared.writes.len(), 1);
        writes.extend(prepared.writes);
        events.extend(prepared.events);
        authorization.merge(part_authorization);
    }

    let aggregate: CellCommit<ObservationEvent, JsonObject, JsonObject> = CellCommit {
        writes,
        events,
        ..CellCommit::empty()
    };
    let error = cell.commit_observed(aggregate, authorization).unwrap_err();
    assert_eq!(error.code(), "commit.invariant_budget_exceeded");
    assert!(matches!(
        error,
        CommitError::InvariantBudgetExceeded { limit, .. } if limit == fuel_limit
    ));
    assert!(cell.ledger().is_empty());
    for key in keys {
        assert!(cell.load(&key).unwrap().values.is_none());
    }
    assert!(cell.outbox().is_empty());
    assert!(cell.timers().is_empty());
}

#[test]
fn observed_findings_count_toward_the_existing_event_budget() {
    let limit = norn_kernel::EnvelopeLimits::V1.max_events;
    assert_eq!(limit, 32);
    // Leave room for the aggregate modification and observation receipt so
    // preparation reaches the two invariant findings before exceeding the cap.
    let changed_fields = usize::try_from(limit).unwrap() - 2;
    let extra_fields = changed_fields - 2;
    let mut properties = vec![
        json!({"id": "item_id", "type": {"kind": "text"}}),
        json!({"id": "amount", "type": {"kind": "integer"}}),
    ];
    let mut before = json!({"item_id": "source-id", "amount": 1});
    let mut after = json!({"item_id": "", "amount": 0});
    for index in 0..extra_fields {
        let id = format!("extra_{index}");
        properties.push(json!({"id": id, "type": {"kind": "text"}}));
        before[&id] = json!(format!("before_{index}"));
        after[&id] = json!(format!("after_{index}"));
    }
    let object_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": ITEM,
        "properties": properties,
        "invariants": [
            {"id": "positive_amount", "expression": "self.amount > 0",
             "message": "amount must be positive"},
            {"id": "required_item_id", "expression": r#"self.item_id != """#,
             "message": "item id is required"}
        ]
    }))
    .unwrap();
    let types = vec![object_type];
    let catalog = TypeCatalog::new(&types);
    let key = FactKey::new(ITEM, "event-budget");
    let mut cell: CommandCell = MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: object(before),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    let prior = cell.load(&key).unwrap();
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let after = object(after);
    let provenance = source_provenance();
    let refusal = prepare_observed_snapshot(
        &catalog,
        &slice,
        &turn("turn-event-budget"),
        ObservedSnapshot {
            key: &key,
            values: &after,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        ObservationRefusal::EventLimit {
            limit: cap,
            actual
        } if cap == limit && actual == changed_fields + 4
    ));
    assert_eq!(cell.load(&key).unwrap(), prior);
    assert_eq!(cell.ledger().len(), 1);
    assert!(cell.outbox().is_empty());
    assert!(cell.timers().is_empty());
}

#[test]
fn observed_snapshot_commits_versioned_finding_with_deterministic_provenance() {
    let (types, interfaces) = definitions();
    let catalog = catalog(&types, &interfaces);
    let mut cell = observation_cell();
    let delivery = Observed {
        id: ObservationId("item-observation-42".to_owned()),
        provenance: delivery_provenance(),
        input: JsonObject::new(),
    };
    assert!(cell.inject(delivery.clone()));
    let consumed = delivery.consumed();
    let mut turn = turn("turn-observed");
    turn.consumes = Some(consumed.clone());
    let key = FactKey::new(ITEM, "observed-invalid");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let values = object(json!({"item_id": "", "amount": 9}));
    let provenance = source_provenance();
    let snapshot = ObservedSnapshot {
        key: &key,
        values: &values,
        lifecycles: &[],
        provenance: &provenance,
    };
    let prepared = prepare_observed_snapshot(&catalog, &slice, &turn, snapshot)
        .expect("source data is retained for quality review");
    let replay = prepare_observed_snapshot(&catalog, &slice, &turn, snapshot)
        .expect("same observed head prepares deterministically");
    assert_eq!(prepared, replay);

    commit_observed_snapshot(&mut cell, &catalog, &slice, &turn, snapshot)
        .expect("authorized observation is committed with its finding");
    assert_eq!(
        cell.observe().unwrap(),
        None,
        "the source delivery was consumed"
    );
    let entry = cell.ledger().last().expect("observation ledger entry");
    assert_eq!(entry.consumed, Some(consumed));
    assert_eq!(entry.written.len(), 1);
    assert!(cell.outbox().is_empty());
    assert!(cell.timers().is_empty());
    let Some(ObservationEvent::DataQualityFinding { fact }) = entry
        .events
        .iter()
        .find(|event| matches!(event, ObservationEvent::DataQualityFinding { .. }))
    else {
        panic!("observation emits its invariant finding");
    };
    assert_eq!(fact.schema_version, 2);
    assert_eq!(
        fact.subjects,
        vec![DataQualitySubject::new(
            ObjectTypeName(ITEM.to_owned()),
            "observed-invalid",
        )]
    );
    assert!(matches!(
        &fact.rule_ref,
        DataQualityRuleRef::Invariant {
            owner: InvariantOwner::Interface(owner),
            invariant_id,
            version,
            ..
        } if owner.as_str() == INTERFACE
            && invariant_id.as_str() == "named_item"
            && version.starts_with("sha256:")
    ));
    assert_eq!(fact.message, "item id is required");
    assert_eq!(fact.outcome, DataQualityOutcome::NotSatisfied);
    assert_eq!(
        fact.provenance,
        vec![DataQualityLineage::new(
            provenance.clone(),
            vec![turn.id.clone()],
        )]
    );
    assert_eq!(fact.provenance[0].provenance.upstream[0].version, 42);

    assert_eq!(
        catalog
            .classify_data_quality_rule(&fact.rule_ref)
            .expect("the emitted typed digest resolves"),
        norn_kernel::DataQualityRuleClassification::Integrity,
        "the kernel's explicitly stated default uses the same typed digest"
    );
    let mut changed_interfaces = interfaces.clone();
    changed_interfaces[0].invariants[0].classification = norn_ir::ConstraintClass::Quality;
    let changed_catalog = TypeCatalog::new(&types).with_interfaces(&changed_interfaces);
    assert_eq!(
        changed_catalog
            .classify_data_quality_rule(&fact.rule_ref)
            .expect("changed owner digest resolves as unverified"),
        norn_kernel::DataQualityRuleClassification::Unverified,
        "a classification change changes the typed owner digest"
    );
    let wrong_owner_kind = DataQualityRuleRef::Invariant {
        owner: InvariantOwner::Object(ObjectTypeName(INTERFACE.to_owned())),
        invariant_id: InvariantId("named_item".to_owned()),
        version: match &fact.rule_ref {
            DataQualityRuleRef::Invariant { version, .. } => version.clone(),
            DataQualityRuleRef::Builtin { .. } => unreachable!(),
        },
    };
    assert_eq!(
        catalog
            .classify_data_quality_rule(&wrong_owner_kind)
            .expect("wrong owner kind resolves as unverified"),
        norn_kernel::DataQualityRuleClassification::Unverified
    );
    assert_eq!(
        catalog
            .classify_data_quality_rule(&DataQualityRuleRef::Invariant {
                owner: InvariantOwner::Interface(norn_ir::InterfaceName(INTERFACE.to_owned())),
                invariant_id: InvariantId("other_invariant".to_owned()),
                version: match &fact.rule_ref {
                    DataQualityRuleRef::Invariant { version, .. } => version.clone(),
                    DataQualityRuleRef::Builtin { .. } => unreachable!(),
                },
            })
            .expect("wrong invariant id resolves as unverified"),
        norn_kernel::DataQualityRuleClassification::Unverified
    );
    assert_eq!(
        catalog
            .classify_data_quality_rule(&DataQualityRuleRef::Builtin {
                id: "norn.temporal.property_validity".to_owned(),
                version: "1".to_owned(),
            })
            .expect("supported built-in resolves"),
        norn_kernel::DataQualityRuleClassification::Builtin
    );
    assert_eq!(
        catalog
            .classify_data_quality_rule(&DataQualityRuleRef::Builtin {
                id: "norn.temporal.property_validity".to_owned(),
                version: "2".to_owned(),
            })
            .expect("unknown built-in version resolves as unverified"),
        norn_kernel::DataQualityRuleClassification::Unverified
    );
}

#[test]
fn observation_authorization_binds_its_write_and_does_not_exempt_other_writes() {
    for mutation in ["key", "version", "values", "extra_write", "provenance"] {
        let (types, interfaces) = definitions();
        let catalog = catalog(&types, &interfaces);
        let mut cell = observation_cell();
        let key = FactKey::new(ITEM, format!("tampered-{mutation}"));
        let slice = load_slice(&cell, [key.clone()]).unwrap();
        let values = object(json!({"item_id": "", "amount": 2}));
        let provenance = source_provenance();
        let prepared = prepare_observed_snapshot(
            &catalog,
            &slice,
            &turn("turn-tamper"),
            ObservedSnapshot {
                key: &key,
                values: &values,
                lifecycles: &[],
                provenance: &provenance,
            },
        )
        .expect("prepare invalid source values for quality review");
        let (mut commit, authorization) = prepared.into_parts();
        match mutation {
            "key" => commit
                .writes
                .first_mut()
                .expect("prepared write")
                .key
                .key
                .push_str("-altered"),
            "version" => {
                commit.writes.first_mut().expect("prepared write").expected = FactVersion(1);
            }
            "values" => {
                let write = commit.writes.first_mut().expect("prepared write");
                let FactMutation::Put { values } = &mut write.mutation else {
                    panic!("observed write is a put")
                };
                values.insert(
                    "item_id".to_owned(),
                    RuntimeValue::String("made-valid".to_owned()),
                );
            }
            "extra_write" => commit.writes.push(FactWrite {
                key: FactKey::new(ITEM, "unauthorized-extra"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({
                        "item_id": "unauthorized-extra", "amount": 0, "status": "draft"
                    })),
                },
            }),
            "provenance" => {
                let Some(ObservationEvent::ObservedFact { provenance, .. }) =
                    commit.events.first_mut()
                else {
                    panic!("prepared property write has a provenance event")
                };
                provenance.recorded_at = UtcTimestamp("2026-10-05T00:00:01Z".to_owned());
            }
            _ => unreachable!(),
        }
        let result = cell.commit_observed(commit, authorization);
        if mutation == "provenance" {
            result.expect("finding provenance comes from the opaque authorization");
            let entry = cell.ledger().last().expect("authorized observation entry");
            assert!(matches!(
                entry.events.iter().find(|event| matches!(event, ObservationEvent::DataQualityFinding { .. })),
                Some(ObservationEvent::DataQualityFinding { fact })
                    if fact.provenance[0].provenance == provenance
                        && fact.provenance[0].provenance.upstream[0].version == 42
            ));
            continue;
        }
        let error = result.unwrap_err();
        if mutation == "extra_write" {
            assert_eq!(error.code(), "commit.invariant_violation");
            assert!(matches!(
                error,
                CommitError::InvariantViolation {
                    key: ref refused,
                    owner: InvariantOwner::Object(ref owner),
                    ref id,
                    outcome,
                    ..
                } if refused.as_ref() == &FactKey::new(ITEM, "unauthorized-extra")
                    && owner.as_str() == ITEM
                    && id.as_str() == "positive_amount"
                    && outcome.as_ref() == &DataQualityOutcome::NotSatisfied
            ));
        } else {
            assert_eq!(
                error.code(),
                "commit.observation_authorization",
                "tampered {mutation}"
            );
        }
        assert!(cell.load(&key).unwrap().values.is_none());
        assert!(cell.ledger().is_empty());
        assert!(cell.outbox().is_empty());
        assert!(cell.timers().is_empty());
    }
}

#[test]
fn interface_diamond_retains_the_original_invariant_owner_at_commit() {
    let (mut types, mut interfaces) = definitions();
    for (name, parents) in [
        ("invariant.Left", vec![INTERFACE]),
        ("invariant.Right", vec![INTERFACE]),
        (
            "invariant.Diamond",
            vec!["invariant.Left", "invariant.Right"],
        ),
    ] {
        interfaces.push(serde_json::from_value(json!({"name":name,"extends":parents})).unwrap());
    }
    types[0].implements = vec![norn_ir::InterfaceName("invariant.Diamond".to_owned())];
    let catalog = catalog(&types, &interfaces);
    let mut cell = cell();
    let slice = load_slice(&cell, []).unwrap();
    let refusal = commit_decision(&mut cell, &catalog, &slice, &turn("diamond-invariant"),
        &output(&json!([{"op":"create","objectType":ITEM,"key":"empty","values":{"item_id":"","amount":5}}]), false))
        .unwrap_err();
    assert!(matches!(refusal, DecisionRefusal::InvariantViolation {
        owner: InvariantOwner::Interface(ref owner), ref id, ..
    } if owner.as_str() == INTERFACE && id.as_str() == "named_item"));
}

#[test]
fn concrete_implementers_of_child_interfaces_are_valid_parent_link_targets() {
    let source: ObjectTypeDef = serde_json::from_value(json!({"name":"invariant.Source","links":[{
        "id":"owner","target":"invariant.Parent","cardinality":{"to":"one","from":"many"}
    }]}))
    .unwrap();
    let target: ObjectTypeDef =
        serde_json::from_value(json!({"name":"invariant.Target","implements":["invariant.Child"]}))
            .unwrap();
    let interfaces: Vec<InterfaceDef> = serde_json::from_value(json!([
        {"name":"invariant.Parent"},{"name":"invariant.Child","extends":["invariant.Parent"]}
    ]))
    .unwrap();
    let mut cell = cell()
        .with_types(vec![source, target])
        .with_interfaces(interfaces);
    let source = FactKey::new("invariant.Source", "s");
    let target = FactKey::new("invariant.Target", "t");
    cell.commit(CellCommit {
        writes: vec![source.clone(), target.clone()]
            .into_iter()
            .map(|key| FactWrite {
                key,
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            })
            .collect(),
        ..CellCommit::empty()
    })
    .expect("objects commit");
    let key = LinkKey::new(source, "owner", target);
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("inherited parent membership admits link");
    assert!(cell.load_link(&key).unwrap().values.is_some());
}
