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

use norn_expr::{EvaluationBudget, TurnFuel};
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
    ActionCall, ActionRefusal, CellCommit, CellHost, CommitError, CommitView, Consumed,
    DecisionRefusal, DecisionTurn, FactEvent, FactExpectation, FactKey, FactMutation, FactVersion,
    FactWrite, LinkKey, LinkMutation, LinkWrite, LogicalTime, ObservationId, Observed, Provenance,
    TypeCatalog, World, commit_action, commit_decision, load_slice,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

use super::support::{AllowAction, Cell as CommandCell, new_cell};

const ITEM: &str = "invariant.Item";

#[test]
fn obs_evidence_unbound_law_pin_cannot_claim_verified_classification() {
    let catalog = TypeCatalog::new(&[]);
    let pin = DataQualityRuleRef::Law {
        artifact_id: "demo.purity.law".into(),
        law_id: "purity_in_range".into(),
        version: "sha256:definition-v1".into(),
    };
    assert_eq!(
        catalog
            .classify_data_quality_rule(&pin)
            .expect("classification"),
        norn_kernel::DataQualityRuleClassification::Unverified,
    );
}
const SUBTYPE: &str = "invariant.SpecialItem";
const INTERFACE: &str = "invariant.Named";
const FUEL_ITEM: &str = "invariant.FuelItem";
const NUMERIC_STRING_BASE: &str = "invariant.NumericStringBase";
const NUMERIC_STRING_ITEM: &str = "invariant.NumericStringItem";
const TEMPORAL_ITEM: &str = "invariant.TemporalItem";
const SEASON_ITEM: &str = "invariant.SeasonWindow";

fn link_domain_type(scale: u32, nested: bool, default: Option<Value>) -> ObjectTypeDef {
    let decimal = json!({"kind":"decimal", "scale":scale});
    let domain = if nested {
        json!({"kind":"record", "members":[{"id":"amount", "type":decimal}]})
    } else {
        decimal
    };
    let mut property = json!({"id":"reading", "type":domain});
    if let Some(default) = default {
        property["default"] = default;
    }
    serde_json::from_value(json!({"name":"demo.LinkDomain", "links":[{
        "id":"related", "target":"demo.LinkDomain",
        "cardinality":{"to":"many", "from":"many"}, "properties":[property]
    }]}))
    .unwrap()
}

#[test]
fn decimal_and_record_link_values_and_defaults_refuse_atomically() {
    for nested in [false, true] {
        for use_default in [false, true] {
            for (text, valid) in [("1.234", true), ("1.2345", false)] {
                let wrap = |number: Value| {
                    if nested {
                        json!({"amount":number})
                    } else {
                        number
                    }
                };
                let value = wrap(serde_json::from_str(text).unwrap());
                let definition =
                    link_domain_type(3, nested, use_default.then(|| wrap(json!(text))));
                let mut cell = new_cell().with_types(vec![definition]);
                let source = FactKey::new("demo.LinkDomain", "source");
                let target = FactKey::new("demo.LinkDomain", "target");
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
                let link = LinkKey::new(source.clone(), "related", target.clone());
                let side = FactKey::new("demo.LinkDomain", "side-effect");
                let before = (
                    cell.load(&source).unwrap(),
                    cell.load(&target).unwrap(),
                    cell.ledger().len(),
                );
                let values = if use_default {
                    JsonObject::new()
                } else {
                    object(json!({"reading":value}))
                };
                let result = cell.commit(CellCommit {
                    writes: vec![FactWrite {
                        key: side.clone(),
                        expected: FactVersion::ABSENT,
                        mutation: FactMutation::Put {
                            values: JsonObject::new(),
                        },
                    }],
                    links: vec![LinkWrite {
                        key: link.clone(),
                        expected: FactVersion::ABSENT,
                        mutation: LinkMutation::Add { values },
                    }],
                    ..CellCommit::empty()
                });
                if valid {
                    result.expect("representable link domain admits");
                    assert!(cell.load_link(&link).unwrap().exists());
                } else {
                    assert_eq!(
                        result.expect_err("excess scale refuses").code(),
                        "commit.effective_values"
                    );
                    assert!(!cell.load_link(&link).unwrap().exists());
                    assert!(!cell.load(&side).unwrap().exists());
                    assert_eq!(
                        before,
                        (
                            cell.load(&source).unwrap(),
                            cell.load(&target).unwrap(),
                            cell.ledger().len()
                        )
                    );
                    assert_eq!(cell.outbox(), []);
                }
            }
        }
    }
}

norn_testkit::proptest::proptest! {
    #[test]
    fn generated_link_decimal_domains_enforce_declared_scale(scale in 0_u32..9, mantissa in 1_i64..10000, nested in norn_testkit::proptest::bool::ANY) {
        let definitions = [link_domain_type(scale, nested, None)];
        let catalog = TypeCatalog::new(&definitions);
        let source = ObjectTypeName("demo.LinkDomain".into());
        let value = |scale: u32| {
            let places = usize::try_from(scale).unwrap();
            let digits = format!("{:0>width$}", mantissa * 10 + 1, width = places + 1);
            let (whole, fraction) = digits.split_at(digits.len() - places);
            let text = if scale == 0 { whole.to_owned() } else { format!("{whole}.{fraction}") };
            let number = RuntimeValue::Number(norn_ir::Decimal::parse(&text).unwrap());
            let reading = if nested { RuntimeValue::Object(BTreeMap::from([("amount".into(), number)])) } else { number };
            BTreeMap::from([("reading".into(), reading)])
        };
        norn_testkit::proptest::prop_assert!(catalog.effective_link_values(&source, "related", &value(scale)).is_ok());
        norn_testkit::proptest::prop_assert!(catalog.effective_link_values(&source, "related", &value(scale + 1)).is_err());
    }
}

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
        let mut cell = new_cell().with_types(vec![definition]);
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
    new_cell().with_types(types).with_interfaces(interfaces)
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
    assert_eq!(cell.outbox(), [], "a refused command reached the outbox");
    assert_eq!(cell.timers().len(), 0, "a refused timer was scheduled");
    let entry = cell.ledger().last().expect("refusal entry");
    assert_eq!(entry.written, [], "refusal entry wrote a business fact");
    assert_eq!(entry.linked, []);
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
        approved_intent: None,
        input: object(input),
        idempotency_key: idempotency_key.to_owned(),
        object_type: Some(ObjectTypeName(ITEM.to_owned())),
        turn: turn(turn_id),
        trace: None,
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
        approved_intent: None,
        input: object(input),
        idempotency_key: idempotency_key.to_owned(),
        object_type: Some(ObjectTypeName(NUMERIC_STRING_ITEM.to_owned())),
        turn: turn(turn_id),
        trace: None,
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
    let reason = norn_kernel::Refusable::refusal(&error)
        .reason
        .expect("invariant reason");
    let (types, interfaces) = definitions();
    let owner = InvariantOwner::Object(ObjectTypeName(ITEM.to_owned()));
    let expected_version = catalog(&types, &interfaces)
        .invariant_owner_version(&owner)
        .unwrap()
        .unwrap();
    assert_eq!(
        reason.pin,
        Some(DataQualityRuleRef::Invariant {
            owner,
            invariant_id: InvariantId("positive_amount".into()),
            version: expected_version,
        })
    );
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
    assert_eq!(cell.ledger(), []);
    assert_eq!(cell.outbox(), []);
    assert_eq!(cell.timers().len(), 0);
}

#[test]
fn temporal_properties_are_checked_on_raw_and_decision_writes_and_observations() {
    let types = vec![temporal_type()];
    let catalog = TypeCatalog::new(&types);
    let mut cell = new_cell().with_types(types.clone());

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
    assert_eq!(first_prepared.writes, [], "no fact version bump");
    assert_eq!(
        first_prepared.events,
        [],
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
    assert_eq!(
        second_prepared.writes,
        [],
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

fn seed_observed_cell(
    types: Vec<ObjectTypeDef>,
    entries: &[(&FactKey, &JsonObject)],
) -> ObservationCell {
    let mut observed: ObservationCell = MemoryCell::new(|_| JsonObject::new()).with_types(types);
    observed
        .commit(CellCommit {
            writes: entries
                .iter()
                .map(|(key, values)| FactWrite {
                    key: (*key).clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: (*values).clone(),
                    },
                })
                .collect(),
            ..CellCommit::empty()
        })
        .expect("fixture retains seeded observed state");
    observed
}

fn last_entry_findings(observed: &ObservationCell) -> Vec<&DataQualityFinding> {
    observed
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
        .collect()
}

fn prepare_observed_parts(
    catalog: &TypeCatalog<'_>,
    slice: &norn_kernel::FactSlice,
    key: &FactKey,
    values: &JsonObject,
    turn_id: &str,
    provenance: &InstanceProvenance,
) -> (
    CellCommit<ObservationEvent, JsonObject, JsonObject>,
    ObservationAuthorization,
) {
    prepare_observed_snapshot(
        catalog,
        slice,
        &turn(turn_id),
        ObservedSnapshot {
            key,
            values,
            lifecycles: &[],
            provenance,
        },
    )
    .expect("observed snapshot prepares")
    .into_parts()
}

#[test]
fn aggregated_read_findings_coalesce_only_exact_authorities() {
    let quality_type = quality_item_type();
    let catalog_types = vec![quality_type.clone()];
    let catalog = TypeCatalog::new(&catalog_types);
    let key = FactKey::new(ITEM, "aggregated-quality-read");
    let values = object(json!({"item_id": "aggregated-quality-read", "amount": 0}));
    let mut observed = seed_observed_cell(vec![quality_type], &[(&key, &values)]);
    let version = observed.load(&key).expect("seeded fact").version;
    let slice = load_slice(&observed, [key.clone()]).expect("one-key shared slice");

    let mut source_a = source_provenance();
    source_a.mapping_version = MappingVersion("map_a@1".to_owned());
    source_a.transform = Some("map_a.header".to_owned());
    let mut source_b = source_a.clone();
    source_b.mapping_version = MappingVersion("map_b@1".to_owned());
    source_b.transform = Some("map_b.header".to_owned());

    let (commit, mut authorization) = prepare_observed_parts(
        &catalog,
        &slice,
        &key,
        &values,
        "aggregate-map-a",
        &source_a,
    );
    let (_, duplicate_auth) = prepare_observed_parts(
        &catalog,
        &slice,
        &key,
        &values,
        "aggregate-map-a",
        &source_a,
    );
    let (_, distinct_auth) = prepare_observed_parts(
        &catalog,
        &slice,
        &key,
        &values,
        "aggregate-map-b",
        &source_b,
    );
    authorization.merge(duplicate_auth);
    authorization.merge(distinct_auth);
    authorization.retain_read_findings(&commit.reads, &commit.writes, &slice);
    assert!(authorization.validates(&commit));

    observed
        .commit_observed(commit, authorization)
        .expect("shared CAS read admits each distinct sealed proof once");
    assert_eq!(observed.load(&key).expect("fact remains").version, version);
    let findings = last_entry_findings(&observed);
    assert_eq!(findings.len(), 2, "exact duplicate was folded only once");
    let mapping_versions: BTreeSet<_> = findings
        .iter()
        .filter_map(|finding| finding.provenance.first())
        .map(|lineage| lineage.provenance.mapping_version.0.as_str())
        .collect();
    assert_eq!(mapping_versions, BTreeSet::from(["map_a@1", "map_b@1"]));
    for (mapping, expected_turn) in [
        ("map_a@1", "aggregate-map-a"),
        ("map_b@1", "aggregate-map-b"),
    ] {
        assert_eq!(
            findings
                .iter()
                .find(|finding| finding.provenance[0].provenance.mapping_version.0 == mapping)
                .expect("mapping finding")
                .provenance[0]
                .observation_ids,
            vec![expected_turn.to_owned()]
        );
    }
}

#[test]
fn final_write_retention_keeps_the_last_sealed_authority_once() {
    let types = vec![quality_item_type()];
    let catalog = TypeCatalog::new(&types);
    let key = FactKey::new(ITEM, "final-write-authority");
    let valid = object(json!({"item_id": "final-write-authority", "amount": 1}));
    let invalid = object(json!({"item_id": "final-write-authority", "amount": 0}));
    let mut observed = seed_observed_cell(types.clone(), &[(&key, &valid)]);
    let initial_slice = load_slice(&observed, [key.clone()]).expect("base slice");
    let invalid_slice = slice_with_values(&initial_slice, &key, &invalid);
    let valid_slice = slice_with_values(&invalid_slice, &key, &valid);
    let final_slice = slice_with_values(&valid_slice, &key, &invalid);

    let mut first_provenance = source_provenance();
    first_provenance.mapping_version = MappingVersion("map_b_first@1".to_owned());
    let (_, mut authorization) = prepare_observed_parts(
        &catalog,
        &initial_slice,
        &key,
        &invalid,
        "map-b-first",
        &first_provenance,
    );
    let (_, middle_auth) = prepare_observed_parts(
        &catalog,
        &invalid_slice,
        &key,
        &valid,
        "map-a-revert",
        &source_provenance(),
    );
    authorization.merge(middle_auth);

    let mut final_provenance = source_provenance();
    final_provenance.mapping_version = MappingVersion("map_b_final@1".to_owned());
    let (commit, final_auth) = prepare_observed_parts(
        &catalog,
        &valid_slice,
        &key,
        &invalid,
        "map-b-final",
        &final_provenance,
    );
    authorization.merge(final_auth);
    authorization.retain_writes(&commit.writes);
    authorization.retain_read_findings(&commit.reads, &commit.writes, &final_slice);

    assert_eq!(authorization.finding_count(), 1);
    assert!(authorization.validates(&commit));
    observed
        .commit_observed(commit, authorization)
        .expect("the aggregate write uses one exact final authority");
    let findings = last_entry_findings(&observed);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].provenance[0].provenance.mapping_version.0,
        "map_b_final@1"
    );
}

