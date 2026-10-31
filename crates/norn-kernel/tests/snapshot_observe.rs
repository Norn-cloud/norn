#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration tests use compact fixture helpers"
)]

//! Snapshot observation commit coverage (#2990).

use norn_ir::mapping::{ObservedLifecycleSnapshot, SnapshotDeviationReason};
use norn_ir::provenance::{InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp};
use norn_ir::{
    DataQualityFindingStatus, DataQualityLineage, DataQualityOutcome, DataQualityRuleRef,
    DataQualitySeverity, DataQualitySubject, DecisionOutput, InterfaceDef, ObjectTypeDef,
    PropertyId, PropertyTypeDef,
};
use norn_kernel::cell_host::{FactVersion, JsonObject, JsonValue};
use norn_kernel::snapshot_observe::{
    ObservationCommitRefusal, ObservationEvent, ObservationSynthesis, ObservedDelete,
    ObservedSnapshot, commit_observed_snapshot, prepare_observed_delete, prepare_observed_snapshot,
    tombstone_key,
};
use norn_kernel::{
    CellHost, CommitError, Consumed, DecisionTurn, FactEvent, FactKey, LogicalTime, ObservationId,
    Observed, Provenance, TypeCatalog, World, load_slice, prepare_decision,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<ObservationEvent, JsonObject, JsonObject>;

const CORE_ORDER: &str =
    include_str!("../../../registry/core/artifacts/commerce.SalesOrder.entity.json");
const CORE_ORDER_INTERFACE: &str =
    include_str!("../../../registry/core/artifacts/core.Order.interface.json");
const CORE_VALUE_TYPES: &str =
    include_str!("../../../registry/core/artifacts/core.value_types.entity.json");
const STANDARD_VALUE_TYPES: &str =
    include_str!("../../../registry/core/artifacts/std.value_types.entity.json");
// An authored two-value catalogue exercises membership without shipping a standards corpus.
const ISO4217_VALUE_TYPES: &str = r#"{"payload":{"propertyTypes":[{"name":"iso4217.currency_code","type":{"kind":"enum","values":[{"value":"EGP"},{"value":"USD"}]}}]}}"#;

fn cell() -> Cell {
    MemoryCell::new(|_| JsonObject::default())
}

fn next_delivery(cell: &mut Cell, id: &str) -> Observed<JsonObject> {
    assert!(cell.inject(Observed {
        id: ObservationId(id.to_owned()),
        provenance: Provenance {
            source: "ERPNext".to_owned(),
            source_id: "Sales Order/SO-0001".to_owned(),
            observed_at: LogicalTime::from_millis(1_000),
            mapping_version: "erpnext.norn_core@0.2.0".to_owned(),
        },
        input: JsonObject::default(),
    }));
    cell.observe().unwrap().expect("delivery stays pending")
}

fn turn(id: &str, consumes: Consumed) -> DecisionTurn {
    DecisionTurn {
        id: id.to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: Some(consumes),
    }
}

fn unconsumed_turn(id: &str) -> DecisionTurn {
    DecisionTurn {
        id: id.to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    }
}

fn provenance(rule: &str) -> InstanceProvenance {
    InstanceProvenance {
        source_fields: Vec::new(),
        markings: Vec::new(),
        source: SourceRecordRef::new("ERPNext", "Sales Order/SO-0001"),
        mapping_version: MappingVersion("erpnext.norn_core@0.2.0".to_owned()),
        transform: Some(rule.to_owned()),
        observed_at: Some(UtcTimestamp("2026-09-30T09:00:00Z".to_owned())),
        recorded_at: UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        upstream: Vec::new(),
    }
}

fn mapped_order() -> (
    FactKey,
    JsonObject,
    Vec<ObservedLifecycleSnapshot>,
    InstanceProvenance,
) {
    let values = serde_json::from_value(json!({
        "order_number":"TOY-ORDER-1", "order_date":"2026-10-01",
        "currency":"EGP", "total_amount":42
    }))
    .unwrap();
    let snapshot = ObservedLifecycleSnapshot {
        property: PropertyId("status".to_owned()),
        state: "approved".to_owned(),
        provenance: provenance("sales_order.status"),
    };
    (
        FactKey::new("commerce.SalesOrder", "TOY-ORDER-1"),
        values,
        vec![snapshot],
        provenance("sales_order.subject"),
    )
}

fn core_order_type() -> ObjectTypeDef {
    let artifact: Value = serde_json::from_str(CORE_ORDER).unwrap();
    serde_json::from_value(artifact["payload"]["objectType"].clone()).unwrap()
}

fn core_order_interface() -> InterfaceDef {
    let artifact: Value = serde_json::from_str(CORE_ORDER_INTERFACE).unwrap();
    serde_json::from_value(artifact["payload"]["interface"].clone()).unwrap()
}

fn core_order_property_types() -> Vec<PropertyTypeDef> {
    let referenced_names = [
        "core.document_number",
        "core.money_amount",
        "iso4217.currency_code",
        "iso8601.date",
    ];
    let property_types: Vec<PropertyTypeDef> =
        [CORE_VALUE_TYPES, STANDARD_VALUE_TYPES, ISO4217_VALUE_TYPES]
            .into_iter()
            .flat_map(|source| {
                let artifact: Value = serde_json::from_str(source).unwrap();
                serde_json::from_value::<Vec<PropertyTypeDef>>(
                    artifact["payload"]["propertyTypes"].clone(),
                )
                .unwrap()
            })
            .filter(|property_type| referenced_names.contains(&property_type.name.as_str()))
            .collect();
    assert_eq!(property_types.len(), referenced_names.len());
    property_types
}

fn core_order_cell(
    object_type: &ObjectTypeDef,
    interface: &InterfaceDef,
    property_types: &[PropertyTypeDef],
) -> Cell {
    cell()
        .with_types(vec![
            object_type.clone(),
            serde_json::from_value(json!({"name":"party.Customer"})).unwrap(),
            serde_json::from_value(json!({"name":norn_kernel::snapshot_observe::SOURCE_OWNERSHIP_TYPE,"properties":[{"id":"owner","type":{"kind":"json-ext"}}]})).unwrap(),
        ])
        .with_interfaces(vec![interface.clone()])
        .with_property_types(property_types.to_vec())
}

fn commit_owned_core_order(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    slice: &norn_kernel::FactSlice,
    turn: &DecisionTurn,
    snapshot: ObservedSnapshot<'_>,
) -> norn_kernel::CommitReceipt {
    let key = snapshot.key.clone();
    let owner =
        norn_kernel::snapshot_observe::ObservedSourceOwner::from_provenance(snapshot.provenance);
    let (mut commit, mut authorization) = prepare_observed_snapshot(catalog, slice, turn, snapshot)
        .unwrap()
        .into_parts();
    let customer = FactKey::new("party.Customer", "customer-1");
    commit.writes.push(norn_kernel::FactWrite {
        key: customer.clone(),
        expected: FactVersion::ABSENT,
        mutation: norn_kernel::FactMutation::Put {
            values: JsonObject::new(),
        },
    });
    commit.links.push(norn_kernel::LinkWrite {
        key: norn_kernel::LinkKey::new(key, "counterparty", customer),
        expected: FactVersion::ABSENT,
        mutation: norn_kernel::LinkMutation::Add {
            values: JsonObject::new(),
        },
    });
    let source = commit.links[0].key.source.clone();
    let target = commit.links[0].key.target.clone();
    commit.writes.push(norn_kernel::FactWrite {
        key: FactKey::new(
            norn_kernel::snapshot_observe::SOURCE_OWNERSHIP_TYPE,
            format!("{}/{}", source.object_type.as_str(), source.key),
        ),
        expected: FactVersion::ABSENT,
        mutation: norn_kernel::FactMutation::Put {
            values: JsonObject::from([(
                "owner".to_owned(),
                norn_ir::runtime::encode(&owner).unwrap(),
            )]),
        },
    });
    authorization
        .seal_link_intent(
            catalog,
            &source,
            "counterparty",
            Some(&target),
            &commit.links,
        )
        .unwrap();
    cell.commit_observed(commit, authorization).unwrap()
}

fn state(cell: &Cell, key: &FactKey, property: &str) -> JsonValue {
    cell.load(key)
        .unwrap()
        .values
        .unwrap()
        .get(property)
        .unwrap()
        .clone()
}

fn model(transitions: &Value) -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": "test.SnapshotOrder",
        "properties": [
            { "id": "amount", "type": { "kind": "integer" }, "default": "0" },
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "draft" }, { "value": "submitted" },
                { "value": "pending" }, { "value": "review" },
                { "value": "approved" }, { "value": "cancelled" }
            ] }, "default": "draft" }
        ],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["approved", "cancelled"],
            "transitions": transitions
        }]
    }))
    .unwrap()
}

