#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    reason = "integration-test crate: fixture helpers unwrap and index"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! The decision-result commit path (T2-KER-2b, #2535).
//!
//! A `DecisionOutput` (fact changes, command records, timer records) becomes
//! one `CellCommit` through `prepare_decision` and commits through the
//! in-memory reference `CellHost`. Every refusal — stale `from` state, false
//! guard, unknown transition, invalid output, stale slice (CAS), consumed
//! delivery — has a stable code and commits nothing.

use norn_ir::{DecisionOutput, ObjectTypeDef, ObjectTypeExtension, PropertyTypeDef, RuntimeValue};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, Consumed, DecisionRefusal, DecisionTurn, Effect,
    EffectiveValuesError, EnvelopeKind, FactEvent, FactExpectation, FactKey, FactMutation,
    FactSlice, FactState, FactVersion, FactWrite, FreshnessPolicy, Generation, IdempotencyKey,
    LogicalTime, Timer, TimerId, TurnEnvelopeLimits, TurnEnvelopeMeter, TypeCatalog, World,
    WorldId, commit_decision, commit_decision_with_meter, load_slice, prepare_decision,
    prepare_decision_with_limits, prepare_decision_with_source_owned_targets,
};
use norn_testkit::Settings;
use norn_testkit::cell_host::MemoryCell;
use norn_testkit::proptest::prelude::*;
use serde_json::json;
use std::convert::Infallible;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

const ORDER: &str = "commerce.SalesOrder";
const BIG: &str = "commerce.BigOrder";
const QUICK: &str = "commerce.QuickOrder";

fn types() -> Vec<ObjectTypeDef> {
    let order = json!({
        "name": ORDER,
        "properties": [
            { "id": "order_no", "type": { "kind": "text" } },
            { "id": "amount", "type": { "kind": "integer" } },
            { "id": "approver", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": { "kind": "enum", "values": [
                    { "value": "draft" }, { "value": "submitted" },
                    { "value": "approved" }, { "value": "cancelled" }
                ] },
                "default": "draft"
            }
        ],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["approved", "cancelled"],
            "transitions": [
                { "id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.amount > 0"] },
                { "id": "approve", "from": ["submitted"], "to": "approved", "guards": ["self.amount <= 1000"] },
                { "id": "cancel", "from": ["draft", "submitted"], "to": "cancelled" }
            ]
        }]
    });
    // The subtype restates the inherited guard and appends its own: its
    // declaration is the effective guard set.
    let big = json!({
        "name": BIG,
        "extends": ORDER,
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["approved", "cancelled"],
            "transitions": [
                { "id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.amount > 0"] },
                { "id": "approve", "from": ["submitted"], "to": "approved",
                  "guards": ["self.amount <= 1000", "self.approver != \"\""] },
                { "id": "cancel", "from": ["draft", "submitted"], "to": "cancelled" }
            ]
        }]
    });
    // Narrows the inherited status enum (no `approved`) without redeclaring
    // the lifecycle: the effective lifecycle drops `approve`.
    let quick = json!({
        "name": QUICK,
        "extends": ORDER,
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": [
                { "value": "draft" }, { "value": "submitted" }, { "value": "cancelled" }
            ] }
        }]
    });
    vec![
        serde_json::from_value(order).unwrap(),
        serde_json::from_value(big).unwrap(),
        serde_json::from_value(quick).unwrap(),
    ]
}

fn timestamp_guard_type(guard: &str) -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": "temporal.Guarded",
            "properties": [
                {
                    "id": "status",
                    "type": { "kind": "enum", "values": [
                        { "value": "draft" }, { "value": "accepted" }
                    ] }
                },
                { "id": "a", "type": { "kind": "timestamp", "timezone": "offset" } },
                { "id": "b", "type": { "kind": "timestamp", "timezone": "offset" } },
                {
                    "id": "optional_at",
                    "type": { "kind": "timestamp", "timezone": "offset" },
                    "constraints": { "nullable": true }
                }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["accepted"],
                "transitions": [{
                    "id": "accept", "from": ["draft"], "to": "accepted",
                    "guards": [guard]
                }]
            }]
        }))
        .unwrap(),
    ]
}

fn seed_timestamp_guard_row(cell: &mut Cell, id: &str, a: &str, b: &str) -> FactKey {
    let row = key("temporal.Guarded", id);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: row.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: record(json!({ "status": "draft", "a": a, "b": b })),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    row
}

fn output(value: serde_json::Value) -> DecisionOutput {
    serde_json::from_value(value).unwrap()
}

fn envelope_output(facts: usize, commands: usize, timers: usize, prefix: &str) -> DecisionOutput {
    let facts = (0..facts)
        .map(|index| create(ORDER, &format!("{prefix}-fact-{index}"), 500.0))
        .collect::<Vec<_>>();
    let commands = vec![json!({ "kind": "notify" }); commands];
    let timers = (0..timers)
        .map(|index| {
            json!({
                "afterMillis": 0,
                "timerId": format!("{prefix}-timer-{index}")
            })
        })
        .collect::<Vec<_>>();
    output(json!({ "facts": facts, "commands": commands, "timers": timers }))
}

fn record(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).unwrap()
}

fn turn(id: &str) -> DecisionTurn {
    DecisionTurn {
        id: id.to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    }
}

fn cell() -> Cell {
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
}

fn key(object_type: &str, key: &str) -> FactKey {
    FactKey::new(object_type, key)
}

fn create(object_type: &str, id: &str, amount: f64) -> serde_json::Value {
    json!({
        "op": "create", "objectType": object_type, "key": id,
        "values": { "order_no": id, "amount": amount, "approver": "" }
    })
}

fn transition(object_type: &str, id: &str, name: &str) -> serde_json::Value {
    json!({
        "op": "transition", "objectType": object_type, "key": id,
        "lifecycle": "status", "transition": name
    })
}

fn set(
    object_type: &str,
    id: &str,
    property: &str,
    value: &serde_json::Value,
) -> serde_json::Value {
    json!({
        "op": "set", "objectType": object_type, "key": id,
        "property": property, "value": value
    })
}

/// Commits `facts` (and nothing else) against a fresh slice of `keys`.
#[expect(
    clippy::unwrap_in_result,
    reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
)]
fn decide(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    id: &str,
    keys: &[FactKey],
    facts: &[serde_json::Value],
) -> Result<(), Box<DecisionRefusal<Infallible>>> {
    let slice = load_slice(cell, keys.iter().cloned()).unwrap();
    let output = output(json!({ "facts": facts, "commands": [], "timers": [] }));
    commit_decision(cell, catalog, &slice, &turn(id), &output)
        .map(|_| ())
        .map_err(Box::new)
}

/// Everything a commit could change.
fn visible(cell: &Cell, keys: &[FactKey]) -> (Vec<FactState>, usize, usize, usize) {
    (
        keys.iter().map(|key| cell.load(key).unwrap()).collect(),
        cell.ledger().len(),
        cell.outbox().len(),
        cell.timers().len(),
    )
}

fn status(cell: &Cell, key: &FactKey) -> Option<JsonValue> {
    cell.load(key)
        .unwrap()
        .values
        .and_then(|values| values.get("status").cloned())
}

/// A refused decision: the expected code, and nothing committed.
#[track_caller]
fn assert_refused(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    code: &str,
) -> DecisionRefusal<Infallible> {
    let keys: Vec<FactKey> = slice.keys().cloned().collect();
    let before = visible(cell, &keys);
    let refusal = commit_decision(cell, catalog, slice, turn, output).unwrap_err();
    assert_eq!(refusal.code(), code, "{refusal}");
    assert!(!refusal.to_string().is_empty());
    assert_eq!(visible(cell, &keys), before, "a refused decision committed");
    refusal
}

// ------------------------------------------------------------- fixtures