#[test]
fn final_read_retention_drops_findings_for_reverted_intermediate_values() {
    let types = vec![quality_item_type()];
    let catalog = TypeCatalog::new(&types);
    let key = FactKey::new(ITEM, "final-read-authority");
    let valid = object(json!({"item_id": "final-read-authority", "amount": 1}));
    let invalid = object(json!({"item_id": "final-read-authority", "amount": 0}));
    let mut observed = seed_observed_cell(types.clone(), &[(&key, &valid)]);
    let initial_slice = load_slice(&observed, [key.clone()]).expect("base slice");
    let invalid_slice = slice_with_values(&initial_slice, &key, &invalid);
    let provenance = source_provenance();

    let (_, mut authorization) = prepare_observed_parts(
        &catalog,
        &initial_slice,
        &key,
        &invalid,
        "read-map-b-first",
        &provenance,
    );
    let (unchanged_commit, read_auth) = prepare_observed_parts(
        &catalog,
        &invalid_slice,
        &key,
        &invalid,
        "read-map-b-noop",
        &provenance,
    );
    assert_eq!(unchanged_commit.writes, []);
    authorization.merge(read_auth);
    let (_, revert_auth) = prepare_observed_parts(
        &catalog,
        &invalid_slice,
        &key,
        &valid,
        "read-map-a-revert",
        &provenance,
    );
    authorization.merge(revert_auth);

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
fn guarded_read_states_share_commit_fuel_per_distinct_state() {
    const AUTHORITY_COUNT: usize = 6;
    let fuel_limit = EvaluationBudget::default().units();
    let text_bytes = usize::try_from(fuel_limit / 10).expect("fuel fixture fits platform");
    let (quality_type, values) =
        fuel_fixture_with_class(false, norn_ir::ConstraintClass::Quality, text_bytes);
    let types = vec![quality_type];
    let catalog = TypeCatalog::new(&types);

    for distinct_keys in [false, true] {
        let keys: Vec<_> = if distinct_keys {
            (0..AUTHORITY_COUNT)
                .map(|index| FactKey::new(FUEL_ITEM, format!("independent-read-{index}")))
                .collect()
        } else {
            vec![FactKey::new(FUEL_ITEM, "shared-quality-read")]
        };
        let entries: Vec<_> = keys.iter().map(|key| (key, &values)).collect();
        let mut observed = seed_observed_cell(types.clone(), &entries);
        let slice = load_slice(&observed, keys.clone()).expect("shared slice");
        let mut aggregate_commit = None;
        let mut authorization = ObservationAuthorization::empty();

        for index in 0..AUTHORITY_COUNT {
            let key = if distinct_keys {
                &keys[index]
            } else {
                &keys[0]
            };
            let mut provenance = source_provenance();
            provenance.mapping_version = MappingVersion(format!("map_{index}@1"));
            provenance.transform = Some(format!("map_{index}.payload"));
            let (part_commit, part_auth) = prepare_observed_parts(
                &catalog,
                &slice,
                key,
                &values,
                &format!("guarded-read-{index}"),
                &provenance,
            );
            assert_eq!(part_commit.writes, []);
            if aggregate_commit.is_none() {
                aggregate_commit = Some(part_commit);
            }
            authorization.merge(part_auth);
        }

        let commit = aggregate_commit.expect("at least one prepared authority");
        authorization.retain_read_findings(&commit.reads, &commit.writes, &slice);
        assert!(authorization.validates(&commit));

        if distinct_keys {
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
        } else {
            observed
                .commit_observed(commit, authorization)
                .expect("one guarded state is evaluated once for all source authorities");
            let findings = last_entry_findings(&observed);
            assert_eq!(findings.len(), AUTHORITY_COUNT);
            let mapping_versions: BTreeSet<_> = findings
                .iter()
                .filter_map(|finding| finding.provenance.first())
                .map(|lineage| lineage.provenance.mapping_version.0.as_str())
                .collect();
            assert_eq!(mapping_versions.len(), AUTHORITY_COUNT);
        }
    }
}

#[test]
fn read_only_findings_refuse_on_catalog_change_or_stale_cas() {
    for (case_id, mutate_catalog, expected_code) in [
        ("catalog-changed", true, "commit.observation_authorization"),
        ("stale-finding-cas", false, "commit.conflict"),
    ] {
        let quality_type = quality_item_type();
        let catalog_types = vec![quality_type.clone()];
        let catalog = TypeCatalog::new(&catalog_types);
        let key = FactKey::new(ITEM, case_id);
        let values = object(json!({"item_id": case_id, "amount": 0}));
        let mut cell = seed_observed_cell(Vec::new(), &[(&key, &values)]);
        if !mutate_catalog {
            cell = cell.with_types(vec![quality_type.clone()]);
        }

        let slice = load_slice(&cell, [key.clone()]).expect("one-key slice");
        let (commit, authorization) = prepare_observed_parts(
            &catalog,
            &slice,
            &key,
            &values,
            case_id,
            &source_provenance(),
        );
        assert_eq!(commit.writes, []);

        if mutate_catalog {
            let mut integrity_type = quality_type;
            integrity_type.invariants[0].classification = norn_ir::ConstraintClass::Integrity;
            cell = cell.with_types(vec![integrity_type]);
        } else {
            let current_version = cell.load(&key).unwrap().version;
            cell.commit(CellCommit {
                writes: vec![FactWrite {
                    key,
                    expected: current_version,
                    mutation: FactMutation::Put {
                        values: object(json!({"item_id": case_id, "amount": -1})),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect("quality violations remain admissible on commands");
        }

        let prior_entries = cell.ledger().len();
        let error = cell.commit_observed(commit, authorization).unwrap_err();
        assert_eq!(error.code(), expected_code, "{case_id}");
        assert_eq!(cell.ledger().len(), prior_entries, "{case_id}");
    }
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
    assert_eq!(cell.outbox(), []);
    assert_eq!(cell.timers().len(), 0);
    for (key, before) in accepted {
        assert_eq!(cell.load(&key).unwrap(), before);
    }
}

#[test]
fn omitted_nullable_default_is_evaluated_and_observed_value_is_retained() {
    let object_type = negative_default_type();
    let catalog = TypeCatalog::new(std::slice::from_ref(&object_type));
    let key = FactKey::new(ITEM, "negative-default");
    let mut command_cell = new_cell().with_types(vec![object_type.clone()]);
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
    assert_eq!(command_cell.ledger(), []);

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
fn numeric_and_referenced_defaults_resolve_canonically_or_fail_closed() {
    // 1. Direct numeric defaults materialize as exact runtime numbers and satisfy invariants.
    let direct_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.NumericDefaults",
        "properties": [
            { "id": "amount", "type": {"kind": "integer"}, "default": "1" },
            { "id": "rate", "type": {"kind": "decimal", "scale": 2}, "default": "12.50" }
        ],
        "invariants": [
            { "id": "positive_amount", "expression": "self.amount == 1", "message": "amount must equal one" },
            { "id": "exact_rate", "expression": "self.rate == 12.5", "message": "rate must equal twelve point five" }
        ]
    }))
    .expect("numeric defaults decode");
    let key = FactKey::new("test.NumericDefaults", "defaults");
    let mut cell = new_cell().with_types(vec![direct_type]);
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
    let expected_values = JsonObject::from([
        (
            "amount".to_owned(),
            RuntimeValue::Number(norn_ir::Decimal::parse("1").unwrap()),
        ),
        (
            "rate".to_owned(),
            RuntimeValue::Number(norn_ir::Decimal::parse("12.50").unwrap()),
        ),
    ]);
    assert_eq!(
        cell.load(&key).unwrap().values,
        Some(expected_values.clone())
    );

    // 2. Referenced property-type defaults resolve before invariant evaluation.
    let property_types: Vec<PropertyTypeDef> = [
        json!({"name": "shared.count", "type": {"kind": "integer"}}),
        json!({"name": "shared.rate", "type": {"kind": "decimal", "scale": 2}}),
    ]
    .into_iter()
    .map(|definition| serde_json::from_value(definition).expect("shared type decodes"))
    .collect();
    assert_eq!(
        norn_checker::check_property_type_defs(&property_types, &BTreeSet::new()),
        []
    );
    let referenced_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.ReferencedNumericDefaults",
        "properties": [
            { "id": "amount", "type": {"kind": "ref", "name": "shared.count"}, "default": "1" },
            { "id": "rate", "type": {"kind": "ref", "name": "shared.rate"}, "default": "12.50" }
        ],
        "invariants": [
            { "id": "positive_amount", "expression": "self.amount == 1", "message": "amount must equal one" },
            { "id": "exact_rate", "expression": "self.rate == 12.5", "message": "rate must equal twelve point five" }
        ]
    }))
    .expect("object type with referenced numeric defaults decodes");
    let type_name = ObjectTypeName("test.ReferencedNumericDefaults".to_owned());
    let catalog = TypeCatalog::new(std::slice::from_ref(&referenced_type))
        .with_property_types(&property_types);
    assert_eq!(
        catalog
            .effective_object_values(&type_name, &JsonObject::new())
            .expect("referenced defaults materialize"),
        expected_values
    );
    let mut remaining = TurnFuel::new(norn_kernel::invariant_evaluation_budget());
    assert_eq!(
        norn_kernel::evaluate_invariants(
            &catalog,
            &FactKey::new(type_name.as_str(), "referenced"),
            &JsonObject::new(),
            &mut remaining,
        )
        .expect("referenced defaults satisfy their invariants"),
        []
    );

    // 3. Unresolved and cyclic referenced defaults fail closed.
    let cyclic_props: Vec<PropertyTypeDef> = [
        json!({"name": "shared.first", "type": {"kind": "ref", "name": "shared.second"}}),
        json!({"name": "shared.second", "type": {"kind": "ref", "name": "shared.first"}}),
    ]
    .into_iter()
    .map(|definition| serde_json::from_value(definition).expect("cyclic type decodes"))
    .collect();
    for (name, ref_name, props) in [
        ("test.MissingReferenceDefault", "shared.missing", &[][..]),
        (
            "test.CyclicReferenceDefault",
            "shared.first",
            &cyclic_props[..],
        ),
    ] {
        let broken_type: ObjectTypeDef = serde_json::from_value(json!({
            "name": name,
            "properties": [{
                "id": "amount",
                "type": {"kind": "ref", "name": ref_name},
                "default": "1"
            }]
        }))
        .expect("broken reference fixture decodes");
        let broken_catalog =
            TypeCatalog::new(std::slice::from_ref(&broken_type)).with_property_types(props);
        assert!(
            broken_catalog
                .effective_object_values(&ObjectTypeName(name.to_owned()), &JsonObject::new())
                .is_err(),
            "{name}"
        );
    }
}

norn_testkit::proptest::proptest! {
    #![proptest_config(norn_testkit::proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn numeric_default_invariants_preserve_exact_decimal_values(
        amount in 0_u32..=10_000,
        whole in 0_u32..=10_000,
        frac in 0_u8..100,
    ) {
        let amount_str = amount.to_string();
        let rate_str = format!("{whole}.{frac:02}");
        let object_type: ObjectTypeDef = serde_json::from_value(json!({
            "name": "test.PropNumericDefaults",
            "properties": [
                { "id": "amount", "type": {"kind": "integer"}, "default": amount_str },
                { "id": "rate", "type": {"kind": "decimal", "scale": 2}, "default": rate_str }
            ],
            "invariants": [
                { "id": "exact_amount", "expression": format!("self.amount == {amount_str}"), "message": "amount mismatch" },
                { "id": "exact_rate", "expression": format!("self.rate == {rate_str}"), "message": "rate mismatch" }
            ]
        }))
        .expect("prop numeric defaults decode");
        let key = FactKey::new("test.PropNumericDefaults", "row");
        let mut cell = new_cell().with_types(vec![object_type]);
        let commit_res = cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        });
        norn_testkit::proptest::prop_assert!(
            commit_res.is_ok(),
            "expected numeric default commit to succeed, got {:?}",
            commit_res
        );
        let expected = JsonObject::from([
            (
                "amount".to_owned(),
                RuntimeValue::Number(norn_ir::Decimal::parse(&amount_str).unwrap()),
            ),
            (
                "rate".to_owned(),
                RuntimeValue::Number(norn_ir::Decimal::parse(&rate_str).unwrap()),
            ),
        ]);
        norn_testkit::proptest::prop_assert_eq!(cell.load(&key).unwrap().values, Some(expected));
    }
}