fn run_deviation(target: &str, transitions: &Value, reason: SnapshotDeviationReason) {
    let object_type = model(transitions);
    let catalog = TypeCatalog::new([&object_type]);
    let key = FactKey::new("test.SnapshotOrder", "SO-1");
    let values: JsonObject = serde_json::from_value(json!({ "amount": 0 })).unwrap();
    let provenance = provenance("snapshot.status");
    let snapshots = [ObservedLifecycleSnapshot {
        property: PropertyId("status".to_owned()),
        state: target.to_owned(),
        provenance: provenance.clone(),
    }];
    let mut cell = cell();
    let delivery = next_delivery(&mut cell, "observation-1");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("observe-1", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &snapshots,
            provenance: &provenance,
        },
    )
    .unwrap();

    assert_eq!(
        state(&cell, &key, "status"),
        JsonValue::String(target.to_owned())
    );
    assert!(cell.ledger()[0].events.iter().any(|event| matches!(
        event,
        ObservationEvent::DataQualityFinding { fact }
            if matches!(
                &fact.rule_ref,
                DataQualityRuleRef::Builtin { id, version }
                    if id == "norn.lifecycle.observed_out_of_band" && version == "1"
            )
                && fact.subjects == vec![DataQualitySubject::new(
                    key.object_type.clone(),
                    key.key.clone(),
                )]
                && fact.severity == DataQualitySeverity::Warning
                && fact.status == DataQualityFindingStatus::Open
                && matches!(
                    &fact.outcome,
                    DataQualityOutcome::ObservedOutOfBand {
                        property,
                        from,
                        state,
                        reason: actual_reason,
                    } if property.as_str() == "status"
                        && from.as_deref() == Some("draft")
                        && state == target
                        && *actual_reason == reason
                )
                && fact.provenance == vec![DataQualityLineage::new(
                    provenance.clone(),
                    vec!["observe-1".to_owned()],
                )]
    )));
}