#[test]
fn decision_result_is_one_prepared_transition() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let so1 = key(ORDER, "SO-1");
    decide(
        &mut cell,
        &catalog,
        "t1",
        &[],
        &[create(ORDER, "SO-1", 500.0)],
    )
    .unwrap();
    // `create` filled the lifecycle default, an initial state.
    assert_eq!(status(&cell, &so1), Some(JsonValue::String("draft".into())));

    let slice = load_slice(&cell, [so1.clone()]).unwrap();
    let output = output(json!({
        "facts": [transition(ORDER, "SO-1", "submit")],
        "commands": [{ "kind": "notify", "order": "SO-1" }],
        "timers": [{ "afterMillis": 86_400_000, "kind": "remind", "order": "SO-1" }]
    }));
    let turn = turn("t2");
    let prepared = prepare_decision::<Infallible>(&catalog, &slice, &turn, &output).unwrap();

    let before = slice[&so1].values.clone().unwrap();
    let mut submitted = before.clone();
    submitted.insert("status".into(), JsonValue::String("submitted".into()));
    let expected: CellCommit<FactEvent, JsonObject, JsonObject> = CellCommit {
        consumes: None,
        reads: vec![],
        writes: vec![FactWrite {
            key: so1.clone(),
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: submitted.clone(),
            },
        }],
        events: vec![
            FactEvent::Transitioned {
                key: so1.clone(),
                lifecycle: norn_ir::PropertyId("status".into()),
                transition: norn_ir::TransitionId("submit".into()),
                from: "draft".into(),
                to: "submitted".into(),
            },
            FactEvent::Modified {
                key: so1.clone(),
                before_version: FactVersion(1),
                after_version: FactVersion(2),
                before,
                after: submitted.clone(),
                changed_properties: vec![norn_ir::PropertyId("status".into())],
            },
        ],
        effects: vec![
            Effect::Perform(Command {
                id: CommandId("t2/command/0".into()),
                generation: Generation(0),
                idempotency_key: IdempotencyKey("t2/command/0".into()),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: None,
                payload: record(json!({ "kind": "notify", "order": "SO-1" })),
            }),
            Effect::Schedule(Timer {
                id: TimerId("t2/timer/0".into()),
                due: LogicalTime::from_millis(86_401_000),
                payload: record(
                    json!({ "afterMillis": 86_400_000, "kind": "remind", "order": "SO-1" }),
                ),
            }),
        ],
        links: vec![],
        link_reads: Vec::new(),
    };
    assert_eq!(prepared, expected);
    // Preparing is pure: the same turn prepares the same commit.
    assert_eq!(
        prepare_decision::<Infallible>(&catalog, &slice, &turn, &output).unwrap(),
        expected
    );

    // One commit: the fact write, the ledger event, the command, the timer.
    let receipt = commit_decision(&mut cell, &catalog, &slice, &turn, &output).unwrap();
    assert_eq!(receipt.seq.0, 2);
    let entry = cell.ledger().last().unwrap();
    assert_eq!(entry.seq, receipt.seq);
    assert_eq!(entry.events, expected.events);
    assert_eq!(
        status(&cell, &so1),
        Some(JsonValue::String("submitted".into()))
    );
    assert_eq!(cell.load(&so1).unwrap().version, FactVersion(2));
    assert_eq!(cell.outbox().len(), 1);
    assert_eq!(cell.timers().len(), 1);
}

#[test]
fn decision_commit_enforces_each_v1_envelope_count_before_preparing_or_committing() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let at_cap = envelope_output(32, 32, 32, "at-cap");

    let prepared =
        prepare_decision::<Infallible>(&catalog, &FactSlice::new(), &turn("prepare"), &at_cap)
            .unwrap();
    assert_eq!(prepared.events.len(), 32);
    assert_eq!(prepared.effects.len(), 64);

    commit_decision(
        &mut cell,
        &catalog,
        &FactSlice::new(),
        &turn("at-cap"),
        &at_cap,
    )
    .unwrap();
    assert_eq!(cell.ledger().len(), 1);
    assert_eq!(cell.outbox().len(), 32);
    assert_eq!(cell.timers().len(), 32);

    for (kind, actuals) in [
        (EnvelopeKind::Events, (33, 0, 0)),
        (EnvelopeKind::Commands, (0, 33, 0)),
        (EnvelopeKind::Timers, (0, 0, 33)),
    ] {
        let output = envelope_output(actuals.0, actuals.1, actuals.2, "over-cap");
        let before = visible(&cell, &[]);
        let refusal = commit_decision(
            &mut cell,
            &catalog,
            &FactSlice::new(),
            &turn("over-cap"),
            &output,
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "decision.envelope_exceeded");
        assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
        assert!(matches!(
            refusal,
            DecisionRefusal::EnvelopeExceeded {
                kind: actual_kind,
                limit: 32,
                actual: 33,
            } if actual_kind == kind
        ));
        assert_eq!(
            visible(&cell, &[]),
            before,
            "an over-cap decision committed"
        );
    }
}

#[test]
fn rule_source_owned_targets_refuse_every_direct_fact_change_but_not_native_siblings() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let observed = key(ORDER, "OBS-1");
    let native = key(ORDER, "NATIVE-1");
    decide(
        &mut cell,
        &catalog,
        "seed-source-and-native",
        &[],
        &[create(ORDER, "OBS-1", 5.0), create(ORDER, "NATIVE-1", 6.0)],
    )
    .unwrap();
    let slice = load_slice(&cell, [observed.clone(), native]).unwrap();
    let protected = std::collections::BTreeSet::from([observed.clone()]);
    let turn = turn("source-owned-guard");
    let changes = [
        output(json!({
            "facts": [{ "op": "set", "objectType": ORDER, "key": "OBS-1", "property": "amount", "value": 10 }],
            "commands": [], "timers": []
        })),
        output(json!({
            "facts": [{ "op": "delete", "objectType": ORDER, "key": "OBS-1" }],
            "commands": [], "timers": []
        })),
        output(json!({
            "facts": [{ "op": "transition", "objectType": ORDER, "key": "OBS-1", "lifecycle": "status", "transition": "submit" }],
            "commands": [], "timers": []
        })),
        output(json!({
            "facts": [{ "op": "create", "objectType": ORDER, "key": "OBS-1", "values": { "amount": 10 } }],
            "commands": [], "timers": []
        })),
    ];
    for decision in &changes {
        assert!(matches!(
            prepare_decision_with_source_owned_targets::<Infallible>(
                &catalog,
                &slice,
                &turn,
                decision,
                &protected,
            ),
            Err(DecisionRefusal::SourceOwnedTarget { ref key }) if *key == observed
        ));
    }
    let native_write = output(json!({
        "facts": [{ "op": "set", "objectType": ORDER, "key": "NATIVE-1", "property": "amount", "value": 11 }],
        "commands": [], "timers": []
    }));
    assert!(
        prepare_decision_with_source_owned_targets::<Infallible>(
            &catalog,
            &slice,
            &turn,
            &native_write,
            &protected,
        )
        .is_ok()
    );
}

#[test]
fn aggregate_modified_events_count_toward_per_decision_and_turn_event_limits() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let row = key(ORDER, "modified-envelope");
    decide(
        &mut cell,
        &catalog,
        "modified-envelope-seed",
        &[],
        &[create(ORDER, "modified-envelope", 500.0)],
    )
    .unwrap();
    let slice = load_slice(&cell, [row.clone()]).unwrap();
    let update = output(json!({
        "facts": [set(ORDER, "modified-envelope", "amount", &json!(501))],
        "commands": [], "timers": []
    }));
    let limits = norn_ir::EnvelopeLimits {
        max_events: 1,
        ..norn_ir::EnvelopeLimits::V1
    };
    let refusal = prepare_decision_with_limits::<Infallible>(
        &catalog,
        &slice,
        &turn("modified-envelope-per-decision"),
        &update,
        &limits,
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        DecisionRefusal::EnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: 1,
            actual: 2,
        }
    ));

    let mut meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
        max_events: 1,
        max_commands: 1,
        max_timers: 1,
    });
    let refusal = commit_decision_with_meter(
        &mut cell,
        &catalog,
        &slice,
        &turn("modified-envelope-turn"),
        &update,
        &mut meter,
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        DecisionRefusal::TurnEnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: 1,
            actual: 2,
        }
    ));
    assert_eq!(meter.used(), Default::default());
    assert_eq!(status(&cell, &row), Some(JsonValue::String("draft".into())));
}