#[test]
fn missing_runtime_interface_metadata_fails_closed() {
    let (types, _) = definitions();
    let mut cell = new_cell().with_types(types);
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
    assert_eq!(cell.ledger(), []);
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
    let refusal = commit_action(&mut cell, &catalog, &invocation, &AllowAction).unwrap_err();
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
    let refusal = commit_action(&mut cell, &catalog, &invocation, &AllowAction).unwrap_err();
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
    let refusal = commit_action(&mut cell, &catalog, &invocation, &AllowAction).unwrap_err();
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
    let mut cell = new_cell().with_types(types.to_vec());
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
            &AllowAction,
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
        &AllowAction,
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
    assert_eq!(cell.outbox(), []);
    assert_eq!(cell.timers().len(), 0);
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
        approved_intent: None,
        input: object(json!({
            "record_id": "quality-action",
            "payload": "p".repeat(text_bytes),
            "auxiliary": "a".repeat(text_bytes)
        })),
        idempotency_key: "quality-action".to_owned(),
        object_type: Some(ObjectTypeName(FUEL_ITEM.to_owned())),
        turn: turn("turn-quality-action-fuel"),
        trace: None,
    };
    commit_action(&mut cell, &catalog, &invocation, &AllowAction)
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
    assert_eq!(cell.ledger(), []);
    for key in keys {
        assert!(cell.load(&key).unwrap().values.is_none());
    }
    assert_eq!(cell.outbox(), []);
    assert_eq!(cell.timers().len(), 0);
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
    assert_eq!(cell.outbox(), []);
    assert_eq!(cell.timers().len(), 0);
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
    assert_eq!(cell.outbox(), []);
    assert_eq!(cell.timers().len(), 0);
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
            DataQualityRuleRef::Builtin { .. } | DataQualityRuleRef::Law { .. } => unreachable!(),
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
                    DataQualityRuleRef::Builtin { .. } | DataQualityRuleRef::Law { .. } =>
                        unreachable!(),
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
        assert_eq!(cell.ledger(), []);
        assert_eq!(cell.outbox(), []);
        assert_eq!(cell.timers().len(), 0);
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

fn property_domain_fixture(
    scale: u64,
    minimum: &str,
) -> (ObjectTypeDef, InterfaceDef, Vec<PropertyTypeDef>) {
    let value_type = json!({"kind":"ref","name":"test.amount"});
    (
        serde_json::from_value(json!({"name":"test.Quantity","implements":["test.Bounded"],"properties":[
            {"id":"amount","type":value_type},
            {"id":"reading","type":{"kind":"record","members":[{"id":"amount","type":value_type}]}}
        ]})).unwrap(),
        serde_json::from_value(json!({"name":"test.Bounded","properties":[
            {"id":"amount","type":value_type,"constraints":{"max":"10"}}
        ]})).unwrap(),
        vec![serde_json::from_value(json!({"name":"test.amount","type":{"kind":"decimal","scale":scale},"constraints":{"min":minimum}})).unwrap()],
    )
}

norn_testkit::proptest::proptest! {
    #[test]
    fn property_rule_pin_catalog_permutations_are_equivalent(reverse_types: bool, reverse_extensions: bool) {
        let (definition, interface, mut property_types) = property_domain_fixture(3,"0");
        let mut extra = property_types[0].clone();
        extra.name = norn_ir::PropertyTypeName("test.other".into());
        property_types.push(extra);
        let mut extensions: Vec<norn_ir::ObjectTypeExtension> = [("alpha", "first"), ("alpha", "second"), ("beta", "third")].into_iter().map(|(namespace, member)| {
            serde_json::from_value(json!({"namespace":namespace,"target":"test.Quantity",
                "properties":[{"id":format!("{namespace}__{member}"),"type":{"kind":"decimal","scale":3}}]})).unwrap()
        }).collect();
        let rule = TypeCatalog::new([&definition]).with_interfaces([&interface]).with_property_types(&property_types)
            .with_extensions(&extensions).property_domain_rule_ref(&definition.name,&norn_ir::PropertyId("reading".into())).unwrap();
        if reverse_types { property_types.reverse(); }
        if reverse_extensions { extensions.reverse(); }
        let permuted = TypeCatalog::new([&definition]).with_interfaces([&interface]).with_property_types(&property_types)
            .with_extensions(&extensions).property_domain_rule_ref(&definition.name,&norn_ir::PropertyId("reading".into())).unwrap();
        norn_testkit::proptest::prop_assert_eq!(rule,permuted);
    }

    #[test]
    fn property_rule_pin_generated_declaration_changes_invalidate_references(scale in 0_u64..9, changed in 0_u64..9) {
        norn_testkit::proptest::prop_assume!(scale != changed);
        let (definition, interface, property_types) = property_domain_fixture(scale,"0");
        let catalog = TypeCatalog::new([&definition]).with_interfaces([&interface]).with_property_types(&property_types);
        let rule = catalog.property_domain_rule_ref(&definition.name,&norn_ir::PropertyId("reading".into())).unwrap().unwrap();
        norn_testkit::proptest::prop_assert_eq!(catalog.classify_data_quality_rule(&rule).unwrap(), norn_kernel::DataQualityRuleClassification::Builtin);
        let (_,_,changed_types) = property_domain_fixture(changed,"0");
        let changed_catalog = TypeCatalog::new([&definition]).with_interfaces([&interface]).with_property_types(&changed_types);
        norn_testkit::proptest::prop_assert_eq!(changed_catalog.classify_data_quality_rule(&rule).unwrap(), norn_kernel::DataQualityRuleClassification::Unverified);
        let (_,_,changed_bounds) = property_domain_fixture(scale,"1");
        let changed_catalog = TypeCatalog::new([&definition]).with_interfaces([&interface]).with_property_types(&changed_bounds);
        norn_testkit::proptest::prop_assert_eq!(changed_catalog.classify_data_quality_rule(&rule).unwrap(), norn_kernel::DataQualityRuleClassification::Unverified);
        let mut changed_interface = interface.clone();
        changed_interface.properties[0].constraints.max = Some(norn_ir::DecimalLiteral("9".into()));
        let changed_catalog = TypeCatalog::new([&definition]).with_interfaces([&changed_interface]).with_property_types(&property_types);
        norn_testkit::proptest::prop_assert_eq!(changed_catalog.classify_data_quality_rule(&rule).unwrap(), norn_kernel::DataQualityRuleClassification::Unverified);
        let forged = DataQualityRuleRef::Builtin {id:norn_kernel::decision_commit::PROPERTY_DOMAIN_RULE_ID.into(),version:"free-text-is-not-a-pin".into()};
        norn_testkit::proptest::prop_assert_eq!(catalog.classify_data_quality_rule(&forged).unwrap(), norn_kernel::DataQualityRuleClassification::Unverified);
    }
}

#[test]
fn property_rule_pin_empty_catalog_forms_are_equivalent() {
    let definition: ObjectTypeDef =
        serde_json::from_value(json!({"name":"test.Simple","properties":[
            {"id":"amount","type":{"kind":"decimal","scale":3}}
        ]}))
        .unwrap();
    let catalog = TypeCatalog::new([&definition]);
    let rule = catalog
        .property_domain_rule_ref(&definition.name, &norn_ir::PropertyId("amount".into()))
        .unwrap()
        .unwrap();
    assert_eq!(
        TypeCatalog::new([&definition])
            .with_property_types(&[])
            .classify_data_quality_rule(&rule)
            .unwrap(),
        norn_kernel::DataQualityRuleClassification::Builtin
    );
}

#[test]
fn property_rule_pin_observations_are_classifiable_and_native_writes_refuse() {
    let (definition, interface, property_types) = property_domain_fixture(3, "0");
    let catalog = TypeCatalog::new([&definition])
        .with_interfaces([&interface])
        .with_property_types(&property_types);
    let mut cell: ObservationCell = MemoryCell::new(|_| JsonObject::new())
        .with_types(vec![definition.clone()])
        .with_interfaces(vec![interface.clone()])
        .with_property_types(property_types.clone());
    let key = FactKey::new("test.Quantity", "observed");
    let values = object(json!({"amount":1.2345,"reading":{"amount":1.2345}}));
    assert!(matches!(
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: values.clone()
                }
            }],
            ..CellCommit::empty()
        }),
        Err(CommitError::InvariantViolation { .. })
    ));
    assert_eq!(cell.ledger(), []);
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let provenance = source_provenance();
    let snapshot = ObservedSnapshot {
        key: &key,
        values: &values,
        lifecycles: &[],
        provenance: &provenance,
    };
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("property-domain"),
        snapshot,
    )
    .unwrap();
    let findings: Vec<_> = cell
        .ledger()
        .last()
        .unwrap()
        .events
        .iter()
        .filter_map(|event| {
            if let ObservationEvent::DataQualityFinding { fact } = event {
                Some(fact)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(findings.len(), 2);
    for finding in findings {
        assert!(
            matches!(&finding.rule_ref, DataQualityRuleRef::Builtin {id,version} if id==norn_kernel::decision_commit::PROPERTY_DOMAIN_RULE_ID && version.starts_with("sha256:"))
        );
        assert_eq!(
            catalog
                .classify_data_quality_rule(&finding.rule_ref)
                .unwrap(),
            norn_kernel::DataQualityRuleClassification::Builtin
        );
    }
    assert!(cell.load(&key).unwrap().exists());
    assert_eq!(cell.outbox(), []);
}

#[test]
fn val_exact_invariants_admit_products_and_refuse_unqualified_division() {
    for (source, refused) in [
        ("self.amount * 0.1 > 0", false),
        ("self.amount / 3 > 0", true),
        ("self.amount * 0.000000000000000001 > 0", true),
    ] {
        let definition: ObjectTypeDef = serde_json::from_value(json!({
            "name": ITEM,
            "properties": [{"id":"amount", "type":{"kind":"decimal","scale":3}}],
            "invariants":[{"id":"exact_value", "expression":source, "message":"exact arithmetic required"}]
        })).expect("definition");
        let catalog = TypeCatalog::new([&definition]);
        let values = object(json!({"amount": "1.005"}));
        let mut remaining = TurnFuel::new(10000);
        let failures = norn_kernel::evaluate_invariants(
            &catalog,
            &FactKey::new(ITEM, "exact"),
            &values,
            &mut remaining,
        )
        .expect("evaluation");
        assert_eq!(!failures.is_empty(), refused, "{source}");
        assert!(remaining.remaining() < 10000);
    }
}

fn direct_amount_law() -> norn_ir::ArtifactEnvelope {
    let mut laws: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("laws");
    let law = &mut laws[0];
    law["artifactId"] = json!("invariant.amount.law");
    law["dependencies"][0]["artifactId"] = json!("invariant.Item.entity");
    law["payload"]["id"] = json!("amount_bounded");
    law["payload"]["subject"] = json!(ITEM);
    law["payload"]["scope"] = json!({"root":{"kind":"entity", "id":ITEM}, "objectTypes":[ITEM]});
    law["payload"]["response"] = json!({"kind":"predicate", "expression":"self.amount > 100"});
    serde_json::from_value(law.clone()).expect("direct law")
}

fn invariant_amount_law() -> norn_ir::ArtifactEnvelope {
    let mut law = serde_json::to_value(direct_amount_law()).expect("law");
    law["payload"]["modality"] = json!({"kind":"always"});
    law["payload"].as_object_mut().unwrap().remove("condition");
    law["payload"]["response"] = json!({
        "kind":"invariant", "objectType":ITEM, "invariantId":"positive_amount"
    });
    serde_json::from_value(law).expect("retained invariant law")
}

#[test]
fn law_invariant_runtime_valid_response_admits() {
    let laws = vec![invariant_amount_law()];
    let mut native = cell()
        .with_types(vec![quality_item_type()])
        .with_governing_laws(laws);
    let key = FactKey::new(ITEM, "invariant-law-valid");
    native
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({"item_id":"valid", "amount":1})),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("supported invariant law admits");
    assert!(native.load(&key).unwrap().values.is_some());
}

#[test]
fn law_invariant_runtime_violation_refuses_atomically_with_the_retained_law_pin() {
    let laws = vec![invariant_amount_law()];
    let mut retained = quality_item_type();
    retained.invariants[0].classification = norn_ir::ConstraintClass::Integrity;
    let types = vec![retained];
    let catalog = TypeCatalog::new(&types).with_governing_laws(&laws);
    let key = FactKey::new(ITEM, "invariant-law-invalid");
    let values = object(json!({"item_id":"invalid", "amount":0}));
    let failure =
        norn_kernel::governing_laws::evaluate(&catalog, &key, &values, &mut TurnFuel::default())
            .expect("law evaluates")
            .pop()
            .expect("violation");
    assert_eq!(failure.outcome, DataQualityOutcome::NotSatisfied);
    assert!(
        matches!(&failure.rule_ref, DataQualityRuleRef::Law {artifact_id, law_id, version}
        if artifact_id == "invariant.amount.law" && law_id == "amount_bounded" && version.starts_with("sha256:"))
    );
    assert_eq!(
        catalog
            .classify_data_quality_rule(&failure.rule_ref)
            .unwrap(),
        norn_kernel::DataQualityRuleClassification::Integrity
    );
    let mut native = cell().with_types(types).with_governing_laws(laws);
    let other = FactKey::new(ITEM, "also-refused");
    let refusal = native
        .commit(CellCommit {
            writes: vec![
                FactWrite {
                    key: key.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put { values },
                },
                FactWrite {
                    key: other.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: object(json!({"item_id":"other", "amount":1})),
                    },
                },
            ],
            ..CellCommit::empty()
        })
        .expect_err("retained law pin survives the integrity evaluator's refusal");
    assert!(
        matches!(refusal, CommitError::Law {reason: norn_kernel::governing_laws::LawEvaluationError::Violated(ref retained), ..}
        if retained.rule_ref == failure.rule_ref)
    );
    assert!(native.load(&key).unwrap().values.is_none());
    assert!(native.load(&other).unwrap().values.is_none());
    assert_eq!(native.ledger(), []);
    assert_eq!(native.outbox(), []);
}