fn property_budget_type(count: usize) -> ObjectTypeDef {
    let properties = (0..count)
        .map(|index| {
            json!({
                "id": format!("property_{index}"),
                "type": { "kind": "integer" }
            })
        })
        .collect::<Vec<_>>();
    serde_json::from_value(json!({
        "name": "test.SnapshotBudgetOrder",
        "properties": properties
    }))
    .unwrap()
}

fn property_budget_values(count: usize, value: i64) -> JsonObject {
    let properties = (0..count)
        .map(|index| (format!("property_{index}"), json!(value)))
        .collect::<serde_json::Map<_, _>>();
    serde_json::from_value(Value::Object(properties)).unwrap()
}

fn seed_observed_snapshot(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    provenance: &InstanceProvenance,
    observation_id: &str,
) {
    let slice = load_slice(&*cell, [key.clone(), tombstone_key(key)]).unwrap();
    let prepared = prepare_observed_snapshot(
        catalog,
        &slice,
        &unconsumed_turn(observation_id),
        ObservedSnapshot {
            key,
            values,
            lifecycles: &[],
            provenance,
        },
    )
    .unwrap();
    let (commit, authorization) = prepared.into_parts();
    cell.commit_observed(commit, authorization).unwrap();
}

#[test]
fn observed_snapshot_reserves_its_recorded_event_in_the_event_limit() {
    let object_type = property_budget_type(31);
    let catalog = TypeCatalog::new([&object_type]);
    let key = FactKey::new("test.SnapshotBudgetOrder", "SO-BUDGET");
    let provenance = provenance("snapshot.event_budget");
    let mut cell = cell();
    seed_observed_snapshot(
        &mut cell,
        &catalog,
        &key,
        &property_budget_values(31, 1),
        &provenance,
        "budget-seed",
    );
    let slice = load_slice(&cell, [key.clone(), tombstone_key(&key)]).unwrap();

    let within_limit = prepare_observed_snapshot(
        &catalog,
        &slice,
        &unconsumed_turn("budget-30-properties"),
        ObservedSnapshot {
            key: &key,
            values: &property_budget_values(30, 2),
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap();
    assert_eq!(within_limit.events.len(), 31, "30 sets plus one aggregate");

    let refusal = prepare_observed_snapshot(
        &catalog,
        &slice,
        &unconsumed_turn("budget-31-properties"),
        ObservedSnapshot {
            key: &key,
            values: &property_budget_values(31, 3),
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        norn_kernel::snapshot_observe::ObservationRefusal::EventLimit {
            limit: 32,
            actual: 33,
        }
    ));
}

#[test]
fn mapped_core_order_catches_up_and_same_state_reimport_is_a_consuming_noop() {
    let object_type = core_order_type();
    let interface = core_order_interface();
    let property_types = core_order_property_types();
    let catalog = TypeCatalog::new([&object_type])
        .with_interfaces([&interface])
        .with_property_types(&property_types);
    let (key, values, snapshots, provenance) = mapped_order();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].state, "approved");
    assert_eq!(
        snapshots[0].provenance.transform.as_deref(),
        Some("sales_order.status")
    );

    let mut cell = core_order_cell(&object_type, &interface, &property_types);
    let delivery = next_delivery(&mut cell, "first-import");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let receipt = commit_owned_core_order(
        &mut cell,
        &catalog,
        &slice,
        &turn("first-import", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &snapshots,
            provenance: &provenance,
        },
    );

    assert_eq!(
        state(&cell, &key, "status"),
        JsonValue::String("approved".to_owned())
    );
    assert_eq!(
        receipt.written.len(),
        3,
        "order, required counterparty and ownership witness create atomically"
    );
    let events = &cell.ledger()[0].events;
    let path: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ObservationEvent::ObservedTransition {
                transition,
                from,
                to,
                synthesized,
                provenance,
                ..
            } => {
                assert_eq!(*synthesized, ObservationSynthesis::SnapshotCatchUp);
                assert_eq!(provenance, &snapshots[0].provenance);
                Some((transition.as_str(), from.as_str(), to.as_str()))
            }
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::DataQualityFinding { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .collect();
    assert_eq!(
        path,
        vec![
            ("submit", "draft", "submitted"),
            ("approve", "submitted", "approved")
        ]
    );
    assert!(cell.observe().unwrap().is_none());

    let before = cell.load(&key).unwrap();
    let delivery = next_delivery(&mut cell, "same-state-reimport");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let receipt = commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("same-state-reimport", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &snapshots,
            provenance: &provenance,
        },
    )
    .unwrap();
    assert_eq!(cell.load(&key).unwrap(), before);
    assert!(receipt.written.is_empty());
    assert!(cell.ledger()[1].events.is_empty());
    assert!(cell.ledger()[1].written.is_empty());
    assert!(cell.observe().unwrap().is_none());
}