#[test]
fn aggregate_envelope_meter_counts_successful_commits_and_leaves_refusals_uncharged() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    for (kind, counts, prefix) in [
        (EnvelopeKind::Events, (1, 0, 0), "events"),
        (EnvelopeKind::Commands, (0, 1, 0), "commands"),
        (EnvelopeKind::Timers, (0, 0, 1), "timers"),
    ] {
        let mut meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
            max_events: 1,
            max_commands: 1,
            max_timers: 1,
        });
        let first = envelope_output(counts.0, counts.1, counts.2, &format!("{prefix}-first"));
        commit_decision_with_meter(
            &mut cell,
            &catalog,
            &FactSlice::new(),
            &turn(&format!("{prefix}-first")),
            &first,
            &mut meter,
        )
        .unwrap();
        let used = meter.used();
        let expected_used = match kind {
            EnvelopeKind::Events => (1, 0, 0),
            EnvelopeKind::Commands => (0, 1, 0),
            EnvelopeKind::Timers => (0, 0, 1),
        };
        assert_eq!((used.events, used.commands, used.timers), expected_used);

        let second = envelope_output(counts.0, counts.1, counts.2, &format!("{prefix}-second"));
        let before = visible(&cell, &[]);
        let refusal = commit_decision_with_meter(
            &mut cell,
            &catalog,
            &FactSlice::new(),
            &turn(&format!("{prefix}-second")),
            &second,
            &mut meter,
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "decision.turn_envelope_exceeded");
        assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
        assert!(matches!(
            refusal,
            DecisionRefusal::TurnEnvelopeExceeded {
                kind: actual_kind,
                limit: 1,
                actual: 2,
            } if actual_kind == kind
        ));
        assert_eq!(meter.used(), used);
        assert_eq!(visible(&cell, &[]), before);
    }

    let stale_key = key(ORDER, "SO-stale-meter");
    decide(
        &mut cell,
        &catalog,
        "seed-stale",
        &[],
        &[create(ORDER, "SO-stale-meter", 500.0)],
    )
    .unwrap();
    let stale_slice = load_slice(&cell, [stale_key.clone()]).unwrap();
    decide(
        &mut cell,
        &catalog,
        "advance-stale",
        std::slice::from_ref(&stale_key),
        &[set(ORDER, "SO-stale-meter", "amount", &json!(600))],
    )
    .unwrap();
    let mut stale_meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
        max_events: 10,
        max_commands: 10,
        max_timers: 10,
    });
    let stale_output = output(json!({
        "facts": [transition(ORDER, "SO-stale-meter", "submit")],
        "commands": [],
        "timers": []
    }));
    assert!(matches!(
        commit_decision_with_meter(
            &mut cell,
            &catalog,
            &stale_slice,
            &turn("stale"),
            &stale_output,
            &mut stale_meter,
        ),
        Err(DecisionRefusal::Stale { .. })
    ));
    assert_eq!(stale_meter.used(), Default::default());
}

#[test]
fn decision_refuses_transition_from_wrong_state() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let so1 = key(ORDER, "SO-1");
    decide(
        &mut cell,
        &catalog,
        "t1",
        &[],
        &[create(ORDER, "SO-1", 5.0)],
    )
    .unwrap();
    let slice = load_slice(&cell, [so1.clone()]).unwrap();
    let output = output(json!({
        "facts": [transition(ORDER, "SO-1", "approve")],
        "commands": [{ "kind": "notify" }],
        "timers": [{ "afterMillis": 5 }]
    }));
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t2"),
        &output,
        "lifecycle.validation.undeclared_write",
    );
    assert_eq!(
        refusal,
        DecisionRefusal::NotFromState {
            key: so1,
            transition: "approve".into(),
            current: Some("draft".into()),
            from: vec!["submitted".into()],
        }
    );
}

#[test]
fn decision_set_refuses_property_writes_after_an_immutable_transition() {
    let mut types = types();
    types[0].lifecycles[0].immutable = vec!["submitted".to_owned()];
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let order = key(ORDER, "SO-IMMUTABLE");
    decide(
        &mut cell,
        &catalog,
        "immutable-create",
        &[],
        &[create(ORDER, "SO-IMMUTABLE", 5.0)],
    )
    .unwrap();
    decide(
        &mut cell,
        &catalog,
        "immutable-submit",
        std::slice::from_ref(&order),
        &[transition(ORDER, "SO-IMMUTABLE", "submit")],
    )
    .unwrap();

    // `patch` is lowered to Set facts before commit; the kernel guard applies
    // to that same canonical write regardless of which authoring surface made it.
    let output = output(json!({
        "facts": [set(ORDER, "SO-IMMUTABLE", "amount", &json!(900))],
        "commands": [], "timers": []
    }));
    let slice = load_slice(&cell, [order.clone()]).unwrap();
    let ledger_len = cell.ledger().len();
    let refusal =
        commit_decision(&mut cell, &catalog, &slice, &turn("immutable-set"), &output).unwrap_err();
    assert_eq!(refusal.code(), "lifecycle.validation.immutable_state_write");
    assert_eq!(cell.ledger().len(), ledger_len);
    let stored = cell.load(&order).unwrap().values.unwrap();
    assert_eq!(
        stored.get("status"),
        Some(&JsonValue::String("submitted".to_owned()))
    );
    assert_eq!(
        stored.get("amount"),
        Some(&JsonValue::Number(norn_ir::Decimal::from(5_i64)))
    );
}

#[test]
fn decision_refuses_false_guard_and_unknown_transition() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let so0 = key(ORDER, "SO-0");
    decide(
        &mut cell,
        &catalog,
        "t1",
        &[],
        &[create(ORDER, "SO-0", 0.0)],
    )
    .unwrap();
    let slice = load_slice(&cell, [so0]).unwrap();

    let submit = output(
        json!({ "facts": [transition(ORDER, "SO-0", "submit")], "commands": [], "timers": [] }),
    );
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t2"),
        &submit,
        "decision.guard_false",
    );
    assert!(refusal.to_string().contains("self.amount > 0"), "{refusal}");

    let ship = output(
        json!({ "facts": [transition(ORDER, "SO-0", "ship")], "commands": [], "timers": [] }),
    );
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t2"),
        &ship,
        "artifact_set.lifecycle.unknown_transition",
    );

    let other = output(json!({
        "facts": [{ "op": "transition", "objectType": ORDER, "key": "SO-0", "lifecycle": "amount", "transition": "submit" }],
        "commands": [], "timers": []
    }));
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t2"),
        &other,
        "artifact_set.lifecycle.not_a_lifecycle",
    );
}

#[test]
fn admitted_offset_timestamps_are_canonical_before_transition_guards() {
    let types = timestamp_guard_type("self.a != self.b");
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell().with_types(types.clone());
    let row = seed_timestamp_guard_row(
        &mut cell,
        "legacy-offsets",
        "2024-02-29T10:00:00Z",
        "2024-02-29T12:00:00+02:00",
    );
    let slice = load_slice(&cell, [row.clone()]).unwrap();
    let output = output(json!({
        "facts": [transition("temporal.Guarded", "legacy-offsets", "accept")],
        "commands": [], "timers": []
    }));

    let refusal =
        commit_decision(&mut cell, &catalog, &slice, &turn("legacy"), &output).unwrap_err();
    assert_eq!(refusal.code(), "decision.guard_false");
    assert_eq!(status(&cell, &row), Some(JsonValue::String("draft".into())));
}

#[test]
fn set_timestamp_is_canonical_before_following_transition_guard() {
    let types = timestamp_guard_type("self.a != self.b");
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let row = seed_timestamp_guard_row(
        &mut cell,
        "set-offset",
        "2024-02-29T10:00:00Z",
        "2024-02-29T10:00:00Z",
    );
    let slice = load_slice(&cell, [row.clone()]).unwrap();
    let set_then_transition = output(json!({
        "facts": [
            set("temporal.Guarded", "set-offset", "a", &json!("2024-02-29T12:00:00+02:00")),
            transition("temporal.Guarded", "set-offset", "accept")
        ],
        "commands": [], "timers": []
    }));

    let refusal = commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("set-offset"),
        &set_then_transition,
    )
    .unwrap_err();
    assert_eq!(refusal.code(), "decision.guard_false");
    assert_eq!(status(&cell, &row), Some(JsonValue::String("draft".into())));

    let set_only = output(json!({
        "facts": [set(
            "temporal.Guarded",
            "set-offset",
            "a",
            &json!("2024-02-29T12:00:00+02:00")
        )],
        "commands": [], "timers": []
    }));
    let prepared =
        prepare_decision::<Infallible>(&catalog, &slice, &turn("set-only"), &set_only).unwrap();
    assert!(matches!(
        prepared.events.as_slice(),
        [FactEvent::Set {
            value: JsonValue::String(value),
            ..
        }] if value == "2024-02-29T10:00:00Z"
    ));
}