#[test]
fn law_invariant_runtime_other_responses_still_refuse_before_activation() {
    for response in [
        json!({"kind":"action", "artifactId":"invariant.approve.action"}),
        json!({"kind":"transition", "objectType":ITEM, "property":"status", "transitionId":"approve"}),
    ] {
        let mut law = serde_json::to_value(invariant_amount_law()).unwrap();
        law["payload"]["response"] = response;
        law["payload"]["condition"] = json!("false");
        let mut native = cell()
            .with_types(vec![quality_item_type()])
            .with_governing_laws(vec![serde_json::from_value(law).unwrap()]);
        let refusal = native
            .commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(ITEM, "unsupported"),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: object(json!({"item_id":"valid", "amount":1})),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect_err("unsupported response cannot disappear behind false activation");
        assert_eq!(refusal.code(), "core.laws.unsupported_semantics");
        assert_eq!(native.ledger(), []);
    }
}

#[test]
fn law_invariant_runtime_misbound_missing_and_negated_references_refuse() {
    for (field, value, code) in [
        (
            "objectType",
            json!("invariant.Other"),
            "core.laws.unresolved_scope",
        ),
        (
            "invariantId",
            json!("missing"),
            "core.laws.unresolved_scope",
        ),
        (
            "modality",
            json!({"kind":"never"}),
            "core.laws.unsupported_semantics",
        ),
        (
            "condition",
            json!("false"),
            "core.laws.unsupported_semantics",
        ),
    ] {
        let mut law = serde_json::to_value(invariant_amount_law()).unwrap();
        if matches!(field, "modality" | "condition") {
            law["payload"][field] = value;
        } else {
            law["payload"]["response"][field] = value;
        }
        let laws = [serde_json::from_value(law).unwrap()];
        let types = [quality_item_type()];
        let catalog = TypeCatalog::new(&types).with_governing_laws(&laws);
        let error = norn_kernel::governing_laws::evaluate(
            &catalog,
            &FactKey::new(ITEM, "invalid-binding"),
            &object(json!({"item_id":"valid", "amount":1})),
            &mut TurnFuel::default(),
        )
        .expect_err("invalid retained reference refuses");
        assert_eq!(error.code(), code);
    }
}

#[test]
fn law_invariant_runtime_resolves_the_retained_subjects_interface_member() {
    let (mut types, mut interfaces) = definitions();
    types[0].invariants.clear();
    interfaces[0].invariants[0].classification = norn_ir::ConstraintClass::Quality;
    let mut law = serde_json::to_value(invariant_amount_law()).unwrap();
    law["payload"]["response"]["invariantId"] = json!("named_item");
    let mut native = cell()
        .with_types(types)
        .with_interfaces(interfaces)
        .with_governing_laws(vec![serde_json::from_value(law).unwrap()]);
    let result = native.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(ITEM, "interface-member"),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: object(json!({"item_id":"", "amount":1})),
            },
        }],
        ..CellCommit::empty()
    });
    assert!(matches!(
        result,
        Err(CommitError::Law {
            reason: norn_kernel::governing_laws::LawEvaluationError::Violated(_),
            ..
        })
    ));
    assert_eq!(native.ledger(), []);
}

#[test]
fn law_invariant_runtime_exhausted_budget_refuses() {
    let laws = [invariant_amount_law()];
    let types = [quality_item_type()];
    let catalog = TypeCatalog::new(&types).with_governing_laws(&laws);
    let mut budget = TurnFuel::new(0);
    assert_eq!(
        norn_kernel::governing_laws::evaluate(
            &catalog,
            &FactKey::new(ITEM, "no-fuel"),
            &object(json!({"item_id":"valid", "amount":1})),
            &mut budget
        ),
        Err(norn_kernel::governing_laws::LawEvaluationError::BudgetExceeded)
    );
    assert_eq!(budget.remaining(), 0);
}

#[test]
fn law_invariant_runtime_decision_and_action_refusals_journal_and_consume() {
    for through_action in [false, true] {
        let laws = vec![invariant_amount_law()];
        let (types, interfaces) = definitions();
        let catalog = catalog(&types, &interfaces).with_governing_laws(&laws);
        let mut native = cell().with_governing_laws(laws.clone());
        let delivery = Observed {
            id: ObservationId("invariant-law-delivery".into()),
            provenance: delivery_provenance(),
            input: JsonObject::new(),
        };
        assert!(native.inject(delivery.clone()));
        let consumed = delivery.consumed();
        let key = FactKey::new(ITEM, "invariant-law-journal");
        let attempt = "invariant-law-attempt";
        let expected_attempt;
        let refusal = if through_action {
            let action = action("upsert_input_object");
            expected_attempt = format!("{}/{attempt}", action.action_id);
            let mut call = action_call(
                &action,
                json!({"item_id":key.key, "amount":0, "status":"draft"}),
                attempt,
                attempt,
            );
            call.turn.consumes = Some(consumed.clone());
            match commit_action(&mut native, &catalog, &call, &AllowAction) {
                Err(ActionRefusal::Decision(refusal)) => refusal,
                other => panic!("expected a journaled action refusal: {other:?}"),
            }
        } else {
            expected_attempt = attempt.into();
            let mut turn = turn(attempt);
            turn.consumes = Some(consumed.clone());
            let slice = load_slice(&native, []).unwrap();
            commit_decision(
                &mut native,
                &catalog,
                &slice,
                &turn,
                &output(
                    &json!([{"op":"create", "objectType":ITEM, "key":key.key,
                    "values":{"item_id":key.key, "amount":0, "status":"draft"}}]),
                    true,
                ),
            )
            .expect_err("law refuses the decision")
        };
        assert!(matches!(refusal, DecisionRefusal::Law {
            reason: norn_kernel::governing_laws::LawEvaluationError::Violated(ref failure), ..
        } if matches!(&failure.rule_ref, DataQualityRuleRef::Law {law_id, ..} if law_id == "amount_bounded")
            && failure.invariant.as_ref().unwrap().version == catalog.invariant_owner_version(
                &InvariantOwner::Object(ObjectTypeName(ITEM.into()))).unwrap().unwrap()));
        assert_refusal_record(
            &native,
            0,
            &key,
            &InvariantOwner::Object(ObjectTypeName(ITEM.into())),
            "positive_amount",
            &expected_attempt,
        );
        assert_eq!(native.ledger().last().unwrap().consumed, Some(consumed));
        assert!(native.observe().unwrap().is_none());
        assert!(native.load(&key).unwrap().values.is_none());
    }
}

#[test]
fn law_invariant_runtime_missing_or_ambiguous_members_do_not_verify_a_pin() {
    let laws = [invariant_amount_law()];
    let mut types = [quality_item_type()];
    let catalog = TypeCatalog::new(&types).with_governing_laws(&laws);
    let pin = norn_kernel::governing_laws::evaluate(
        &catalog,
        &FactKey::new(ITEM, "pin"),
        &object(json!({"item_id":"pin", "amount":0})),
        &mut TurnFuel::default(),
    )
    .unwrap()
    .pop()
    .unwrap()
    .rule_ref;
    types[0].invariants.clear();
    assert_eq!(
        TypeCatalog::new(&types)
            .with_governing_laws(&laws)
            .classify_data_quality_rule(&pin)
            .unwrap(),
        norn_kernel::DataQualityRuleClassification::Unverified
    );
    types[0] = quality_item_type();
    types[0].implements = vec![norn_ir::InterfaceName(INTERFACE.into())];
    let interface: InterfaceDef = serde_json::from_value(json!({"name":INTERFACE,
        "invariants":[{"id":"positive_amount", "classification":"quality", "expression":"true", "message":"duplicate id"}]})).unwrap();
    assert_eq!(
        TypeCatalog::new(&types)
            .with_interfaces([&interface])
            .with_governing_laws(&laws)
            .classify_data_quality_rule(&pin)
            .unwrap(),
        norn_kernel::DataQualityRuleClassification::Unverified
    );
}

fn interface_invariant_law(scope: &[&str], invariant: &str) -> norn_ir::ArtifactEnvelope {
    let mut law = serde_json::to_value(invariant_amount_law()).expect("law");
    law["artifactId"] = json!("invariant.named.law");
    law["payload"]["id"] = json!("named_item_required");
    law["payload"]["subject"] = json!(INTERFACE);
    law["payload"]["scope"] =
        json!({"root":{"kind":"entity", "id":INTERFACE}, "objectTypes":scope});
    law["payload"]["response"] =
        json!({"kind":"invariant", "objectType":INTERFACE, "invariantId":invariant});
    serde_json::from_value(law).expect("interface law")
}

fn named_values(item_id: &str) -> JsonObject {
    object(json!({"item_id":item_id, "amount":1, "status":"draft"}))
}

#[test]
fn law_interface_transitive_and_diamond_implementors_govern_once_with_the_interface_pin() {
    let (mut types, mut interfaces) = definitions();
    interfaces.push(
        serde_json::from_value(json!({"name":"invariant.Labelled", "extends":[INTERFACE]}))
            .expect("diamond interface"),
    );
    types[0]
        .implements
        .push(norn_ir::InterfaceName("invariant.Labelled".into()));
    let laws = [interface_invariant_law(&[INTERFACE], "named_item")];
    let retained = catalog(&types, &interfaces).with_governing_laws(&laws);
    for ty in [ITEM, SUBTYPE] {
        let governed = retained.governing_laws(&ObjectTypeName(ty.into())).unwrap();
        assert_eq!(governed.len(), 1, "{ty}");
        let key = FactKey::new(ty, "unnamed");
        let mut budget = TurnFuel::default();
        let failure =
            norn_kernel::governing_laws::evaluate(&retained, &key, &named_values(""), &mut budget)
                .unwrap()
                .pop()
                .expect("violation");
        assert!(
            matches!(&failure.rule_ref, DataQualityRuleRef::Law { artifact_id, law_id, .. }
            if artifact_id == "invariant.named.law" && law_id == "named_item_required")
        );
        assert!(matches!(failure.invariant.as_deref(), Some(bound)
            if bound.failure.owner == InvariantOwner::Interface(norn_ir::InterfaceName(INTERFACE.into()))));
        assert_eq!(
            retained
                .classify_data_quality_rule(&failure.rule_ref)
                .unwrap(),
            norn_kernel::DataQualityRuleClassification::Integrity
        );
        let mut budget = TurnFuel::default();
        assert_eq!(
            norn_kernel::governing_laws::evaluate(
                &retained,
                &key,
                &named_values("named"),
                &mut budget
            )
            .unwrap(),
            []
        );
    }
    let mut native = cell()
        .with_types(types)
        .with_interfaces(interfaces)
        .with_governing_laws(laws.to_vec());
    let write = |ty: &str, item_id: &str| CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(ty, item_id),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: named_values(item_id),
            },
        }],
        ..CellCommit::empty()
    };
    native
        .commit(write(ITEM, "named"))
        .expect("implementor admits");
    let before = native.ledger().len();
    assert!(matches!(
        native.commit(write(SUBTYPE, "")),
        Err(CommitError::Law {
            reason: norn_kernel::governing_laws::LawEvaluationError::Violated(_),
            ..
        })
    ));
    assert_eq!(native.ledger().len(), before);
    assert!(
        native
            .load(&FactKey::new(SUBTYPE, ""))
            .unwrap()
            .values
            .is_none()
    );
}

norn_testkit::proptest::proptest! {
    #[test]
    fn law_interface_missing_ancestors_refuse_before_law_filtering(missing in "missing.[A-Z][a-z]{0,12}") {
        use norn_testkit::proptest::prelude::*;
        use norn_kernel::governing_laws::LawEvaluationError;
        let (types, mut interfaces) = definitions();
        interfaces[0].extends.push(norn_ir::InterfaceName(missing.clone()));
        let mut law = serde_json::to_value(interface_invariant_law(&[INTERFACE], "named_item")).unwrap();
        law["payload"]["subject"] = json!(missing);
        law["payload"]["scope"] = json!({"root":{"kind":"entity", "id":missing}, "objectTypes":[missing]});
        law["payload"]["response"]["objectType"] = json!(missing);
        let laws = [serde_json::from_value(law).unwrap()];
        let retained = catalog(&types, &interfaces).with_governing_laws(&laws);
        let key = FactKey::new(ITEM, "missing-interface");
        prop_assert!(matches!(retained.governing_laws(&key.object_type), Err(LawEvaluationError::Invalid(name)) if name == missing));
        let mut budget = TurnFuel::default();
        prop_assert!(matches!(norn_kernel::governing_laws::evaluate(&retained, &key, &named_values("named"), &mut budget), Err(LawEvaluationError::Invalid(name)) if name == missing));
    }
}