#[test]
fn immutable_submitted_order_accepts_changed_observed_fields_and_cancellation() {
    let mut object_type = model(&json!([
        { "id": "submit", "from": ["draft"], "to": "submitted" },
        { "id": "cancel", "from": ["submitted"], "to": "cancelled" }
    ]));
    object_type.lifecycles[0].immutable = vec!["submitted".to_owned()];
    let catalog = TypeCatalog::new([&object_type]);
    let key = FactKey::new("test.SnapshotOrder", "SO-IMMUTABLE");
    let provenance = provenance("immutable.source");
    let mut cell = cell();

    let submitted = [ObservedLifecycleSnapshot {
        property: PropertyId("status".to_owned()),
        state: "submitted".to_owned(),
        provenance: provenance.clone(),
    }];
    let original: JsonObject = serde_json::from_value(json!({ "amount": 5 })).unwrap();
    let delivery = next_delivery(&mut cell, "immutable-submit-observation");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("immutable-submit-observation", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &original,
            lifecycles: &submitted,
            provenance: &provenance,
        },
    )
    .unwrap();
    assert_eq!(
        state(&cell, &key, "status"),
        JsonValue::String("submitted".to_owned())
    );
    let command: DecisionOutput = serde_json::from_value(json!({
        "facts": [{ "op": "set", "objectType": "test.SnapshotOrder",
            "key": "SO-IMMUTABLE", "property": "amount", "value": 9 }],
        "commands": [], "timers": []
    }))
    .unwrap();
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let refusal = prepare_decision::<std::convert::Infallible>(
        &catalog,
        &slice,
        &unconsumed_turn("immutable-command-write"),
        &command,
    )
    .unwrap_err();
    assert_eq!(refusal.code(), "lifecycle.validation.immutable_state_write");
    assert_eq!(state(&cell, &key, "amount"), JsonValue::from(5));

    let changed: JsonObject = serde_json::from_value(json!({ "amount": 9 })).unwrap();
    let cancelled = [ObservedLifecycleSnapshot {
        property: PropertyId("status".to_owned()),
        state: "cancelled".to_owned(),
        provenance: provenance.clone(),
    }];
    let delivery = next_delivery(&mut cell, "immutable-cancel-observation");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("immutable-cancel-observation", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &changed,
            lifecycles: &cancelled,
            provenance: &provenance,
        },
    )
    .unwrap();

    assert_eq!(state(&cell, &key, "amount"), JsonValue::from(9));
    assert_eq!(
        state(&cell, &key, "status"),
        JsonValue::String("cancelled".to_owned())
    );
    assert_cancel_observation_events(&cell.ledger()[1].events, &provenance);
}

fn assert_cancel_observation_events(events: &[ObservationEvent], provenance: &InstanceProvenance) {
    assert!(events.iter().any(|event| matches!(
        event,
        ObservationEvent::ObservedFact {
            event: norn_kernel::FactEvent::Set { property, .. },
            provenance: observed,
        } if property.as_str() == "amount" && observed == provenance
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ObservationEvent::ObservedTransition {
            transition,
            from,
            to,
            provenance: observed,
            ..
        } if transition.as_str() == "cancel"
            && from == "submitted"
            && to == "cancelled"
            && observed == provenance
    )));
    let modified: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ObservationEvent::ObservedFact {
                event:
                    norn_kernel::FactEvent::Modified {
                        before_version,
                        after_version,
                        before,
                        after,
                        changed_properties,
                        ..
                    },
                provenance: observed,
            } if observed == provenance => Some((
                before_version,
                after_version,
                before,
                after,
                changed_properties,
            )),
            ObservationEvent::ObservedFact { .. }
            | ObservationEvent::ObservedTransition { .. }
            | ObservationEvent::DataQualityFinding { .. }
            | ObservationEvent::ObservedDeleted { .. }
            | ObservationEvent::ObservedRecreated { .. } => None,
        })
        .collect();
    assert_eq!(modified.len(), 1);
    let (before_version, after_version, before, after, changed_properties) = modified[0];
    assert_eq!(Some(*after_version), before_version.checked_next());
    assert_eq!(
        before.get("status"),
        Some(&JsonValue::String("submitted".to_owned()))
    );
    assert_eq!(
        after.get("status"),
        Some(&JsonValue::String("cancelled".to_owned()))
    );
    assert_eq!(after.get("amount"), Some(&JsonValue::from(9)));
    assert_eq!(
        changed_properties
            .iter()
            .map(PropertyId::as_str)
            .collect::<Vec<_>>(),
        ["amount", "status"]
    );
}