#[test]
fn existing_object_writes_emit_one_sorted_versioned_modified_snapshot() {
    let types = timestamp_guard_type("true");
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let row = seed_timestamp_guard_row(
        &mut cell,
        "modified-snapshot",
        "2024-02-29T10:00:00Z",
        "2024-02-29T10:00:00Z",
    );
    let slice = load_slice(&cell, [row.clone()]).unwrap();
    let before_version = slice.get(&row).unwrap().version;
    let update = output(json!({
        "facts": [
            set("temporal.Guarded", "modified-snapshot", "a", &json!("2024-03-01T10:00:00Z")),
            set("temporal.Guarded", "modified-snapshot", "b", &json!("2024-03-02T10:00:00Z")),
            set("temporal.Guarded", "modified-snapshot", "a", &json!("2024-03-03T10:00:00Z"))
        ],
        "commands": [], "timers": []
    }));
    let prepared =
        prepare_decision::<Infallible>(&catalog, &slice, &turn("modified-snapshot"), &update)
            .unwrap();
    let modified: Vec<_> = prepared
        .events
        .iter()
        .filter_map(|event| match event {
            FactEvent::Modified {
                before_version,
                after_version,
                before,
                after,
                changed_properties,
                ..
            } => Some((
                before_version,
                after_version,
                before,
                after,
                changed_properties,
            )),
            FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesAllocated { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::Created { .. }
            | FactEvent::Set { .. }
            | FactEvent::Deleted { .. }
            | FactEvent::Transitioned { .. }
            | FactEvent::InvariantRefused { .. } => None,
        })
        .collect();
    assert_eq!(modified.len(), 1);
    let (before, after, before_values, after_values, changed) = modified[0];
    assert_eq!(*before, before_version);
    assert_eq!(*after, before_version.checked_next().unwrap());
    assert_eq!(
        before_values.get("a"),
        Some(&JsonValue::String("2024-02-29T10:00:00Z".into()))
    );
    assert_eq!(
        after_values.get("a"),
        Some(&JsonValue::String("2024-03-03T10:00:00Z".into()))
    );
    assert_eq!(
        after_values.get("b"),
        Some(&JsonValue::String("2024-03-02T10:00:00Z".into()))
    );
    assert_eq!(
        changed
            .iter()
            .map(|property| property.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );

    let net_no_op = output(json!({
        "facts": [
            set("temporal.Guarded", "modified-snapshot", "a", &json!("2024-03-04T10:00:00Z")),
            set("temporal.Guarded", "modified-snapshot", "a", &json!("2024-02-29T10:00:00Z"))
        ],
        "commands": [], "timers": []
    }));
    let prepared =
        prepare_decision::<Infallible>(&catalog, &slice, &turn("modified-net-no-op"), &net_no_op)
            .unwrap();
    assert!(
        !prepared
            .events
            .iter()
            .any(|event| matches!(event, FactEvent::Modified { .. }))
    );
}

#[test]
fn canonical_set_value_can_satisfy_following_transition_guard() {
    let types = timestamp_guard_type("self.a == self.b");
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let row = seed_timestamp_guard_row(
        &mut cell,
        "set-equal",
        "2024-02-29T10:00:00Z",
        "2024-02-29T10:00:00Z",
    );
    let slice = load_slice(&cell, [row.clone()]).unwrap();
    let set_then_transition = output(json!({
        "facts": [
            set("temporal.Guarded", "set-equal", "a", &json!("2024-02-29T12:00:00+02:00")),
            transition("temporal.Guarded", "set-equal", "accept")
        ],
        "commands": [], "timers": []
    }));

    commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("set-equal"),
        &set_then_transition,
    )
    .unwrap();
    assert_eq!(
        status(&cell, &row),
        Some(JsonValue::String("accepted".into()))
    );
    assert!(
        !cell
            .load(&row)
            .unwrap()
            .values
            .unwrap()
            .contains_key("optional_at"),
        "an absent legacy field stays absent in working state"
    );
}

#[test]
fn decision_output_with_an_unrepresentable_number_is_refused() {
    // Far beyond the exact decimal range (#2770): the output does not decode,
    // so no guard can ever see a value it cannot evaluate exactly.
    let refused = serde_json::from_value::<DecisionOutput>(json!({
        "facts": [create(ORDER, "SO-H", 1e300)],
        "commands": [],
        "timers": []
    }));
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn decision_effective_guards_come_from_the_subtype() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let big = key(BIG, "BO-1");
    decide(&mut cell, &catalog, "t1", &[], &[create(BIG, "BO-1", 10.0)]).unwrap();
    decide(
        &mut cell,
        &catalog,
        "t2",
        std::slice::from_ref(&big),
        &[transition(BIG, "BO-1", "submit")],
    )
    .unwrap();

    // The subtype's appended guard (an approver) refuses what the base allows.
    let slice = load_slice(&cell, [big.clone()]).unwrap();
    let approve = output(
        json!({ "facts": [transition(BIG, "BO-1", "approve")], "commands": [], "timers": [] }),
    );
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t3"),
        &approve,
        "decision.guard_false",
    );
    assert!(refusal.to_string().contains("approver"), "{refusal}");

    // Setting the approver first, in the same decision, satisfies it: guards
    // read the state immediately before the change.
    let approve = output(json!({
        "facts": [set(BIG, "BO-1", "approver", &json!("ops")), transition(BIG, "BO-1", "approve")],
        "commands": [], "timers": []
    }));
    commit_decision(&mut cell, &catalog, &slice, &turn("t3"), &approve).unwrap();
    assert_eq!(
        status(&cell, &big),
        Some(JsonValue::String("approved".into()))
    );
}

#[test]
fn decision_refuses_invalid_output() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let so1 = key(ORDER, "SO-1");
    decide(
        &mut cell,
        &catalog,
        "t1",
        &[],
        &[create(ORDER, "SO-1", 5.0)],
    )
    .unwrap();
    let slice = load_slice(&cell, [so1]).unwrap();
    let cases = [
        (
            json!({ "facts": [], "commands": [], "timers": [{ "kind": "remind" }] }),
            "decision.invalid_output",
        ),
        (
            json!({ "facts": [], "commands": [], "timers": [{ "afterMillis": -5 }] }),
            "decision.invalid_output",
        ),
        (
            json!({ "facts": [], "commands": [], "timers": [{ "afterMillis": 1.5 }] }),
            "decision.invalid_output",
        ),
        (
            json!({ "facts": [], "commands": [], "timers": [{ "afterMillis": 5, "timerId": "" }] }),
            "decision.invalid_output",
        ),
        (
            json!({ "facts": [set(ORDER, "SO-1", "status", &json!("approved"))], "commands": [], "timers": [] }),
            "artifact_set.lifecycle.direct_write",
        ),
        (
            json!({ "facts": [create(ORDER, "SO-1", 1.0)], "commands": [], "timers": [] }),
            "decision.object_exists",
        ),
        (
            json!({ "facts": [{ "op": "delete", "objectType": ORDER, "key": "SO-2" }], "commands": [], "timers": [] }),
            "decision.not_in_slice",
        ),
        (
            json!({ "facts": [create("commerce.Unknown", "U-1", 1.0)], "commands": [], "timers": [] }),
            "decision.unknown_type",
        ),
        (
            json!({ "facts": [{ "op": "create", "objectType": ORDER, "key": "SO-9", "values": { "status": "approved" } }], "commands": [], "timers": [] }),
            "lifecycle.validation.undeclared_write",
        ),
        (
            // A later change fails: the earlier, valid one is not committed either.
            json!({ "facts": [set(ORDER, "SO-1", "amount", &json!(7)), transition(ORDER, "SO-1", "approve")], "commands": [], "timers": [] }),
            "lifecycle.validation.undeclared_write",
        ),
    ];
    for (value, code) in cases {
        assert_refused(
            &mut cell,
            &catalog,
            &slice,
            &turn("t2"),
            &output(value),
            code,
        );
    }
}