#[test]
fn law_interface_mixed_scopes_and_misowned_invariants_refuse() {
    use norn_kernel::governing_laws::LawEvaluationError;
    let (types, interfaces) = definitions();
    let item = ObjectTypeName(ITEM.into());
    let mixed = [interface_invariant_law(&[INTERFACE, ITEM], "named_item")];
    assert!(matches!(
        catalog(&types, &interfaces)
            .with_governing_laws(&mixed)
            .governing_laws(&item),
        Err(LawEvaluationError::UnsupportedUntilB2(_))
    ));
    let mut concrete =
        serde_json::to_value(interface_invariant_law(&[INTERFACE], "named_item")).unwrap();
    concrete["payload"]["response"]["objectType"] = json!(ITEM);
    for law in [
        interface_invariant_law(&[INTERFACE], "positive_amount"),
        interface_invariant_law(&[INTERFACE], "missing"),
        serde_json::from_value(concrete).unwrap(),
    ] {
        let laws = [law];
        let retained = catalog(&types, &interfaces).with_governing_laws(&laws);
        let mut budget = TurnFuel::default();
        assert!(matches!(
            norn_kernel::governing_laws::evaluate(
                &retained,
                &FactKey::new(ITEM, "misowned"),
                &named_values(""),
                &mut budget
            ),
            Err(LawEvaluationError::Invalid(_))
        ));
    }
}

norn_testkit::proptest::proptest! {
    #[test]
    fn law_interface_runtime_matches_the_shared_evaluator_for_implementors(item_id in "[a-z]{0,3}", subtype in norn_testkit::proptest::bool::ANY) {
        use norn_testkit::proptest::prelude::*;
        let (types, interfaces) = definitions();
        let laws = [interface_invariant_law(&[INTERFACE], "named_item")];
        let retained = catalog(&types, &interfaces).with_governing_laws(&laws);
        let key = FactKey::new(if subtype { SUBTYPE } else { ITEM }, "generated");
        let values = named_values(&item_id);
        let shared = norn_kernel::evaluate_invariants(&retained, &key, &values, &mut TurnFuel::default()).unwrap();
        let governed = norn_kernel::governing_laws::evaluate(&retained, &key, &values, &mut TurnFuel::default()).unwrap();
        prop_assert_eq!(governed.is_empty(), !item_id.is_empty());
        prop_assert_eq!(
            shared.into_iter().map(|failure| (failure.owner, failure.outcome)).collect::<Vec<_>>(),
            governed.into_iter().map(|failure| (failure.invariant.expect("bound").failure.owner, failure.outcome)).collect::<Vec<_>>()
        );
    }
}

norn_testkit::proptest::proptest! {
    #[test]
    fn law_invariant_runtime_matches_the_existing_evaluator(amount in -1000i64..1000) {
        use norn_testkit::proptest::prelude::*;
        let laws = [invariant_amount_law()];
        let types = [quality_item_type()];
        let catalog = TypeCatalog::new(&types).with_governing_laws(&laws);
        let key = FactKey::new(ITEM, "generated-invariant-law");
        let values = object(json!({"item_id":"generated", "amount":amount}));
        let mut shared_budget = TurnFuel::default();
        let shared = norn_kernel::evaluate_invariants(&catalog, &key, &values, &mut shared_budget).unwrap();
        let mut law_budget = TurnFuel::default();
        let governed = norn_kernel::governing_laws::evaluate(&catalog, &key, &values, &mut law_budget).unwrap();
        prop_assert_eq!(governed.is_empty(), amount > 0);
        prop_assert_eq!(shared.into_iter().map(|failure| failure.outcome).collect::<Vec<_>>(),
            governed.into_iter().map(|failure| failure.outcome).collect::<Vec<_>>());
        prop_assert_eq!(law_budget, shared_budget);
    }
}

norn_testkit::proptest::proptest! {
    #[test]
    fn law_commit_direct_positive_and_refusal_cases_are_atomic(amount in 1i64..200) {
        use norn_testkit::proptest::prelude::*;
        for amount in [100, 101, amount] {
            let laws = [direct_amount_law()];
            let (types, interfaces) = definitions();
            let catalog = catalog(&types, &interfaces).with_governing_laws(&laws);
            let key = FactKey::new(ITEM, "bounded");
            let values = object(json!({"item_id":"bounded", "amount":amount, "status":"draft"}));
            for ty in ["norn.ActionCall", "norn.OwnedRows", "norn.ObservedTombstone", "norn.ObservedSource", norn_kernel::naming_series::NAMING_SERIES_COUNTER_TYPE, "norn.PendingLinks"] {
                prop_assert!(catalog.governing_laws(&ObjectTypeName(ty.into())).expect("kernel-owned record").is_empty());
            }
            prop_assert!(catalog.governing_laws(&ObjectTypeName("unknown.Application".into())).is_err());
            let mut budget = TurnFuel::default();
            let failures = norn_kernel::governing_laws::evaluate(
                &catalog, &key, &values, &mut budget,
            ).expect("evaluate");
            prop_assert_eq!(failures.is_empty(), amount <= 100);
            prop_assert!(budget.remaining() < EvaluationBudget::default().units());
            let mut native = cell().with_governing_laws(laws.into());
            let result = native.commit(CellCommit {
                writes: vec![FactWrite {
                    key, expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put { values },
                }],
                ..CellCommit::empty()
            });
            prop_assert_eq!(result.is_ok(), amount <= 100);
            if let Err(refusal) = result {
                prop_assert_eq!(refusal.code(), "core.laws.violated");
                prop_assert!(native.ledger().is_empty());
            }
        }
    }
}

#[test]
fn obs_evidence_direct_law_violation_persists_its_governing_pin() {
    let laws = vec![direct_amount_law()];
    let (types, interfaces) = definitions();
    let catalog = catalog(&types, &interfaces).with_governing_laws(&laws);
    let key = FactKey::new(ITEM, "direct-law");
    let values = object(json!({"item_id":"direct-law", "amount":101}));
    let mut observed = observation_cell().with_governing_laws(laws.clone());
    let slice = load_slice(&observed, [key.clone()]).expect("slice");
    commit_observed_snapshot(
        &mut observed,
        &catalog,
        &slice,
        &turn("direct-law"),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &source_provenance(),
        },
    )
    .expect("capture");
    let findings: Vec<_> = observed
        .ledger()
        .iter()
        .flat_map(|entry| &entry.events)
        .filter_map(|event| match event {
            ObservationEvent::DataQualityFinding { fact } => Some(fact),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .collect();
    assert_eq!(findings.len(), 1);
    assert!(
        matches!(&findings[0].rule_ref, DataQualityRuleRef::Law {law_id, ..} if law_id == "amount_bounded")
    );
    assert_eq!(findings[0].outcome, DataQualityOutcome::NotSatisfied);
}

norn_testkit::proptest::proptest! {
    #[test]
    fn obs_evidence_redelivery_keeps_the_law_finding_identity(amount in 101i64..200, delivery in "[a-z]{1,12}") {
        use norn_testkit::proptest::prelude::*;
        let laws = [direct_amount_law()];
        let (types, interfaces) = definitions();
        let catalog = catalog(&types, &interfaces).with_governing_laws(&laws);
        let cell = observation_cell();
        let key = FactKey::new(ITEM, "law-replay");
        let slice = load_slice(&cell, [key.clone()]).expect("slice");
        let values = object(json!({"item_id":"law-replay", "amount":amount}));
        let provenance = source_provenance();
        let finding = |id: &str| {
            let (mut commit, authority) = prepare_observed_snapshot(&catalog, &slice, &turn(id), ObservedSnapshot {key:&key, values:&values, lifecycles:&[], provenance:&provenance}).expect("prepare").into_parts();
            authority.append_findings(&mut commit);
            commit.events.into_iter().find_map(|event| match event { ObservationEvent::DataQualityFinding {fact} => Some(fact), ObservationEvent::ObservedFact { .. } | ObservationEvent::ObservedTransition { .. } | ObservationEvent::ObservedDeleted { .. } | ObservationEvent::ObservedRecreated { .. } => None }).expect("law finding")
        };
        let first = finding("first-delivery");
        let again = finding(&delivery);
        prop_assert_eq!(&first.id, &again.id);
        prop_assert_eq!(&first.dedupe_key, &again.dedupe_key);
        prop_assert!(norn_kernel::snapshot_observe::verify_finding_values(&catalog, &key, &catalog.effective_object_values(&key.object_type, &values).expect("effective defaults"), &first, &mut norn_kernel::governing_laws::NoLinkTargets).expect("verify"));
        prop_assert_eq!(catalog.classify_data_quality_rule(&first.rule_ref).expect("classification"), norn_kernel::DataQualityRuleClassification::Integrity);
    }
}

#[test]
fn law_supported_temporal_semantics_refuse_even_when_activation_is_false() {
    let mut law = serde_json::to_value(direct_amount_law()).expect("law");
    law["payload"]["condition"] = json!("false");
    law["payload"]["modality"] = json!({"kind":"eventually"});
    let laws = [serde_json::from_value(law).expect("temporal law")];
    let (types, interfaces) = definitions();
    let catalog = catalog(&types, &interfaces).with_governing_laws(&laws);
    let key = FactKey::new(ITEM, "unsupported-law");
    let slice = load_slice(&observation_cell(), [key.clone()]).expect("slice");
    let refusal = prepare_observed_snapshot(
        &catalog,
        &slice,
        &turn("unsupported"),
        ObservedSnapshot {
            key: &key,
            values: &object(json!({"item_id":"unsupported-law", "amount":5})),
            lifecycles: &[],
            provenance: &source_provenance(),
        },
    )
    .expect_err("unsupported semantics");
    assert!(
        matches!(refusal, ObservationRefusal::Law(ref error) if error.code() == "core.laws.unsupported_semantics")
    );
}

#[test]
fn property_domain_unsupported_record_member_is_not_a_validity_finding() {
    let definition: ObjectTypeDef = serde_json::from_value(json!({
        "name":"test.UnsupportedRecord",
        "properties":[{"id":"reading","type":{"kind":"record","members":[
            {"id":"elapsed","type":{"kind":"duration"}}
        ]}}]
    }))
    .unwrap();
    let key = FactKey::new("test.UnsupportedRecord", "unsupported");
    let values = object(json!({"reading":{"elapsed":"PT1S"}}));
    let catalog = TypeCatalog::new([&definition]);
    assert!(matches!(
        norn_kernel::evaluate_invariants(&catalog, &key, &values, &mut TurnFuel::default()),
        Err(norn_kernel::InvariantEvaluationError::InvalidEffectiveValues(_))
    ));
    let mut native = new_cell().with_types(vec![definition]);
    let refusal = native
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put { values },
            }],
            ..CellCommit::empty()
        })
        .expect_err("unsupported members cannot become validity findings");
    let CommitError::EffectiveValues { reason, .. } = refusal else {
        panic!("expected unsupported effective values");
    };
    assert!(reason.contains("unsupported"));
    assert!(reason.contains("duration"));
    assert!(native.load(&key).unwrap().values.is_none());
    assert_eq!(native.ledger(), []);
    assert_eq!(native.outbox(), []);
}

fn law_exception(id: &str, overrides: &str, condition: &str) -> Value {
    json!({
        "id": id,
        "overrides": overrides,
        "condition": condition,
        "rationale": format!("{id} exception"),
        "source": {"artifactId": "invariant.amount.law", "pointer": "/payload"}
    })
}