#[test]
fn observed_snapshot_skips_false_guards_on_the_unique_declared_path() {
    let object_type = model(&json!([
        { "id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.amount > 1"] },
        { "id": "approve", "from": ["submitted"], "to": "approved", "guards": ["self.amount > 1"] }
    ]));
    let catalog = TypeCatalog::new([&object_type]);
    let key = FactKey::new("test.SnapshotOrder", "SO-2");
    let values: JsonObject = serde_json::from_value(json!({ "amount": 0 })).unwrap();
    let provenance = provenance("guarded.status");
    let snapshots = [ObservedLifecycleSnapshot {
        property: PropertyId("status".to_owned()),
        state: "approved".to_owned(),
        provenance: provenance.clone(),
    }];
    let mut cell = cell();
    let delivery = next_delivery(&mut cell, "guard-seed");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("guard-seed", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap();

    let slice = load_slice(&cell, [key.clone()]).unwrap();
    let command_transition: DecisionOutput = serde_json::from_value(json!({
        "facts": [{ "op": "transition", "objectType": "test.SnapshotOrder", "key": "SO-2",
            "lifecycle": "status", "transition": "submit" }],
        "commands": [], "timers": []
    }))
    .unwrap();
    let refusal = prepare_decision::<std::convert::Infallible>(
        &catalog,
        &slice,
        &unconsumed_turn("command-transition"),
        &command_transition,
    )
    .unwrap_err();
    assert_eq!(refusal.code(), "decision.guard_false");

    let delivery = next_delivery(&mut cell, "guarded-observation");
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("guarded-observation", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &snapshots,
            provenance: &provenance,
        },
    )
    .unwrap();
    assert_eq!(
        state(&cell, &key, "status"),
        JsonValue::String("approved".to_owned())
    );
    assert_eq!(
        cell.ledger()[1]
            .events
            .iter()
            .filter(|event| matches!(event, ObservationEvent::ObservedTransition { .. }))
            .count(),
        2,
    );
}

#[test]
fn unreachable_valid_terminal_is_projected_as_out_of_band() {
    run_deviation(
        "cancelled",
        &json!([{ "id": "approve", "from": ["draft"], "to": "approved" }]),
        SnapshotDeviationReason::NoDeclaredPath,
    );
}

#[test]
fn equal_shortest_paths_are_projected_as_ambiguous_out_of_band() {
    run_deviation(
        "approved",
        &json!([
            { "id": "submit", "from": ["draft"], "to": "pending" },
            { "id": "review", "from": ["draft"], "to": "review" },
            { "id": "approve_pending", "from": ["pending"], "to": "approved" },
            { "id": "approve_review", "from": ["review"], "to": "approved" }
        ]),
        SnapshotDeviationReason::AmbiguousShortestPath,
    );
}

#[test]
fn stale_snapshot_commit_is_atomic_and_does_not_consume_delivery() {
    let object_type = core_order_type();
    let interface = core_order_interface();
    let property_types = core_order_property_types();
    let catalog = TypeCatalog::new([&object_type])
        .with_interfaces([&interface])
        .with_property_types(&property_types);
    let (key, values, snapshots, provenance) = mapped_order();
    let mut cell = core_order_cell(&object_type, &interface, &property_types);

    let delivery = next_delivery(&mut cell, "initial");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    commit_owned_core_order(
        &mut cell,
        &catalog,
        &slice,
        &turn("initial", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    );
    let stale = load_slice(&cell, [key.clone()]).unwrap();

    let mut changed = values.clone();
    changed.insert(
        "order_number".to_owned(),
        JsonValue::String("SO-CHANGED".to_owned()),
    );
    let delivery = next_delivery(&mut cell, "concurrent-update");
    let slice = load_slice(&cell, [key.clone()]).unwrap();
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn("concurrent-update", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &changed,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap();
    let before = cell.load(&key).unwrap();
    let ledger_len = cell.ledger().len();

    let delivery = next_delivery(&mut cell, "stale-snapshot");
    let refusal = commit_observed_snapshot(
        &mut cell,
        &catalog,
        &stale,
        &turn("stale-snapshot", delivery.consumed()),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &snapshots,
            provenance: &provenance,
        },
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        ObservationCommitRefusal::Commit(CommitError::Conflict { .. })
    ));
    assert_eq!(cell.load(&key).unwrap(), before);
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(
        cell.observe().unwrap().unwrap().id,
        ObservationId("stale-snapshot".to_owned())
    );
}

// ------------------------------------------------------------ deletes (#3013)