#[test]
fn generic_decision_cannot_forge_an_owned_rows_action_call_record() {
    let mut definitions = types();
    definitions.push(
        serde_json::from_value(json!({ "name": norn_kernel::ACTION_CALL_TYPE }))
            .expect("internal action-call type decodes"),
    );
    let catalog = TypeCatalog::new(&definitions);
    let proposal = output(json!({
        "facts": [{
            "op": "create",
            "objectType": norn_kernel::ACTION_CALL_TYPE,
            "key": "forged-call",
            "values": { "effectKind": norn_kernel::ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS }
        }],
        "commands": [],
        "timers": []
    }));

    let refusal = prepare_decision::<Infallible>(
        &catalog,
        &FactSlice::new(),
        &turn("forged-action-call"),
        &proposal,
    )
    .expect_err("generic decision output cannot create kernel action-call records");
    assert_eq!(refusal.code(), "decision.invalid_output");
}

#[test]
fn decision_world_rules() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let slice = FactSlice::new();
    let with_command =
        output(json!({ "facts": [], "commands": [{ "kind": "notify" }], "timers": [] }));
    let mut fork = turn("t1");
    fork.world = World::Fork {
        id: WorldId("scenario".into()),
    };
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &fork,
        &with_command,
        "decision.perform_outside_main",
    );
    let mut historical = turn("t1");
    historical.world = World::Historical {
        commit: norn_kernel::CommitId("c1".into()),
    };
    let empty = output(json!({ "facts": [], "commands": [], "timers": [] }));
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &historical,
        &empty,
        "decision.read_only_world",
    );
}

// ------------------------------------------------------------- CAS and races

#[test]
fn decision_stale_slice_is_refused_at_commit() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let so1 = key(ORDER, "SO-1");
    let so2 = key(ORDER, "SO-2");
    decide(
        &mut cell,
        &catalog,
        "t1",
        &[],
        &[create(ORDER, "SO-1", 5.0), create(ORDER, "SO-2", 5.0)],
    )
    .unwrap();

    // Decision A reads SO-1 and SO-2 and submits SO-1.
    let slice_a = load_slice(&cell, [so1.clone(), so2.clone()]).unwrap();
    let submit = output(
        json!({ "facts": [transition(ORDER, "SO-1", "submit")], "commands": [{ "kind": "n" }], "timers": [] }),
    );
    let prepared = prepare_decision::<Infallible>(&catalog, &slice_a, &turn("a"), &submit).unwrap();
    assert_eq!(
        prepared.reads,
        vec![FactExpectation {
            key: so2.clone(),
            expected: FactVersion(1)
        }],
        "objects read but not written are compare-and-swap reads"
    );

    // Decision B commits first and changes SO-2, which A only read.
    decide(
        &mut cell,
        &catalog,
        "b",
        std::slice::from_ref(&so2),
        &[set(ORDER, "SO-2", "amount", &json!(9))],
    )
    .unwrap();
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &slice_a,
        &turn("a"),
        &submit,
        "decision.stale_state",
    );
    assert_eq!(
        refusal,
        DecisionRefusal::Stale {
            key: so2,
            expected: FactVersion(1),
            current: FactVersion(2)
        }
    );

    // Two decisions racing on the same object: the second loses.
    let slice = load_slice(&cell, [so1.clone()]).unwrap();
    let cancel = output(
        json!({ "facts": [transition(ORDER, "SO-1", "cancel")], "commands": [], "timers": [] }),
    );
    commit_decision(&mut cell, &catalog, &slice, &turn("c"), &submit).unwrap();
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("d"),
        &cancel,
        "decision.stale_state",
    );
    assert_eq!(
        status(&cell, &so1),
        Some(JsonValue::String("submitted".into()))
    );
}

#[test]
fn decision_consumed_delivery_is_refused() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let timer = output(
        json!({ "facts": [], "commands": [], "timers": [{ "afterMillis": 0, "timerId": "tick" }] }),
    );
    commit_decision(&mut cell, &catalog, &FactSlice::new(), &turn("t1"), &timer).unwrap();
    cell.advance_to(LogicalTime::from_millis(1_000));
    let due = cell.clock().unwrap().unwrap();
    let mut fired = turn("t2");
    fired.consumes = Some(due.consumed());
    let empty = output(json!({ "facts": [], "commands": [], "timers": [] }));
    commit_decision(&mut cell, &catalog, &FactSlice::new(), &fired, &empty).unwrap();
    // A redelivered (duplicate) turn for the same timer commits nothing.
    assert_refused(
        &mut cell,
        &catalog,
        &FactSlice::new(),
        &fired,
        &empty,
        "decision.not_pending",
    );
    assert!(matches!(fired.consumes, Some(Consumed::Timer { .. })));
}

/// Two decisions prepared on overlapping slices and committed in order: the
/// second commits exactly when the first wrote nothing it read, and a refused
/// second leaves the state the first produced.
#[test]
fn decision_race_property() {
    let types = types();
    let keys = [key(ORDER, "SO-0"), key(ORDER, "SO-1")];
    let strategy = (
        prop::sample::subsequence(vec![0_usize, 1], 0..=2),
        prop::sample::subsequence(vec![0_usize, 1], 0..=2),
        prop::sample::subsequence(vec![0_usize, 1], 0..=2),
        prop::sample::subsequence(vec![0_usize, 1], 0..=2),
    );
    norn_testkit::check_property(
        &Settings::new(file!()),
        &strategy,
        |(read_a, write_a, read_b, write_b)| {
            let catalog = TypeCatalog::new(&types);
            let mut cell = cell();
            decide(
                &mut cell,
                &catalog,
                "seed",
                &[],
                &[create(ORDER, "SO-0", 1.0), create(ORDER, "SO-1", 1.0)],
            )
            .unwrap();
            // Each decision reads its read set plus its write set.
            let slice_of = |reads: &[usize], writes: &[usize], cell: &Cell| {
                let mut wanted: Vec<FactKey> = reads
                    .iter()
                    .chain(writes)
                    .map(|&n| keys[n].clone())
                    .collect();
                wanted.sort();
                wanted.dedup();
                load_slice(cell, wanted).unwrap()
            };
            let facts = |writes: &[usize], value: i64| -> Vec<serde_json::Value> {
                writes
                    .iter()
                    .map(|&n| set(ORDER, &format!("SO-{n}"), "amount", &json!(value)))
                    .collect()
            };
            let slice_a = slice_of(&read_a, &write_a, &cell);
            let slice_b = slice_of(&read_b, &write_b, &cell);
            let out_a =
                output(json!({ "facts": facts(&write_a, 10), "commands": [], "timers": [] }));
            let out_b =
                output(json!({ "facts": facts(&write_b, 20), "commands": [], "timers": [] }));

            commit_decision(&mut cell, &catalog, &slice_a, &turn("a"), &out_a).unwrap();
            let after_a = visible(&cell, &keys);
            let conflict = write_a.iter().any(|n| slice_b.contains_key(&keys[*n]));
            match commit_decision(&mut cell, &catalog, &slice_b, &turn("b"), &out_b) {
                Ok(_) => prop_assert!(!conflict, "B committed over A's write to its slice"),
                Err(refusal) => {
                    prop_assert!(conflict, "B refused without a conflict: {}", refusal);
                    prop_assert_eq!(refusal.code(), "decision.stale_state");
                    prop_assert_eq!(visible(&cell, &keys), after_a);
                }
            }
            Ok(())
        },
    );
}

// ------------------------------------------- correspondence with the checker