#[test]
fn law_with_exceptions_folds_nested_overrides_and_is_order_independent() {
    let base_exceptions = vec![
        law_exception("vip", "amount_bounded", "self.item_id == 'vip'"),
        law_exception("too_big", "vip", "self.amount > 1000"),
        law_exception("board_cap", "too_big", "self.amount == 2000"),
        law_exception("exact_150", "amount_bounded", "self.amount == 150"),
    ];
    for reversed in [false, true] {
        let mut exceptions = base_exceptions.clone();
        if reversed {
            exceptions.reverse();
        }
        let mut law = serde_json::to_value(direct_amount_law()).unwrap();
        law["payload"]["exceptions"] = Value::Array(exceptions);
        let artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(law).unwrap();
        let types = vec![quality_item_type()];
        let catalog = TypeCatalog::new(&types).with_governing_laws(std::slice::from_ref(&artifact));
        let mut native = cell()
            .with_types(types.clone())
            .with_governing_laws(vec![artifact.clone()]);
        let commit_row = |cell: &mut CommandCell, key: &str, item_id: &str, amount: i64| {
            cell.commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(ITEM, key),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: object(json!({"item_id": item_id, "amount": amount})),
                    },
                }],
                ..CellCommit::empty()
            })
        };

        // Base law holds without needing any exception.
        commit_row(&mut native, "base-ok", "regular", 50).expect("base law holds");
        // Sibling exception lifts the violation.
        commit_row(&mut native, "sibling-lifted", "regular", 150).expect("sibling lifts");
        // First-level exception lifts the violation.
        commit_row(&mut native, "vip-lifted", "vip", 500).expect("vip lifts");
        // Third-level exception overrides `too_big`, restoring `vip`'s lift.
        commit_row(&mut native, "board-relifted", "vip", 2000).expect("board cap re-lifts");

        // Unexcepted violation refuses with DIAG-REASON-1 operands including exception reads.
        let regular_refusal = commit_row(&mut native, "regular-bad", "regular", 500)
            .expect_err("unexcepted write must refuse");
        assert_eq!(regular_refusal.code(), "core.laws.violated");
        let CommitError::Law {
            reason: norn_kernel::governing_laws::LawEvaluationError::Violated(failure),
            ..
        } = regular_refusal
        else {
            panic!("expected law violation");
        };
        assert_eq!(failure.outcome, DataQualityOutcome::NotSatisfied);
        assert_eq!(
            failure.values.get("item_id"),
            Some(&RuntimeValue::String("regular".into()))
        );
        assert_eq!(
            failure.values.get("amount"),
            Some(&RuntimeValue::Number(
                norn_ir::Decimal::parse("500").unwrap()
            ))
        );
        assert_eq!(
            catalog
                .classify_data_quality_rule(&failure.rule_ref)
                .unwrap(),
            norn_kernel::DataQualityRuleClassification::Integrity
        );

        // Second-level exception overrides `vip`, re-applying the base law.
        let override_refusal = commit_row(&mut native, "vip-overridden", "vip", 1500)
            .expect_err("nested override must re-apply the law");
        assert_eq!(override_refusal.code(), "core.laws.violated");
        assert!(
            native
                .load(&FactKey::new(ITEM, "vip-overridden"))
                .unwrap()
                .values
                .is_none()
        );
    }

    // `Always` modality with `when` guard + `unless` exception.
    let mut always = serde_json::to_value(direct_amount_law()).unwrap();
    always["payload"]["modality"] = json!({"kind": "always"});
    always["payload"]["condition"] = json!("self.amount >= 0");
    always["payload"]["response"]["expression"] = json!("self.amount <= 100");
    always["payload"]["exceptions"] = json!([law_exception(
        "vip",
        "amount_bounded",
        "self.item_id == 'vip'"
    )]);
    let mut native = cell()
        .with_types(vec![quality_item_type()])
        .with_governing_laws(vec![serde_json::from_value(always).unwrap()]);
    for (key, item_id, amount, ok) in [
        ("inapplicable", "regular", -5, true),
        ("lifted", "vip", 500, true),
        ("violated", "regular", 500, false),
    ] {
        let res = native.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(ITEM, key),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({"item_id": item_id, "amount": amount})),
                },
            }],
            ..CellCommit::empty()
        });
        assert_eq!(res.is_ok(), ok, "key={key} res={res:?}");
    }
}

#[test]
fn law_with_exceptions_fails_closed_on_malformed_tree_broken_expr_invariant_and_dataset_rule() {
    // Malformed exception tree (cycle) refuses even when `when` is false, and never classifies as Integrity.
    let mut cyclic = serde_json::to_value(direct_amount_law()).unwrap();
    cyclic["payload"]["condition"] = json!("false");
    cyclic["payload"]["exceptions"] = json!([
        law_exception("a", "b", "true"),
        law_exception("b", "a", "true")
    ]);
    let cyclic_payload = cyclic["payload"].clone();
    let cyclic_artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(cyclic).unwrap();
    let types = vec![quality_item_type()];
    let cyclic_catalog =
        TypeCatalog::new(&types).with_governing_laws(std::slice::from_ref(&cyclic_artifact));
    let mut cyclic_cell = cell()
        .with_types(types.clone())
        .with_governing_laws(vec![cyclic_artifact.clone()]);
    let cyclic_err = cyclic_cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(ITEM, "cyc"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({"item_id": "cyc", "amount": 5})),
                },
            }],
            ..CellCommit::empty()
        })
        .expect_err("malformed exception tree must refuse even when condition is false");
    assert_eq!(cyclic_err.code(), "core.laws.unresolved_scope");
    let cyclic_pin = DataQualityRuleRef::Law {
        artifact_id: cyclic_artifact.artifact_id.clone(),
        law_id: "amount_bounded".into(),
        version: {
            let decoded: norn_ir::LawIR = serde_json::from_value(cyclic_payload).unwrap();
            let bytes = norn_ir::canonical::canonical_json_of(&decoded).unwrap();
            format!(
                "sha256:{}",
                norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
            )
        },
    };
    assert_eq!(
        cyclic_catalog
            .classify_data_quality_rule(&cyclic_pin)
            .unwrap(),
        norn_kernel::DataQualityRuleClassification::Unverified
    );

    // Non-boolean and failing exception conditions fail closed (never lift or ignore),
    // including a broken child exception under an unmatched parent.
    for (exceptions, is_non_bool) in [
        (
            vec![law_exception("bad", "amount_bounded", "self.amount")],
            true,
        ),
        (
            vec![law_exception("bad", "amount_bounded", "upper(5) == '5'")],
            false,
        ),
        (
            vec![
                law_exception("parent", "amount_bounded", "self.item_id == 'vip'"),
                law_exception("broken_child", "parent", "upper(5) == '5'"),
            ],
            false,
        ),
    ] {
        let mut broken = serde_json::to_value(direct_amount_law()).unwrap();
        broken["payload"]["exceptions"] = Value::Array(exceptions);
        let mut broken_cell = cell()
            .with_types(types.clone())
            .with_governing_laws(vec![serde_json::from_value(broken).unwrap()]);
        let err = broken_cell
            .commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(ITEM, "k"),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: object(json!({"item_id": "k", "amount": 5})),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect_err("broken exception condition must refuse");
        assert_eq!(err.code(), "core.laws.violated");
        let CommitError::Law {
            reason: norn_kernel::governing_laws::LawEvaluationError::Violated(failure),
            ..
        } = err
        else {
            panic!("expected Violated");
        };
        if is_non_bool {
            assert!(
                matches!(failure.outcome, DataQualityOutcome::NonBoolean { .. }),
                "{:?}",
                failure.outcome
            );
        } else {
            assert!(
                matches!(failure.outcome, DataQualityOutcome::EvaluationFailed { .. }),
                "{:?}",
                failure.outcome
            );
        }
    }

    // Invariant response with exceptions still refuses as unsupported semantics.
    let mut inv = serde_json::to_value(invariant_amount_law()).unwrap();
    inv["payload"]["exceptions"] = json!([law_exception("small", "amount_bounded", "true")]);
    let inv_artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(inv).unwrap();
    let mut inv_cell = cell()
        .with_types(types.clone())
        .with_governing_laws(vec![inv_artifact.clone()]);
    let inv_err = inv_cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(ITEM, "inv"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: object(json!({"item_id": "inv", "amount": 5})),
                },
            }],
            ..CellCommit::empty()
        })
        .expect_err("invariant law with exceptions is unsupported");
    assert_eq!(inv_err.code(), "core.laws.unsupported_semantics");

    // Bulk/dataset `law_rule` accepts valid non-link predicate laws with exceptions,
    // refuses cyclic trees (`core.laws.unresolved_scope`), link-reading exceptions and
    // `Invariant` laws with exceptions (`core.laws.unsupported_semantics`), and zero fuel refuses.
    let mut valid = serde_json::to_value(direct_amount_law()).unwrap();
    valid["payload"]["exceptions"] = json!([law_exception(
        "vip",
        "amount_bounded",
        "self.item_id == 'vip'"
    )]);
    let valid_artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(valid).unwrap();
    let valid_catalog =
        TypeCatalog::new(&types).with_governing_laws(std::slice::from_ref(&valid_artifact));
    let (dataset_law, _) = norn_kernel::governing_laws::law_rule(
        &valid_catalog,
        &ObjectTypeName(ITEM.into()),
        &valid_artifact,
    )
    .expect("dataset law_rule accepts valid non-link predicate laws with exceptions");
    assert_eq!(dataset_law.exceptions.len(), 1);

    let cyclic_rule_err = norn_kernel::governing_laws::law_rule(
        &cyclic_catalog,
        &ObjectTypeName(ITEM.into()),
        &cyclic_artifact,
    )
    .expect_err("dataset law_rule must refuse cyclic exception trees");
    assert_eq!(cyclic_rule_err.code(), "core.laws.unresolved_scope");

    let inv_catalog =
        TypeCatalog::new(&types).with_governing_laws(std::slice::from_ref(&inv_artifact));
    let inv_rule_err = norn_kernel::governing_laws::law_rule(
        &inv_catalog,
        &ObjectTypeName(ITEM.into()),
        &inv_artifact,
    )
    .expect_err("dataset law_rule must refuse invariant laws with exceptions");
    assert_eq!(inv_rule_err.code(), "core.laws.unsupported_semantics");

    let doc_types = document_types().expect("document types");
    let mut linked_exc = serde_json::to_value(
        link_law("line_cap_dataset", SPECIAL_LINE, "self.amount > 100").expect("link law"),
    )
    .unwrap();
    linked_exc["payload"]["exceptions"] = json!([law_exception(
        "doc_ex",
        "line_cap_dataset",
        "self.doc.limit >= 1000"
    )]);
    let linked_exc_artifact: norn_ir::ArtifactEnvelope =
        serde_json::from_value(linked_exc).unwrap();
    let linked_catalog = TypeCatalog::new(&doc_types)
        .with_governing_laws(std::slice::from_ref(&linked_exc_artifact));
    let linked_rule_err = norn_kernel::governing_laws::law_rule(
        &linked_catalog,
        &ObjectTypeName(SPECIAL_LINE.into()),
        &linked_exc_artifact,
    )
    .expect_err("dataset law_rule must refuse link-reading exception laws");
    assert_eq!(linked_rule_err.code(), "core.laws.unsupported_semantics");

    let fuel_err = norn_kernel::governing_laws::evaluate(
        &valid_catalog,
        &FactKey::new(ITEM, "fuel"),
        &object(json!({"item_id": "vip", "amount": 500})),
        &mut TurnFuel::new(0),
    )
    .expect_err("zero fuel must fail closed");
    assert_eq!(
        fuel_err,
        norn_kernel::governing_laws::LawEvaluationError::BudgetExceeded
    );
}

struct EmptyCommitView;

impl CommitView for EmptyCommitView {
    type Error = std::convert::Infallible;
    fn version(&self, _: &FactKey) -> Result<FactVersion, Self::Error> {
        Ok(FactVersion::ABSENT)
    }
    fn concrete(&self, _: &FactKey) -> Result<Option<ObjectTypeName>, Self::Error> {
        Ok(None)
    }
    fn link_version(&self, _: &LinkKey) -> Result<FactVersion, Self::Error> {
        Ok(FactVersion::ABSENT)
    }
    fn links_of(&self, _: &FactKey) -> Result<Vec<LinkKey>, Self::Error> {
        Ok(Vec::new())
    }
    fn object_values(&self, _: &FactKey) -> Result<Option<JsonObject>, Self::Error> {
        Ok(None)
    }
    fn pending(&self, _: &Consumed) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

#[test]
fn law_exception_audit_attribution_records_applied_exception_ids() {
    let types = vec![quality_item_type()];
    let mut nested = serde_json::to_value(direct_amount_law()).unwrap();
    nested["payload"]["exceptions"] = json!([
        law_exception("vip", "amount_bounded", "self.item_id == 'vip'"),
        law_exception("board_override", "amount_bounded", "self.amount >= 400"),
        law_exception("huge_cap", "vip", "self.amount > 1000"),
    ]);
    let nested_artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(nested).unwrap();
    let nested_catalog =
        TypeCatalog::new(&types).with_governing_laws(std::slice::from_ref(&nested_artifact));
    let lifted = norn_kernel::governing_laws::evaluate_with_audit(
        &nested_catalog,
        &FactKey::new(ITEM, "vip"),
        &object(json!({"item_id": "vip", "amount": 500})),
        &mut TurnFuel::default(),
    )
    .expect("both sibling exceptions lift the law");
    assert_eq!(lifted.failures, Vec::new());
    assert_eq!(lifted.applied_exceptions.len(), 1);
    assert_eq!(
        lifted.applied_exceptions[0].applied_exception_ids,
        vec!["board_override".to_owned(), "vip".to_owned()]
    );
    assert!(matches!(
        &lifted.applied_exceptions[0].rule_ref,
        DataQualityRuleRef::Law { law_id, .. } if law_id == "amount_bounded"
    ));

    let re_engaged = norn_kernel::governing_laws::evaluate_with_audit(
        &nested_catalog,
        &FactKey::new(ITEM, "vip"),
        &object(json!({"item_id": "vip", "amount": 2000})),
        &mut TurnFuel::default(),
    )
    .expect("huge_cap defeats vip while board_override still lifts");
    assert_eq!(re_engaged.failures, Vec::new());
    assert_eq!(
        re_engaged.applied_exceptions[0].applied_exception_ids,
        vec!["board_override".to_owned(), "huge_cap".to_owned()]
    );

    let mut defeated = serde_json::to_value(direct_amount_law()).unwrap();
    defeated["payload"]["exceptions"] = json!([
        law_exception("vip", "amount_bounded", "self.item_id == 'vip'"),
        law_exception("huge_cap", "vip", "self.amount > 1000"),
    ]);
    let defeated_artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(defeated).unwrap();
    let defeated_catalog =
        TypeCatalog::new(&types).with_governing_laws(std::slice::from_ref(&defeated_artifact));
    let defeated_report = norn_kernel::governing_laws::evaluate_with_audit(
        &defeated_catalog,
        &FactKey::new(ITEM, "vip"),
        &object(json!({"item_id": "vip", "amount": 2000})),
        &mut TurnFuel::default(),
    )
    .expect("huge_cap defeats sole vip exception so law re-engages");
    assert_eq!(defeated_report.failures.len(), 1);
    assert_eq!(defeated_report.applied_exceptions, Vec::new());

    let not_lifted = norn_kernel::governing_laws::evaluate_with_audit(
        &nested_catalog,
        &FactKey::new(ITEM, "std"),
        &object(json!({"item_id": "std", "amount": 200})),
        &mut TurnFuel::default(),
    )
    .expect("no exception matches");
    assert_eq!(not_lifted.applied_exceptions, Vec::new());
    assert_eq!(not_lifted.failures.len(), 1);

    // Real CellCommit through check_commit_with_audit preserves the same §8.1 attribution.
    let model = norn_kernel::CommitModel::new(&nested_catalog);
    let commit = CellCommit::<FactEvent, JsonObject, JsonObject> {
        writes: vec![FactWrite {
            key: FactKey::new(ITEM, "vip"),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: object(json!({"item_id": "vip", "amount": 500})),
            },
        }],
        ..CellCommit::empty()
    };
    let audit = norn_kernel::check_commit_with_audit(&commit, &model, &EmptyCommitView)
        .expect("excepted commit admits with audit attribution");
    assert_eq!(audit.applied_exceptions.len(), 1);
    assert_eq!(
        audit.applied_exceptions[0].subject,
        FactKey::new(ITEM, "vip")
    );
    assert_eq!(
        audit.applied_exceptions[0].applied_exceptions[0].applied_exception_ids,
        vec!["board_override".to_owned(), "vip".to_owned()]
    );
}