fn lifecycle_chain(steps: usize) -> (ObjectTypeDef, String) {
    let states = (0..=steps)
        .map(|index| format!("state_{index}"))
        .collect::<Vec<_>>();
    let initial = states.first().unwrap().clone();
    let target = states.last().unwrap().clone();
    let state_values = states
        .iter()
        .map(|state| json!({ "value": state }))
        .collect::<Vec<_>>();
    let transitions = states
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            json!({
                "id": format!("advance_{index}"),
                "from": [pair[0]],
                "to": pair[1],
            })
        })
        .collect::<Vec<_>>();
    let object_type = serde_json::from_value(json!({
        "name": "test.SnapshotDeleteBudgetOrder",
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": state_values },
            "default": initial,
        }],
        "lifecycles": [{
            "property": "status",
            "initial": [initial],
            "terminal": [target],
            "transitions": transitions,
        }]
    }))
    .unwrap();
    (object_type, target)
}

#[expect(
    clippy::unwrap_in_result,
    reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
)]
fn prepare_delete_for_steps(
    steps: usize,
) -> Result<usize, norn_kernel::snapshot_observe::ObservationRefusal> {
    let (object_type, target) = lifecycle_chain(steps);
    let catalog = TypeCatalog::new([&object_type]);
    let key = FactKey::new("test.SnapshotDeleteBudgetOrder", "SO-DELETE-BUDGET");
    let provenance = provenance("snapshot.delete_budget");
    let mut cell = cell();
    seed_observed_snapshot(
        &mut cell,
        &catalog,
        &key,
        &JsonObject::default(),
        &provenance,
        "delete-budget-seed",
    );
    let slice = load_slice(&cell, [key.clone(), tombstone_key(&key)]).unwrap();
    let lifecycles = [ObservedLifecycleSnapshot {
        property: PropertyId("status".to_owned()),
        state: target,
        provenance: provenance.clone(),
    }];
    let prepared = prepare_observed_delete(
        &catalog,
        &slice,
        &unconsumed_turn("delete-event-budget"),
        ObservedDelete {
            key: &key,
            lifecycles: &lifecycles,
            provenance: &provenance,
        },
    )?;
    Ok(prepared.events.len())
}

#[test]
fn reachable_observed_deletes_reserve_the_recorded_event_in_the_event_limit() {
    assert_eq!(
        prepare_delete_for_steps(29).unwrap(),
        31,
        "29 transitions, one aggregate, and one delete leave room for the wrapper"
    );
    assert!(matches!(
        prepare_delete_for_steps(30),
        Err(
            norn_kernel::snapshot_observe::ObservationRefusal::EventLimit {
                limit: 32,
                actual: 33,
            }
        )
    ));
}