/// `fixtures/ir/action-firing/*.case.json` (#2876): each case's
/// `fire_transition` call, committed as a `transition` fact change on an
/// object holding the case's values, is accepted exactly when
/// `norn_checker::check_action_firing` has no finding, and otherwise refused
/// with the finding's code.
#[test]
fn decision_commit_corresponds_to_checker_action_firing() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let read = |path: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let mut cases: Vec<_> = std::fs::read_dir(fixtures.join("ir/action-firing"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".case.json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 3, "action-firing cases missing");
    for path in cases {
        let case = read(&path);
        let artifacts: Vec<serde_json::Value> = case["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|artifact| read(&fixtures.join(artifact.as_str().unwrap())))
            .collect();
        let types: Vec<ObjectTypeDef> = artifacts
            .iter()
            .filter(|artifact| artifact["kind"] == "entity")
            .map(|artifact| {
                serde_json::from_value(artifact["payload"]["objectType"].clone()).unwrap()
            })
            .collect();
        let action = artifacts
            .iter()
            .find(|artifact| artifact["artifactId"] == case["action"])
            .unwrap();
        let effect = &action["payload"]["effectProgram"];
        assert_eq!(effect["kind"], "fire_transition");
        let object_type = effect["objectKind"].as_str().unwrap();
        let values: JsonObject = serde_json::from_value(case["object"]["values"].clone()).unwrap();
        let id = case["object"]["values"][effect["objectIdField"].as_str().unwrap()]
            .as_str()
            .unwrap()
            .to_owned();

        let catalog = TypeCatalog::new(&types);
        let mut cell = cell();
        let object = key(object_type, &id);
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: object.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put { values },
            }],
            ..CellCommit::empty()
        })
        .unwrap();
        let slice = load_slice(&cell, [object]).unwrap();
        let lifecycle = effect["lifecycle"].clone();
        let transition_id = effect["transition"].clone();
        let fire = output(json!({
            "facts": [{
                "op": "transition", "objectType": object_type, "key": id,
                "lifecycle": lifecycle, "transition": transition_id
            }],
            "commands": [], "timers": []
        }));
        let result = commit_decision(&mut cell, &catalog, &slice, &turn("call"), &fire);
        let expected: Vec<String> = case["expect"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| finding[0].as_str().unwrap().to_owned())
            .collect();
        match (result, expected.as_slice()) {
            (Ok(_), []) => {}
            (Err(refusal), [code]) => {
                assert_eq!(refusal.code(), code, "{}: {refusal}", path.display());
            }
            (result, expected) => panic!(
                "{}: commit {result:?} does not correspond to checker findings {expected:?}",
                path.display()
            ),
        }
    }
}

#[test]
fn decision_effective_lifecycle_follows_subtype_enum_narrowing() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let quick = key(QUICK, "Q-1");

    // The nearest declaration narrows the enum but inherits the base type's
    // default. Creation therefore persists the initial lifecycle state.
    decide(
        &mut cell,
        &catalog,
        "t1-omitted-status",
        &[],
        &[create(QUICK, "Q-1", 5.0)],
    )
    .unwrap();
    assert_eq!(
        status(&cell, &quick),
        Some(JsonValue::String("draft".to_owned()))
    );
    decide(
        &mut cell,
        &catalog,
        "t2",
        std::slice::from_ref(&quick),
        &[transition(QUICK, "Q-1", "submit")],
    )
    .unwrap();
    let slice = load_slice(&cell, [quick]).unwrap();
    let approve = output(
        json!({ "facts": [transition(QUICK, "Q-1", "approve")], "commands": [], "timers": [] }),
    );
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t3"),
        &approve,
        "artifact_set.lifecycle.unknown_transition",
    );
}

#[test]
fn decision_missing_state_is_an_undeclared_write_before_the_transition_id() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let stateless = key(ORDER, "SO-X");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: stateless.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: record(json!({ "order_no": "SO-X", "amount": 5 })),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    let slice = load_slice(&cell, [stateless]).unwrap();
    for name in ["ship", "submit"] {
        let fire = output(
            json!({ "facts": [transition(ORDER, "SO-X", name)], "commands": [], "timers": [] }),
        );
        assert_refused(
            &mut cell,
            &catalog,
            &slice,
            &turn("t2"),
            &fire,
            "lifecycle.validation.undeclared_write",
        );
    }
}

// ------------------------------- extension properties and fail-closed lifecycles

fn ticket_types() -> (Vec<ObjectTypeDef>, norn_ir::ObjectTypeExtension) {
    let base = json!({
        "name": "commerce.Ticket",
        "properties": [
            { "id": "title", "type": { "kind": "text" } },
            { "id": "tier", "type": { "kind": "ref", "name": "commerce.tier" } }
        ]
    });
    // A lifecycle over an extension-added property.
    let sub = json!({
        "name": "commerce.AcmeTicket",
        "extends": "commerce.Ticket",
        "lifecycles": [{
            "property": "acme__stage",
            "initial": ["open"],
            "terminal": ["closed"],
            "transitions": [
                { "id": "triage", "from": ["open"], "to": "triaged" },
                { "id": "close", "from": ["open", "triaged"], "to": "closed", "guards": ["self.title != \"\""] }
            ]
        }]
    });
    // A lifecycle whose states come from a shared property type the kernel
    // does not resolve: enforced fail-closed.
    let tiered = json!({
        "name": "commerce.TieredTicket",
        "extends": "commerce.Ticket",
        "lifecycles": [{
            "property": "tier",
            "initial": ["bronze"],
            "transitions": [{ "id": "upgrade", "from": ["bronze"], "to": "gold" }]
        }]
    });
    let extension = json!({
        "namespace": "acme",
        "target": "commerce.Ticket",
        "properties": [{
            "id": "acme__stage",
            "type": { "kind": "enum", "values": [
                { "value": "open" }, { "value": "triaged" }, { "value": "closed" }
            ] },
            "default": "open"
        }]
    });
    (
        vec![
            serde_json::from_value(base).unwrap(),
            serde_json::from_value(sub).unwrap(),
            serde_json::from_value(tiered).unwrap(),
        ],
        serde_json::from_value(extension).unwrap(),
    )
}

#[test]
fn decision_lifecycle_over_extension_property_is_enforced() {
    let (types, extension) = ticket_types();
    let catalog = TypeCatalog::new(&types).with_extensions([&extension]);
    let mut cell = cell();
    let ticket = key("commerce.AcmeTicket", "T-1");
    let create = json!({
        "op": "create", "objectType": "commerce.AcmeTicket", "key": "T-1",
        "values": { "title": "printer" }
    });
    decide(&mut cell, &catalog, "t1", &[], &[create]).unwrap();
    assert_eq!(
        cell.load(&ticket)
            .unwrap()
            .values
            .unwrap()
            .get("acme__stage"),
        Some(&JsonValue::String("open".into())),
        "the extension property's default is the initial state"
    );
    let slice = load_slice(&cell, [ticket]).unwrap();
    let direct = output(json!({
        "facts": [set("commerce.AcmeTicket", "T-1", "acme__stage", &json!("closed"))],
        "commands": [], "timers": []
    }));
    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("t2"),
        &direct,
        "artifact_set.lifecycle.direct_write",
    );
    let triage = output(json!({
        "facts": [transition_of("commerce.AcmeTicket", "T-1", "acme__stage", "triage")],
        "commands": [], "timers": []
    }));
    commit_decision(&mut cell, &catalog, &slice, &turn("t2"), &triage).unwrap();
}

#[test]
fn effective_object_values_use_nearest_declarations_and_extension_defaults() {
    let base: ObjectTypeDef = serde_json::from_value(json!({
        "name": "effective.Base",
        "properties": [{
            "id": "amount",
            "type": { "kind": "integer" },
            "default": "-1"
        }]
    }))
    .unwrap();
    let subtype: ObjectTypeDef = serde_json::from_value(json!({
        "name": "effective.Sub",
        "extends": "effective.Base",
        "properties": [{ "id": "amount", "type": { "kind": "integer" } }]
    }))
    .unwrap();
    let extension: norn_ir::ObjectTypeExtension = serde_json::from_value(json!({
        "namespace": "acme",
        "target": "effective.Base",
        "properties": [{
            "id": "acme__level",
            "type": { "kind": "text" },
            "default": "gold"
        }]
    }))
    .unwrap();
    let types = [base, subtype];
    let catalog = TypeCatalog::new(&types).with_extensions([&extension]);
    let values = catalog
        .effective_object_values(
            &norn_ir::ObjectTypeName("effective.Sub".to_owned()),
            &JsonObject::new(),
        )
        .unwrap();
    assert_eq!(
        values.get("amount"),
        Some(&JsonValue::Number(norn_ir::Decimal::parse("-1").unwrap()))
    );
    assert_eq!(
        values.get("acme__level"),
        Some(&JsonValue::String("gold".to_owned()))
    );
}