#[test]
fn law_exception_reading_links_rechecks_on_target_mutation_and_supports_observations() -> Checked {
    let types = document_types()?;
    let mut law = serde_json::to_value(link_law(
        "line_within_default_cap",
        SPECIAL_LINE,
        "self.amount > 100",
    )?)?;
    law["payload"]["condition"] = json!("self.doc != null");
    law["payload"]["exceptions"] = json!([{
        "id": "high_limit_doc",
        "overrides": "line_within_default_cap",
        "condition": "self.doc.limit >= 1000",
        "rationale": "High-limit documents allow larger lines.",
        "source": {"artifactId": "line_within_default_cap.law", "pointer": "/payload"}
    }]);
    let artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(law)?;
    let mut cell = new_cell()
        .with_types(types)
        .with_governing_laws(vec![artifact]);
    let doc = FactKey::new(DOC, "d");
    let line = FactKey::new(SPECIAL_LINE, "l");

    // Unlinked line: `when: self.doc != null` is false, so exception link read is skipped.
    cell.commit(CellCommit {
        writes: vec![put_new(SPECIAL_LINE, "unlinked", json!({"amount": 500}))?],
        ..CellCommit::empty()
    })?;

    // Linked line: `doc.limit == 1000` satisfies `high_limit_doc` exception, lifting the cap.
    cell.commit(CellCommit {
        writes: vec![
            put_new(DOC, "d", json!({"limit": 1000}))?,
            put_new(SPECIAL_LINE, "l", json!({"amount": 500}))?,
        ],
        links: vec![add_link(&line, "doc", &doc)],
        ..CellCommit::empty()
    })?;

    // Lowering `doc.limit` revokes the exception and refuses the target mutation with link + operand evidence.
    let lowered = cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: doc,
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: object(json!({"limit": 50})),
            },
        }],
        ..CellCommit::empty()
    });
    let Err(CommitError::Law {
        reason: norn_kernel::governing_laws::LawEvaluationError::Violated(failure),
        ..
    }) = lowered
    else {
        panic!("mutating a target that revokes a linked exception must refuse");
    };
    let pin = failure.links.get("doc").expect("pinned target version");
    assert_eq!(
        (pin.object_type.as_str(), pin.key.as_str(), pin.version),
        (DOC, "d", 2)
    );
    assert_eq!(
        failure.values.get("doc.limit"),
        Some(&RuntimeValue::Number(norn_ir::Decimal::parse("50")?))
    );

    // Observed snapshot path: lifted write records zero findings; unexcepted write records a finding.
    let mut obs_law = serde_json::to_value(direct_amount_law())?;
    obs_law["payload"]["exceptions"] = json!([law_exception(
        "vip",
        "amount_bounded",
        "self.item_id == 'vip'"
    )]);
    let obs_artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(obs_law)?;
    let obs_types = vec![quality_item_type()];
    let obs_catalog =
        TypeCatalog::new(&obs_types).with_governing_laws(std::slice::from_ref(&obs_artifact));
    let mut obs_cell: ObservationCell = MemoryCell::new(|_| JsonObject::new())
        .with_types(obs_types.clone())
        .with_governing_laws(vec![obs_artifact.clone()]);
    let provenance = source_provenance();
    for (id, item_id, expected_findings) in [("obs-vip", "vip", 0), ("obs-reg", "regular", 1)] {
        let key = FactKey::new(ITEM, id);
        let slice = load_slice(&obs_cell, [key.clone()])?;
        let vals = object(json!({"item_id": item_id, "amount": 500}));
        let prepared = prepare_observed_snapshot(
            &obs_catalog,
            &slice,
            &turn(id),
            ObservedSnapshot {
                key: &key,
                values: &vals,
                lifecycles: &[],
                provenance: &provenance,
            },
        )?;
        let (commit, auth) = prepared.into_parts();
        obs_cell.commit_observed(commit, auth)?;
        let count = obs_cell.ledger().last().map_or(0, |entry| {
            entry
                .events
                .iter()
                .filter(|event| matches!(event, ObservationEvent::DataQualityFinding { .. }))
                .count()
        });
        assert_eq!(count, expected_findings, "id={id}");
    }
    Ok(())
}

#[test]
fn val_exact_invariants_admit_and_evaluate_temporal_functions_with_decimals()
-> Result<(), Box<dyn std::error::Error>> {
    let definition: ObjectTypeDef = serde_json::from_value(json!({
        "name": ITEM,
        "properties": [
            {"id": "count", "type": {"kind": "integer"}},
            {"id": "hours", "type": {"kind": "decimal", "scale": 2}},
            {"id": "start_date", "type": {"kind": "date"}},
            {"id": "end_date", "type": {"kind": "date"}}
        ],
        "invariants": [
            {
                "id": "valid_shift_window",
                "expression": "((self.count >= 0) and ((self.hours >= 0.00) and (self.hours <= 24.00))) and (date(self.start_date) < date(self.end_date))",
                "message": "shift hours and date range must be valid"
            }
        ]
    }))?;
    let catalog = TypeCatalog::new([&definition]);

    // Valid record satisfies the invariant cleanly through the exact VM.
    let valid_values = object(json!({
        "count": 1,
        "hours": "8.50",
        "start_date": "2026-01-01",
        "end_date": "2026-01-05"
    }));
    let mut remaining = TurnFuel::new(10_000);
    let failures = norn_kernel::evaluate_invariants(
        &catalog,
        &FactKey::new(ITEM, "shift-ok"),
        &valid_values,
        &mut remaining,
    )
    .map_err(|err| format!("evaluate_invariants shift-ok: {err:?}"))?;
    assert!(
        failures.is_empty(),
        "expected no failures, got {failures:?}"
    );
    assert!(remaining.remaining() < 10_000);

    // Date-order violation evaluates to NotSatisfied (not an admission EvaluationFailed).
    let reversed_dates = object(json!({
        "count": 1,
        "hours": "8.50",
        "start_date": "2026-01-05",
        "end_date": "2026-01-01"
    }));
    let mut remaining = TurnFuel::new(10_000);
    let date_failures = norn_kernel::evaluate_invariants(
        &catalog,
        &FactKey::new(ITEM, "shift-bad-dates"),
        &reversed_dates,
        &mut remaining,
    )
    .map_err(|err| format!("evaluate_invariants shift-bad-dates: {err:?}"))?;
    let first_date_failure = date_failures
        .first()
        .ok_or("expected date-order invariant failure")?;
    assert_eq!(date_failures.len(), 1);
    assert_eq!(first_date_failure.outcome, DataQualityOutcome::NotSatisfied);

    // Decimal-range violation also evaluates to NotSatisfied.
    let bad_hours = object(json!({
        "count": 1,
        "hours": "25.00",
        "start_date": "2026-01-01",
        "end_date": "2026-01-05"
    }));
    let mut remaining = TurnFuel::new(10_000);
    let hour_failures = norn_kernel::evaluate_invariants(
        &catalog,
        &FactKey::new(ITEM, "shift-bad-hours"),
        &bad_hours,
        &mut remaining,
    )
    .map_err(|err| format!("evaluate_invariants shift-bad-hours: {err:?}"))?;
    let first_hour_failure = hour_failures
        .first()
        .ok_or("expected decimal-hours invariant failure")?;
    assert_eq!(hour_failures.len(), 1);
    assert_eq!(first_hour_failure.outcome, DataQualityOutcome::NotSatisfied);

    // Negative control: bare division alongside temporal/decimal still refuses at admission.
    let bad_def: ObjectTypeDef = serde_json::from_value(json!({
        "name": ITEM,
        "properties": [
            {"id": "hours", "type": {"kind": "decimal", "scale": 2}},
            {"id": "start_date", "type": {"kind": "date"}},
            {"id": "end_date", "type": {"kind": "date"}}
        ],
        "invariants": [
            {
                "id": "bare_division",
                "expression": "(self.hours / 2 >= 0) and (date(self.start_date) < date(self.end_date))",
                "message": "bare division must refuse"
            }
        ]
    }))?;
    let bad_catalog = TypeCatalog::new([&bad_def]);
    let div_values = object(json!({
        "hours": "8.50",
        "start_date": "2026-01-01",
        "end_date": "2026-01-05"
    }));
    let mut remaining = TurnFuel::new(10_000);
    let div_failures = norn_kernel::evaluate_invariants(
        &bad_catalog,
        &FactKey::new(ITEM, "shift-div"),
        &div_values,
        &mut remaining,
    )
    .map_err(|err| format!("evaluate_invariants shift-div: {err:?}"))?;
    let first_div_failure = div_failures
        .first()
        .ok_or("expected admission failure for bare division")?;
    assert!(matches!(
        first_div_failure.outcome,
        DataQualityOutcome::EvaluationFailed { .. }
    ));

    Ok(())
}

// ------------------------------------------- #1701: post-commit link laws

const DOC: &str = "linklaw.Doc";
const SPECIAL_DOC: &str = "linklaw.SpecialDoc";
const LINE: &str = "linklaw.Line";
const SPECIAL_LINE: &str = "linklaw.SpecialLine";
const OWNER: &str = "linklaw.Owner";
const TASK: &str = "linklaw.Task";

type Checked = Result<(), Box<dyn std::error::Error>>;

/// A retained predicate law `never <expression>` on `subject`.
fn link_law(
    id: &str,
    subject: &str,
    expression: &str,
) -> Result<norn_ir::ArtifactEnvelope, serde_json::Error> {
    let provenance = json!({"source": "authoring_sdk", "sourcePath": "invariant_commit.rs"});
    serde_json::from_value(json!({
        "artifactId": format!("{id}.law"),
        "dependencies": [{"artifactId": format!("{subject}.entity"), "kind": "entity",
                          "required": true, "version": "0.1.0"}],
        "irVersion": "0.1", "kind": "law", "lifecycle": "draft", "name": id,
        "namespace": "linklaw",
        "payload": {
            "id": id, "lawVersion": "0.1", "modality": {"kind": "never"},
            "owner": "linklaw.controls", "provenance": provenance, "rationale": id,
            "response": {"kind": "predicate", "expression": expression},
            "scope": {"root": {"kind": "entity", "id": subject}, "objectTypes": [subject]},
            "severity": "error", "subject": subject
        },
        "provenance": provenance,
        "scope": {"environmentId": "dev", "orgId": "org_demo"},
        "version": "0.1.0"
    }))
}

fn put_new(object_type: &str, key: &str, values: Value) -> Result<FactWrite, serde_json::Error> {
    Ok(FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_value(values)?,
        },
    })
}

fn add_link(source: &FactKey, link: &str, target: &FactKey) -> LinkWrite {
    LinkWrite {
        key: LinkKey::new(source.clone(), link, target.clone()),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    }
}

/// The refusal code of `result`, if it refused.
fn refusal<T>(result: &Result<T, CommitError<std::convert::Infallible>>) -> Option<&'static str> {
    result.as_ref().err().map(CommitError::code)
}

/// A document whose law bounds its lines; only the `SpecialLine` subtype
/// (not its root `Line`) declares the link whose inverse role is `lines`.
fn document_cell() -> Result<CommandCell, serde_json::Error> {
    let law = link_law("lines_within_limit", DOC, "len(self.lines) > self.limit")?;
    Ok(new_cell()
        .with_types(document_types()?)
        .with_governing_laws(vec![law]))
}