fn delete_world(transitions: &Value) -> (ObjectTypeDef, FactKey, Cell) {
    let object_type = model(transitions);
    let key = FactKey::new("test.SnapshotOrder", "SO-1");
    let mut cell = cell();
    let catalog = TypeCatalog::new([&object_type]);
    let values: JsonObject = serde_json::from_value(json!({ "amount": 5 })).unwrap();
    let provenance = provenance("snapshot.subject");
    let slice = load_slice(&cell, [key.clone(), tombstone_key(&key)]).unwrap();
    let prepared = prepare_observed_snapshot(
        &catalog,
        &slice,
        &unconsumed_turn("create"),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap();
    let (commit, authorization) = prepared.into_parts();
    cell.commit_observed(commit, authorization).unwrap();
    (object_type, key, cell)
}

fn observe_delete(
    cell: &mut Cell,
    object_type: &ObjectTypeDef,
    key: &FactKey,
    on_delete: Option<&str>,
) -> Vec<ObservationEvent> {
    let catalog = TypeCatalog::new([object_type]);
    let provenance = provenance("snapshot.delete");
    let snapshots: Vec<ObservedLifecycleSnapshot> = on_delete
        .map(|state| ObservedLifecycleSnapshot {
            property: PropertyId("status".to_owned()),
            state: state.to_owned(),
            provenance: provenance.clone(),
        })
        .into_iter()
        .collect();
    let slice = load_slice(&*cell, [key.clone(), tombstone_key(key)]).unwrap();
    let prepared = prepare_observed_delete(
        &catalog,
        &slice,
        &unconsumed_turn("delete"),
        ObservedDelete {
            key,
            lifecycles: &snapshots,
            provenance: &provenance,
        },
    )
    .unwrap();
    let events = prepared.events.clone();
    let (commit, authorization) = prepared.into_parts();
    cell.commit_observed(commit, authorization).unwrap();
    events
}

#[test]
fn an_observed_delete_reaches_its_on_delete_state_when_a_path_is_declared() {
    let transitions = json!([{ "id": "cancel", "from": ["draft"], "to": "cancelled" }]);
    let (object_type, key, mut cell) = delete_world(&transitions);
    let events = observe_delete(&mut cell, &object_type, &key, Some("cancelled"));
    assert!(
        matches!(
            &events[..],
            [
                ObservationEvent::ObservedTransition { transition, .. },
                ObservationEvent::ObservedFact {
                    event: FactEvent::Modified {
                        before_version,
                        after_version,
                        changed_properties,
                        ..
                    },
                    ..
                },
                ObservationEvent::ObservedDeleted { tombstoned: false, .. }
            ] if transition.as_str() == "cancel"
                && *before_version == FactVersion(1)
                && *after_version == FactVersion(2)
                && changed_properties == &[PropertyId("status".to_owned())]
        ),
        "{events:?}"
    );
    assert_eq!(
        state(&cell, &key, "status"),
        JsonValue::String("cancelled".to_owned())
    );
    assert!(!cell.load(&tombstone_key(&key)).unwrap().exists());
}

#[test]
fn a_source_delete_of_an_object_already_in_its_on_delete_state_is_still_recorded() {
    let transitions = json!([{ "id": "cancel", "from": ["draft"], "to": "cancelled" }]);
    let (object_type, key, mut cell) = delete_world(&transitions);
    // Cancelled first by a command, then the source deletes it: no
    // transition is needed, but the delete is recorded with its provenance.
    let before = cell.load(&key).unwrap();
    let mut values = before.values.clone().unwrap();
    values.insert(
        "status".to_owned(),
        JsonValue::String("cancelled".to_owned()),
    );
    cell.commit(norn_kernel::CellCommit {
        writes: vec![norn_kernel::FactWrite {
            key: key.clone(),
            expected: before.version,
            mutation: norn_kernel::FactMutation::Put { values },
        }],
        ..norn_kernel::CellCommit::empty()
    })
    .unwrap();
    let version = cell.load(&key).unwrap().version;
    let events = observe_delete(&mut cell, &object_type, &key, Some("cancelled"));
    assert!(
        matches!(
            &events[..],
            [ObservationEvent::ObservedDeleted { tombstoned: false, provenance, .. }]
                if provenance.transform.as_deref() == Some("snapshot.delete")
        ),
        "{events:?}"
    );
    assert_eq!(
        cell.load(&key).unwrap().version,
        version,
        "nothing rewritten"
    );
}

#[test]
fn an_observed_delete_without_a_declared_path_tombstones_and_a_recreate_lifts_it() {
    let transitions = json!([{ "id": "approve", "from": ["draft"], "to": "approved" }]);
    let (object_type, key, mut cell) = delete_world(&transitions);
    let events = observe_delete(&mut cell, &object_type, &key, Some("cancelled"));
    assert!(
        matches!(
            &events[..],
            [ObservationEvent::ObservedDeleted {
                tombstoned: true,
                ..
            }]
        ),
        "{events:?}"
    );
    let tombstone = cell.load(&tombstone_key(&key)).unwrap().values.unwrap();
    assert_eq!(
        tombstone.get("source"),
        Some(&JsonValue::String(
            "source:ERPNext/Sales Order/SO-0001".to_owned()
        ))
    );
    assert_eq!(
        tombstone.get("at"),
        Some(&JsonValue::String("2026-09-30T09:00:00Z".to_owned()))
    );
    // The object keeps its values: nothing is erased.
    assert_eq!(state(&cell, &key, "amount"), JsonValue::from(5_i64));
    // Deleting again is a no-op; observing the record again recreates it.
    assert!(observe_delete(&mut cell, &object_type, &key, None).is_empty());
    let catalog = TypeCatalog::new([&object_type]);
    let values: JsonObject = serde_json::from_value(json!({ "amount": 7 })).unwrap();
    let provenance = provenance("snapshot.subject");
    let slice = load_slice(&cell, [key.clone(), tombstone_key(&key)]).unwrap();
    let prepared = prepare_observed_snapshot(
        &catalog,
        &slice,
        &unconsumed_turn("recreate"),
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .unwrap();
    assert!(
        prepared
            .events
            .iter()
            .any(|event| matches!(event, ObservationEvent::ObservedRecreated { .. }))
    );
    let (commit, authorization) = prepared.into_parts();
    cell.commit_observed(commit, authorization).unwrap();
    assert!(!cell.load(&tombstone_key(&key)).unwrap().exists());
}

#[test]
fn deleting_an_unknown_object_changes_nothing() {
    let object_type = model(&json!([]));
    let key = FactKey::new("test.SnapshotOrder", "SO-404");
    let mut cell = cell();
    assert!(observe_delete(&mut cell, &object_type, &key, None).is_empty());
    assert!(!cell.load(&tombstone_key(&key)).unwrap().exists());
}

#[test]
fn snapshot_event_chunks_preserve_ordinary_cap_and_bind_the_atomic_batch() {
    let object_type = model(&json!([]));
    let catalog = TypeCatalog::new([&object_type]);
    let mut cell = cell().with_types(vec![object_type.clone()]);
    let delivery = next_delivery(&mut cell, "complete-snapshot");
    let decision_turn = unconsumed_turn("complete-snapshot");
    let values: JsonObject = serde_json::from_value(json!({"amount": 0})).unwrap();
    let source = provenance("snapshot.batch");
    let mut commit = norn_kernel::CellCommit::empty().consuming(delivery.consumed());
    let mut authorization = norn_kernel::ObservationAuthorization::empty();
    for index in 0..32 {
        let key = FactKey::new("test.SnapshotOrder", format!("batch-{index}"));
        let slice = load_slice(&cell, [key.clone(), tombstone_key(&key)]).unwrap();
        let prepared = prepare_observed_snapshot(
            &catalog,
            &slice,
            &decision_turn,
            ObservedSnapshot {
                key: &key,
                values: &values,
                lifecycles: &[],
                provenance: &source,
            },
        )
        .unwrap();
        let (prepared, authority) = prepared.into_parts();
        commit.reads.extend(prepared.reads);
        commit.writes.extend(prepared.writes);
        commit.events.extend(prepared.events);
        authorization.merge(authority);
    }
    commit.events.insert(0, commit.events[0].clone());
    assert!(commit.events.len() > 32);
    assert!(matches!(
        cell.commit_observed(commit.clone(), authorization.clone()),
        Err(CommitError::EventLimitExceeded { limit: 32, .. })
    ));
    let chunks = authorization.seal_snapshot_event_chunks(&commit).unwrap();
    assert_eq!(chunks.iter().sum::<usize>(), commit.events.len());
    assert!(chunks.iter().all(|count| *count > 0 && *count <= 32));
    let mut changed = commit.clone();
    changed.events.push(changed.events[0].clone());
    assert!(matches!(
        cell.commit_observed(changed, authorization.clone()),
        Err(CommitError::ObservationAuthorization)
    ));
    let mut changed = commit.clone();
    changed.consumes = None;
    assert!(matches!(
        cell.commit_observed(changed, authorization.clone()),
        Err(CommitError::ObservationAuthorization)
    ));
    assert!(cell.observe().unwrap().is_some());
    cell.commit_observed(commit, authorization).unwrap();
    assert!(cell.observe().unwrap().is_none());
    for index in 0..32 {
        assert!(
            cell.load(&FactKey::new(
                "test.SnapshotOrder",
                format!("batch-{index}")
            ))
            .unwrap()
            .values
            .is_some()
        );
    }
}

#[test]
fn empty_authority_cannot_request_an_unbounded_snapshot_envelope() {
    let mut cell = cell();
    let delivery = next_delivery(&mut cell, "empty-snapshot");
    let mut commit = norn_kernel::CellCommit::<ObservationEvent, JsonObject, JsonObject>::empty()
        .consuming(delivery.consumed());
    let event = ObservationEvent::ObservedFact {
        event: FactEvent::Created {
            key: FactKey::new("test.SnapshotOrder", "fake"),
            values: JsonObject::new(),
        },
        provenance: provenance("snapshot.fake"),
    };
    commit.events = vec![event; 33];
    assert!(
        norn_kernel::ObservationAuthorization::empty()
            .seal_snapshot_event_chunks(&commit)
            .is_err()
    );
    assert!(cell.observe().unwrap().is_some());
}

#[test]
fn observations_cannot_reconcile_a_protected_lifecycle() {
    let mut object_type = model(&json!([{ "id":"submit", "from":["draft"], "to":"submitted" }]));
    let status = object_type
        .properties
        .iter_mut()
        .find(|p| p.id.as_str() == "status")
        .unwrap();
    status.protected = true;
    let catalog = TypeCatalog::new([&object_type]);
    let key = FactKey::new("test.SnapshotOrder", "PROTECTED");
    let provenance = provenance("protected.source");
    let mut cell = cell();
    for (id, amount, state, refused) in [
        ("initialize", 5, None, false),
        ("refuse-change", 6, Some("submitted"), true),
        ("unchanged", 6, Some("draft"), false),
    ] {
        let delivery = next_delivery(&mut cell, id);
        let slice = load_slice(&cell, [key.clone()]).unwrap();
        let before = cell.load(&key).unwrap();
        let ledger_len = cell.ledger().len();
        let lifecycles = state.map(|state| ObservedLifecycleSnapshot {
            property: PropertyId("status".to_owned()),
            state: state.to_owned(),
            provenance: provenance.clone(),
        });
        let values = serde_json::from_value(json!({"amount":amount})).unwrap();
        let result = commit_observed_snapshot(
            &mut cell,
            &catalog,
            &slice,
            &turn(id, delivery.consumed()),
            ObservedSnapshot {
                key: &key,
                values: &values,
                lifecycles: lifecycles.as_slice(),
                provenance: &provenance,
            },
        );
        if refused {
            assert!(matches!(result, Err(ObservationCommitRefusal::Observation(
                norn_kernel::snapshot_observe::ObservationRefusal::InvalidLifecycle { reason, .. })) if reason.contains("protected lifecycle")));
            assert_eq!(cell.load(&key).unwrap(), before);
            assert_eq!(cell.ledger().len(), ledger_len);
            assert!(cell.observe().unwrap().is_some());
            // Consume the refused delivery before injecting the next case.
            cell.commit(norn_kernel::CellCommit::empty().consuming(delivery.consumed()))
                .unwrap();
        } else {
            result.unwrap();
            assert_eq!(
                cell.load(&key).unwrap().values.unwrap()["amount"],
                json!(amount)
            );
        }
    }
}