#[test]
fn effective_temporal_defaults_normalize_through_inheritance_refs_and_extensions() {
    let base: ObjectTypeDef = serde_json::from_value(json!({
        "name": "effective.TemporalBase",
        "properties": [
            {
                "id": "inherited_at",
                "type": { "kind": "timestamp", "timezone": "utc" },
                "default": "2024-02-29T12:30:00+02:30"
            },
            {
                "id": "shared_at",
                "type": { "kind": "ref", "name": "shared.instant" },
                "default": "2024-02-29T12:30:00+02:30"
            }
        ]
    }))
    .unwrap();
    let subtype: ObjectTypeDef = serde_json::from_value(json!({
        "name": "effective.TemporalSub",
        "extends": "effective.TemporalBase"
    }))
    .unwrap();
    let property_type: PropertyTypeDef = serde_json::from_value(json!({
        "name": "shared.instant",
        "type": { "kind": "timestamp", "timezone": "offset" }
    }))
    .unwrap();
    let extension: ObjectTypeExtension = serde_json::from_value(json!({
        "namespace": "acme",
        "target": "effective.TemporalBase",
        "properties": [{
            "id": "acme__occurred_at",
            "type": { "kind": "timestamp", "timezone": "offset" },
            "default": "2024-02-29T12:30:00+02:30"
        }]
    }))
    .unwrap();
    let types = [base, subtype];
    let property_types = [property_type];
    let catalog = TypeCatalog::new(&types)
        .with_extensions([&extension])
        .with_property_types(&property_types);
    let values = catalog
        .effective_object_values(
            &norn_ir::ObjectTypeName("effective.TemporalSub".to_owned()),
            &JsonObject::new(),
        )
        .unwrap();
    let canonical = RuntimeValue::String("2024-02-29T10:00:00Z".to_owned());
    for property in ["inherited_at", "shared_at", "acme__occurred_at"] {
        assert_eq!(values.get(property), Some(&canonical), "{property}");
    }
}

#[test]
fn effective_link_values_apply_defaults_and_validate_temporal_values() {
    let source: ObjectTypeDef = serde_json::from_value(json!({
        "name": "effective.LinkSource",
        "links": [{
            "id": "events",
            "target": "effective.LinkTarget",
            "cardinality": { "to": "many", "from": "many" },
            "properties": [
                {
                    "id": "occurred_at",
                    "type": { "kind": "timestamp", "timezone": "offset" },
                    "default": "2024-02-29T12:30:00+02:30"
                },
                {
                    "id": "on_date",
                    "type": { "kind": "date" },
                    "default": "2024-02-29"
                },
                {
                    "id": "optional_at",
                    "type": { "kind": "timestamp", "timezone": "offset" },
                    "constraints": { "nullable": true }
                }
            ]
        }]
    }))
    .unwrap();
    let target: ObjectTypeDef =
        serde_json::from_value(json!({ "name": "effective.LinkTarget" })).unwrap();
    let types = [source, target];
    let catalog = TypeCatalog::new(&types);
    let source_name = norn_ir::ObjectTypeName("effective.LinkSource".to_owned());
    let values = catalog
        .effective_link_values(&source_name, "events", &JsonObject::new())
        .unwrap();
    assert_eq!(
        values.get("occurred_at"),
        Some(&RuntimeValue::String("2024-02-29T10:00:00Z".to_owned()))
    );
    assert_eq!(
        values.get("on_date"),
        Some(&RuntimeValue::String("2024-02-29".to_owned()))
    );
    assert_eq!(values.get("optional_at"), Some(&RuntimeValue::Null));

    let submitted = record(json!({
        "occurred_at": "2024-02-29T11:00:00+01:00",
        "on_date": "2024-02-28"
    }));
    let values = catalog
        .effective_link_values(&source_name, "events", &submitted)
        .unwrap();
    assert_eq!(
        values.get("occurred_at"),
        Some(&RuntimeValue::String("2024-02-29T10:00:00Z".to_owned()))
    );
    assert_eq!(
        values.get("on_date"),
        Some(&RuntimeValue::String("2024-02-28".to_owned()))
    );

    let invalid_value = record(json!({ "occurred_at": "not-a-timestamp" }));
    assert!(matches!(
        catalog.effective_link_values(&source_name, "events", &invalid_value),
        Err(EffectiveValuesError::ValueType {
            property,
            expected: "valid timestamp"
        }) if property == "occurred_at"
    ));

    let invalid_date = record(json!({ "on_date": "2024-02-30" }));
    assert!(matches!(
        catalog.effective_link_values(&source_name, "events", &invalid_date),
        Err(EffectiveValuesError::ValueType {
            property,
            expected: "valid date"
        }) if property == "on_date"
    ));

    let invalid_default: ObjectTypeDef = serde_json::from_value(json!({
        "name": "effective.InvalidLinkSource",
        "links": [{
            "id": "events",
            "target": "effective.LinkTarget",
            "cardinality": { "to": "many", "from": "many" },
            "properties": [{
                "id": "occurred_at",
                "type": { "kind": "timestamp", "timezone": "offset" },
                "default": "not-a-timestamp"
            }]
        }]
    }))
    .unwrap();
    let invalid_types = [invalid_default, types[1].clone()];
    let invalid_catalog = TypeCatalog::new(&invalid_types);
    let invalid_source_name = norn_ir::ObjectTypeName("effective.InvalidLinkSource".to_owned());
    assert!(matches!(
        invalid_catalog.effective_link_values(
            &invalid_source_name,
            "events",
            &JsonObject::new()
        ),
        Err(EffectiveValuesError::ValueType {
            property,
            expected: "valid timestamp"
        }) if property == "occurred_at"
    ));
}

#[test]
fn decision_unresolved_lifecycle_fails_closed() {
    let (types, extension) = ticket_types();
    let catalog = TypeCatalog::new(&types).with_extensions([&extension]);
    let mut cell = cell();
    let ticket = key("commerce.TieredTicket", "T-2");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: ticket.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: record(json!({ "title": "x", "tier": "bronze" })),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    let slice = load_slice(&cell, [ticket]).unwrap();
    let cases = [
        (
            set("commerce.TieredTicket", "T-2", "tier", &json!("gold")),
            "artifact_set.lifecycle.direct_write",
        ),
        (
            transition_of("commerce.TieredTicket", "T-2", "tier", "upgrade"),
            "decision.unresolved_lifecycle",
        ),
        (
            json!({ "op": "create", "objectType": "commerce.TieredTicket", "key": "T-3", "values": { "tier": "bronze" } }),
            "decision.unresolved_lifecycle",
        ),
    ];
    for (fact, code) in cases {
        let refused = output(json!({ "facts": [fact], "commands": [], "timers": [] }));
        assert_refused(&mut cell, &catalog, &slice, &turn("t2"), &refused, code);
    }

    // Without the extension, the extension lifecycle is unresolved too: a
    // direct write of its state is still refused.
    let bare = TypeCatalog::new(&types);
    let direct = output(json!({
        "facts": [{ "op": "create", "objectType": "commerce.AcmeTicket", "key": "T-9",
                    "values": { "acme__stage": "closed" } }],
        "commands": [], "timers": []
    }));
    assert_refused(
        &mut cell,
        &bare,
        &FactSlice::new(),
        &turn("t3"),
        &direct,
        "decision.unresolved_lifecycle",
    );
}

fn transition_of(object_type: &str, id: &str, lifecycle: &str, name: &str) -> serde_json::Value {
    json!({
        "op": "transition", "objectType": object_type, "key": id,
        "lifecycle": lifecycle, "transition": name
    })
}

/// #3079: a slice that lists many objects (a list entry's targets) makes every
/// one of them writable in one commit, and a change to any one of them
/// between the read and the commit refuses the whole turn.
#[test]
fn decision_over_a_listed_slice_writes_every_member_or_none() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let lines: Vec<FactKey> = (1..=3)
        .map(|index| key(ORDER, &format!("L-{index}")))
        .collect();
    decide(
        &mut cell,
        &catalog,
        "seed",
        &[],
        &(1..=3)
            .map(|index| create(ORDER, &format!("L-{index}"), 5.0))
            .collect::<Vec<_>>(),
    )
    .unwrap();

    // One decision reads all three and patches all three, and creates one
    // row per member, atomically.
    let patch_all = |amount: i64| {
        let mut facts: Vec<serde_json::Value> = (1..=3)
            .map(|index| set(ORDER, &format!("L-{index}"), "amount", &json!(amount)))
            .collect();
        facts.extend((1..=3).map(|index| create(ORDER, &format!("ROW-{amount}-{index}"), 1.0)));
        output(json!({ "facts": facts, "commands": [], "timers": [] }))
    };
    let slice = load_slice(&cell, lines.iter().cloned()).unwrap();
    let prepared =
        prepare_decision::<Infallible>(&catalog, &slice, &turn("all"), &patch_all(7)).unwrap();
    assert_eq!(
        prepared.writes.len(),
        6,
        "three patches and three rows in one commit"
    );
    assert!(prepared.reads.is_empty(), "every listed object is written");
    commit_decision(&mut cell, &catalog, &slice, &turn("all"), &patch_all(7)).unwrap();
    for line in &lines {
        let values = cell.load(line).unwrap().values.unwrap();
        assert_eq!(values.get("amount"), Some(&JsonValue::from(7)));
    }

    // A second decision reads the list; another turn changes the middle
    // member first; the second decision is refused and writes nothing.
    let stale = load_slice(&cell, lines.iter().cloned()).unwrap();
    decide(
        &mut cell,
        &catalog,
        "other",
        std::slice::from_ref(&lines[1]),
        &[set(ORDER, "L-2", "amount", &json!(8))],
    )
    .unwrap();
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &stale,
        &turn("late"),
        &patch_all(9),
        "decision.stale_state",
    );
    assert!(
        matches!(refusal, DecisionRefusal::Stale { ref key, .. } if *key == lines[1]),
        "{refusal}"
    );
    assert!(cell.load(&key(ORDER, "ROW-9-1")).unwrap().values.is_none());
    assert_eq!(
        cell.load(&lines[0]).unwrap().values.unwrap().get("amount"),
        Some(&JsonValue::from(7))
    );
}