fn document_types() -> Result<Vec<ObjectTypeDef>, serde_json::Error> {
    serde_json::from_value(json!([
        {"name": DOC, "properties": [{"id": "limit", "type": {"kind": "integer"}}]},
        {"name": SPECIAL_DOC, "extends": DOC},
        {"name": LINE, "properties": [{"id": "amount", "type": {"kind": "integer"}}]},
        {"name": SPECIAL_LINE, "extends": LINE, "links": [{
            "id": "doc", "target": DOC, "cardinality": {"to": "one", "from": "many"},
            "inverse": "lines"
        }]}
    ]))
}

/// #1701: a document (new, stored base, or stored subtype) governed by an
/// inverse-role law counts a subtype line created and linked to it.
#[test]
fn inverse_role_law_counts_subtype_lines_for_new_and_stored_documents() -> Checked {
    for (doc_type, preseed_doc) in [(DOC, false), (DOC, true), (SPECIAL_DOC, true)] {
        for (limit, admitted) in [(0, false), (1, true)] {
            let mut cell = document_cell()?;
            let doc = FactKey::new(doc_type, "d");
            let line = FactKey::new(SPECIAL_LINE, "l");
            let mut writes = vec![put_new(SPECIAL_LINE, "l", json!({"amount": 1}))?];
            if preseed_doc {
                cell.commit(CellCommit {
                    writes: vec![put_new(doc_type, "d", json!({"limit": limit}))?],
                    ..CellCommit::empty()
                })?;
            } else {
                writes.insert(0, put_new(doc_type, "d", json!({"limit": limit}))?);
            }
            let result = cell.commit(CellCommit {
                writes,
                links: vec![add_link(&line, "doc", &doc)],
                ..CellCommit::empty()
            });
            let expected = (!admitted).then_some("core.laws.violated");
            assert_eq!(
                refusal(&result),
                expected,
                "{doc_type} preseed={preseed_doc} limit={limit}"
            );
            assert_eq!(cell.load(&doc)?.exists(), preseed_doc || admitted);
            assert_eq!(cell.load(&line)?.exists(), admitted);
        }
    }
    Ok(())
}

/// A task's law reads its owner (a forward link); an owner's law reads its
/// tasks (the inverse role).
fn owner_cell() -> Result<CommandCell, serde_json::Error> {
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {"name": OWNER, "properties": [
            {"id": "capacity", "type": {"kind": "integer"}},
            {"id": "minimum", "type": {"kind": "integer"}}
        ]},
        {"name": TASK, "properties": [{"id": "size", "type": {"kind": "integer"}}],
         "links": [{
            "id": "owner", "target": OWNER, "cardinality": {"to": "one", "from": "many"},
            "inverse": "tasks"
        }]}
    ]))?;
    let laws = vec![
        link_law("task_fits_owner", TASK, "self.size > self.owner.capacity")?,
        link_law("owner_keeps_tasks", OWNER, "len(self.tasks) < self.minimum")?,
    ];
    Ok(new_cell().with_types(types).with_governing_laws(laws))
}

/// #1701 (P2): deleting and unlinking a law-governed subject rechecks only its
/// surviving owner, and removing an absent law-read link from an absent subject
/// commits without rechecking a non-existent subject.
#[test]
fn deleting_and_unlinking_law_governed_subjects_and_absent_links() -> Checked {
    for (minimum, admitted) in [(0, true), (1, false)] {
        let mut cell = owner_cell()?;
        let owner = FactKey::new(OWNER, "o");
        let task = FactKey::new(TASK, "t");
        cell.commit(CellCommit {
            writes: vec![
                put_new(OWNER, "o", json!({"capacity": 10, "minimum": minimum}))?,
                put_new(TASK, "t", json!({"size": 1}))?,
            ],
            links: vec![add_link(&task, "owner", &owner)],
            ..CellCommit::empty()
        })?;
        let link = LinkKey::new(task.clone(), "owner", owner);
        let task_version = cell.load(&task)?.version;
        let link_version = cell.load_link(&link)?.version;
        let result = cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: task.clone(),
                expected: task_version,
                mutation: FactMutation::Delete,
            }],
            links: vec![LinkWrite {
                key: link.clone(),
                expected: link_version,
                mutation: LinkMutation::Remove,
            }],
            ..CellCommit::empty()
        });
        let expected = (!admitted).then_some("core.laws.violated");
        assert_eq!(refusal(&result), expected, "minimum {minimum}");
        assert_eq!(cell.load(&task)?.exists(), !admitted);
        assert_eq!(cell.load_link(&link)?.exists(), !admitted);
    }

    let mut cell = owner_cell()?;
    let owner = FactKey::new(OWNER, "o");
    cell.commit(CellCommit {
        writes: vec![put_new(OWNER, "o", json!({"capacity": 10, "minimum": 0}))?],
        ..CellCommit::empty()
    })?;
    let ghost = FactKey::new(TASK, "ghost");
    let result = cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(ghost.clone(), "owner", owner.clone()),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Remove,
        }],
        ..CellCommit::empty()
    });
    assert_eq!(refusal(&result), None);
    assert!(!cell.load(&ghost)?.exists());
    assert!(cell.load(&owner)?.exists());
    Ok(())
}

/// The stored graph of the observed-line regression: `SpecialDoc/d`
/// (`limit` 3) with one `SpecialLine/l` (`amount` 1), each at version 1,
/// and their incident edge at version `.0`.
struct StoredDocument(u64);

impl CommitView for StoredDocument {
    type Error = std::convert::Infallible;

    fn pending(&self, _: &Consumed) -> Result<bool, Self::Error> {
        Ok(true)
    }

    fn version(&self, key: &FactKey) -> Result<FactVersion, Self::Error> {
        Ok(match key.key.as_str() {
            "d" | "l" => FactVersion(1),
            _ => FactVersion::ABSENT,
        })
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Self::Error> {
        Ok(match key.key.as_str() {
            "d" => Some(ObjectTypeName(SPECIAL_DOC.to_owned())),
            "l" => Some(ObjectTypeName(SPECIAL_LINE.to_owned())),
            _ => None,
        })
    }

    fn link_version(&self, _: &LinkKey) -> Result<FactVersion, Self::Error> {
        Ok(FactVersion(self.0))
    }

    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Self::Error> {
        Ok(match key.key.as_str() {
            "d" | "l" => vec![LinkKey::new(
                FactKey::new(LINE, "l"),
                "doc",
                FactKey::new(DOC, "d"),
            )],
            _ => Vec::new(),
        })
    }

    fn object_values(&self, key: &FactKey) -> Result<Option<JsonObject>, Self::Error> {
        let values = match key.key.as_str() {
            "d" => json!({"limit": 3}),
            "l" => json!({"amount": 1}),
            _ => return Ok(None),
        };
        Ok(serde_json::from_value(values).ok())
    }
}

/// Documents whose `SpecialDoc` carries the inverse-role law itself, and is
/// a `Doc` only when `aliased`.
fn alias_types(aliased: bool) -> Result<Vec<ObjectTypeDef>, serde_json::Error> {
    let mut special = json!({
        "name": SPECIAL_DOC,
        "properties": [{"id": "limit", "type": {"kind": "integer"}}]
    });
    if aliased && let Some(definition) = special.as_object_mut() {
        definition.insert("extends".to_owned(), json!(DOC));
    }
    serde_json::from_value(json!([
        {"name": DOC},
        special,
        {"name": LINE, "properties": [{"id": "amount", "type": {"kind": "integer"}}]},
        {"name": SPECIAL_LINE, "extends": LINE, "links": [{
            "id": "doc", "target": SPECIAL_DOC, "cardinality": {"to": "one", "from": "many"},
            "inverse": "lines"
        }]}
    ]))
}

/// #1701: an observed line finding binds to the document's existing `Doc/d`
/// alias pin (single read, commits finding) when the host shares the alias,
/// and refuses with `commit.observation_authorization` when the host catalog
/// separates `Doc/d` and `SpecialDoc/d`.
#[test]
fn observed_line_finding_alias_pin_admission_and_mismatched_catalog_refusal() -> Checked {
    let pin = FactExpectation {
        key: FactKey::new(DOC, "d"),
        expected: FactVersion(1),
    };
    let doc = FactKey::new(SPECIAL_DOC, "d");
    let line = FactKey::new(SPECIAL_LINE, "l");
    let observed: JsonObject = serde_json::from_value(json!({"amount": 5}))?;

    for shared_alias in [true, false] {
        let (prep_types, host_types, laws, turn_id) = if shared_alias {
            let types = document_types()?;
            let laws = vec![link_law(
                "lines_sum_within_limit",
                DOC,
                "sum(map(self.lines, #.amount)) > self.limit",
            )?];
            (types.clone(), types, laws, "observed-line")
        } else {
            let laws = vec![link_law(
                "special_lines_within_limit",
                SPECIAL_DOC,
                "sum(map(self.lines, #.amount)) > self.limit",
            )?];
            (
                alias_types(true)?,
                alias_types(false)?,
                laws,
                "observed-line-alias",
            )
        };
        let catalog = TypeCatalog::new(&prep_types).with_governing_laws(&laws);
        let mut cell: ObservationCell = MemoryCell::new(|_| JsonObject::new())
            .with_types(host_types)
            .with_governing_laws(laws.clone());
        let mut writes = vec![
            put_new(SPECIAL_DOC, "d", json!({"limit": 3}))?,
            put_new(SPECIAL_LINE, "l", json!({"amount": 1}))?,
        ];
        if !shared_alias {
            writes.insert(0, put_new(DOC, "d", json!({}))?);
        }
        cell.commit(CellCommit {
            writes,
            links: vec![add_link(&line, "doc", &doc)],
            ..CellCommit::empty()
        })?;

        let slice = load_slice(&cell, [line.clone()])?;
        let (mut commit, mut authorization) = prepare_observed_parts(
            &catalog,
            &slice,
            &line,
            &observed,
            turn_id,
            &source_provenance(),
        );
        commit.reads.push(pin.clone());
        authorization.refresh_link_findings(&catalog, &mut commit, &StoredDocument(1))?;
        let document_reads: Vec<_> = commit
            .reads
            .iter()
            .filter(|read| read.key.key == "d")
            .collect();
        assert_eq!(document_reads, [&pin]);

        let before = cell.ledger().len();
        let receipt = cell.commit_observed(commit, authorization);
        if shared_alias {
            assert_eq!(refusal(&receipt), None);
            assert_ne!(
                last_entry_findings(&cell),
                Vec::<&norn_ir::DataQualityFinding>::new()
            );
            assert_eq!(cell.load(&line)?.values, Some(observed.clone()));
        } else {
            assert_eq!(refusal(&receipt), Some("commit.observation_authorization"));
            assert_eq!(cell.ledger().len(), before);
            assert_eq!(
                cell.load(&line)?.values,
                Some(serde_json::from_value(json!({"amount": 1}))?)
            );
        }
    }
    Ok(())
}

#[test]
fn exhausted_link_version_refuses_before_post_state_evaluation() -> Checked {
    let types = document_types()?;
    let laws = [link_law(
        "document_keeps_a_line",
        DOC,
        "len(self.lines) == 0",
    )?];
    let catalog = TypeCatalog::new(&types).with_governing_laws(&laws);
    let model = norn_kernel::CommitModel::new(&catalog);
    let mut write = put_new(SPECIAL_DOC, "d", json!({"limit":1}))?;
    write.expected = FactVersion(1);
    let mut other = put_new(SPECIAL_LINE, "l", json!({"amount":1}))?;
    other.expected = FactVersion(1);
    let edge = LinkWrite {
        key: LinkKey::new(other.key.clone(), "doc", write.key.clone()),
        expected: FactVersion(u64::MAX),
        mutation: LinkMutation::Remove,
    };
    let commit = CellCommit::<(), (), ()> {
        writes: vec![write, other],
        links: vec![edge],
        ..CellCommit::empty()
    };
    assert_eq!(
        norn_kernel::check_commit(&commit, &model, &StoredDocument(u64::MAX)),
        Err(CommitError::Overflow)
    );
    Ok(())
}

#[test]
fn forward_law_pins_the_created_targets_concrete_type_and_checked_version() -> Checked {
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {"name": LINE, "properties": [{"id": "amount", "type": {"kind": "integer"}}]},
        {"name": SPECIAL_LINE, "extends": LINE},
        {"name": DOC, "links": [{"id":"line","target":LINE,"cardinality":{"to":"one","from":"many"}}]}
    ]))?;
    let law = link_law("positive_line", DOC, "self.line.amount < 0")?;
    let mut cell = new_cell().with_types(types).with_governing_laws(vec![law]);
    let doc = FactKey::new(DOC, "d");
    let line = FactKey::new(LINE, "l");
    let result = cell.commit(CellCommit {
        writes: vec![
            put_new(DOC, "d", json!({}))?,
            put_new(SPECIAL_LINE, "l", json!({"amount":-1}))?,
        ],
        links: vec![add_link(&doc, "line", &line)],
        ..CellCommit::empty()
    });
    let Err(CommitError::Law {
        reason: norn_kernel::governing_laws::LawEvaluationError::Violated(failure),
        ..
    }) = result
    else {
        panic!("the document must refuse the proposed line");
    };
    let pin = failure.links.get("line").expect("linked evidence");
    assert_eq!(
        (pin.object_type.as_str(), pin.key.as_str(), pin.version),
        (SPECIAL_LINE, "l", 1)
    );
    Ok(())
}