/// #3079: a list entry reads a linked subtype object as its concrete type,
/// while the decision names the link's declared (super)type. The kernel
/// resolves the write to the object it read, so `set` and `transition`
/// commit under the read version, and nothing is refused as not in the
/// slice.
#[test]
fn decision_writes_naming_the_declared_type_reach_a_subtype_it_read() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    decide(&mut cell, &catalog, "seed", &[], &[create(BIG, "B-1", 5.0)]).unwrap();
    let concrete = key(BIG, "B-1");
    let slice = load_slice(&cell, [concrete.clone()]).unwrap();
    let output = output(json!({
        "facts": [set(ORDER, "B-1", "amount", &json!(6)), transition(ORDER, "B-1", "submit")],
        "commands": [],
        "timers": []
    }));
    let prepared = prepare_decision::<Infallible>(&catalog, &slice, &turn("sub"), &output).unwrap();
    assert_eq!(prepared.writes.len(), 1);
    assert_eq!(
        prepared.writes[0].key, concrete,
        "the write goes to the object read"
    );
    assert_eq!(prepared.writes[0].expected, FactVersion(1));
    commit_decision(&mut cell, &catalog, &slice, &turn("sub"), &output).unwrap();
    assert_eq!(
        status(&cell, &concrete),
        Some(JsonValue::String("submitted".into()))
    );
    assert_eq!(
        cell.load(&concrete).unwrap().values.unwrap().get("amount"),
        Some(&JsonValue::from(6))
    );

    // A write naming the declared type of an object the decision did not
    // read is still refused.
    let unread = output_of_set(ORDER, "B-2");
    let refusal =
        prepare_decision::<Infallible>(&catalog, &slice, &turn("unread"), &unread).unwrap_err();
    assert_eq!(refusal.code(), "decision.not_in_slice", "{refusal}");
}

fn output_of_set(object_type: &str, id: &str) -> DecisionOutput {
    output(json!({
        "facts": [set(object_type, id, "amount", &json!(1))],
        "commands": [],
        "timers": []
    }))
}

/// A newly created subtype in the working copy is not a read-slice alias
/// target for a later write naming its supertype.
#[test]
fn a_new_subtype_does_not_authorize_a_base_typed_write() {
    for (id, base_change) in [
        ("CREATE-SET", set(ORDER, "CREATE-SET", "amount", &json!(6))),
        (
            "CREATE-TRANSITION",
            transition(ORDER, "CREATE-TRANSITION", "submit"),
        ),
    ] {
        let types = types();
        let catalog = TypeCatalog::new(&types);
        let mut cell = cell();
        let slice = FactSlice::new();
        let output = output(json!({
            "facts": [create(BIG, id, 5.0), base_change],
            "commands": [],
            "timers": []
        }));

        assert_refused(
            &mut cell,
            &catalog,
            &slice,
            &turn("unread-subtype"),
            &output,
            "decision.not_in_slice",
        );
        assert!(cell.load(&key(BIG, id)).unwrap().values.is_none());
        assert!(cell.load(&key(ORDER, id)).unwrap().values.is_none());
    }
}

/// An absent subtype read stays non-authoritative after a subtype is created
/// during the same decision.
#[test]
fn an_absent_subtype_read_does_not_alias_a_later_create() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let slice = load_slice(&cell, [key(BIG, "ABSENT-THEN-CREATE")]).unwrap();
    let output = output(json!({
        "facts": [
            create(BIG, "ABSENT-THEN-CREATE", 5.0),
            set(ORDER, "ABSENT-THEN-CREATE", "amount", &json!(6))
        ],
        "commands": [],
        "timers": []
    }));

    assert_refused(
        &mut cell,
        &catalog,
        &slice,
        &turn("absent-read"),
        &output,
        "decision.not_in_slice",
    );
    assert!(
        cell.load(&key(BIG, "ABSENT-THEN-CREATE"))
            .unwrap()
            .values
            .is_none()
    );
}

/// Exact writes to a subtype created earlier in this decision keep using that
/// identity through the working copy.
#[test]
fn a_subtype_created_in_working_accepts_exact_set_and_transition() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = cell();
    let slice = FactSlice::new();
    let concrete = key(BIG, "EXACT-CREATE");
    let output = output(json!({
        "facts": [
            create(BIG, "EXACT-CREATE", 5.0),
            set(BIG, "EXACT-CREATE", "amount", &json!(6)),
            transition(BIG, "EXACT-CREATE", "submit")
        ],
        "commands": [],
        "timers": []
    }));

    let prepared =
        prepare_decision::<Infallible>(&catalog, &slice, &turn("exact"), &output).unwrap();
    assert_eq!(prepared.writes.len(), 1);
    assert_eq!(prepared.writes[0].key, concrete);
    assert_eq!(prepared.writes[0].expected, FactVersion::ABSENT);
    commit_decision(&mut cell, &catalog, &slice, &turn("exact"), &output).unwrap();
    assert_eq!(
        status(&cell, &concrete),
        Some(JsonValue::String("submitted".into()))
    );
    assert_eq!(
        cell.load(&concrete).unwrap().values.unwrap().get("amount"),
        Some(&JsonValue::from(6))
    );
}

/// #3079 review: a create naming the declared type is never redirected to an
/// absent subtype the decision read; it creates exactly what it names.
#[test]
fn a_create_is_not_redirected_through_an_absent_subtype_read() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let cell = cell();
    let slice = load_slice(&cell, [key(BIG, "X-1")]).unwrap();
    let output = output(json!({
        "facts": [create(ORDER, "X-1", 5.0)],
        "commands": [],
        "timers": []
    }));
    let prepared = prepare_decision::<Infallible>(&catalog, &slice, &turn("x"), &output).unwrap();
    assert!(
        prepared
            .writes
            .iter()
            .any(|write| write.key == key(ORDER, "X-1")),
        "{:?}",
        prepared.writes
    );
    assert!(
        prepared
            .writes
            .iter()
            .all(|write| write.key.object_type.as_str() != BIG)
    );
}

#[test]
fn lean_l1_lifecycle_corpus_matches_real_commit() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let corpus = include_str!("../../../verification/kernel-l1/corpus.tsv");
    let mut count = 0;
    for line in corpus
        .lines()
        .filter(|line| line.starts_with("lifecycle\t"))
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0], "lifecycle");
        let amount: i64 = fields[2].parse().unwrap();
        let accepted: bool = fields[3].parse().unwrap();
        let mut cell = cell();
        let object = key(ORDER, "lean");
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: object.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: record(json!({
                        "order_no": "lean", "amount": amount, "approver": "",
                        "status": fields[1]
                    })),
                },
            }],
            ..CellCommit::empty()
        })
        .unwrap();
        let before = visible(&cell, std::slice::from_ref(&object));
        let result = decide(
            &mut cell,
            &catalog,
            "lean-submit",
            std::slice::from_ref(&object),
            &[transition(ORDER, "lean", "submit")],
        );
        assert_eq!(result.is_ok(), accepted, "{line}: {result:?}");
        if accepted {
            assert_eq!(
                status(&cell, &object),
                Some(JsonValue::String("submitted".into()))
            );
        } else {
            assert_eq!(
                visible(&cell, std::slice::from_ref(&object)),
                before,
                "{line}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 20);
}
