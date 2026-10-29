#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::result_large_err,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
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

//! The decide → commit seam (T3-DEC-2 #2670 with T2-KER-2b #2535): a `RuleIR`
//! v2 JDM decision evaluates through `norn-decisions`, and its canonical
//! `norn_ir::DecisionOutput` commits through `norn_kernel::commit_decision`
//! against the reference [`MemoryCell`]. The facts land, with their ledger
//! events, commands, and timers, or a typed refusal comes back and nothing is
//! committed.

use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate};
use norn_expr::{EvaluationBudget, Number, Value};
use norn_ir::{ObjectTypeDef, RuleRoot};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{
    CellHost, Command, DecisionRefusal, DecisionTurn, FactKey, LogicalTime, TypeCatalog, World,
    commit_decision, load_slice,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<norn_kernel::FactEvent, JsonObject, JsonObject>;

const ORDER: &str = "demo.Order";

fn types() -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": ORDER,
            "properties": [
                { "id": "order_no", "type": { "kind": "text" } },
                { "id": "amount", "type": { "kind": "integer" } },
                {
                    "id": "status",
                    "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "submitted" }] },
                    "default": "draft"
                }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["submitted"],
                "transitions": [
                    { "id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.amount > 0"] }
                ]
            }]
        }))
        .unwrap(),
    ]
}

fn closed(properties: &serde_json::Value, required: &[&str]) -> serde_json::Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

/// `open` creates an order and schedules a reminder; `submit` fires the
/// guarded `submit` transition and sends a notification.
fn rule() -> RuleRoot {
    let text = json!({ "type": "string" });
    let integer = json!({ "type": "integer" });
    let fact = closed(
        &json!({
            "op": { "type": "string", "enum": ["create", "transition"] },
            "objectType": { "type": "string", "enum": [ORDER] },
            "key": text,
            "values": closed(&json!({ "order_no": text, "amount": integer }), &["order_no", "amount"]),
            "lifecycle": { "type": "string", "enum": ["status"] },
            "transition": { "type": "string", "enum": ["submit"] }
        }),
        &["op", "objectType", "key"],
    );
    let facts = "event.kind == 'open' \
        ? [{op: 'create', objectType: 'demo.Order', key: event.key, values: {order_no: event.key, amount: event.amount}}] \
        : [{op: 'transition', objectType: 'demo.Order', key: event.key, lifecycle: 'status', transition: 'submit'}]";
    serde_json::from_value(json!({
        "ruleId": "demo.rule.order_flow",
        "ruleVersion": "2.0",
        "contract": "decide",
        "inputSchema": closed(&json!({
            "slice": closed(&json!({ "status": text }), &[]),
            "event": closed(&json!({ "kind": text, "key": text, "amount": integer }), &["kind", "key", "amount"])
        }), &["slice", "event"]),
        "outputSchema": closed(&json!({
            "facts": { "type": "array", "items": fact },
            "commands": { "type": "array", "items": closed(&json!({ "kind": text, "order": text }), &["kind", "order"]) },
            "timers": { "type": "array", "items": closed(&json!({ "afterMillis": integer, "kind": text }), &["afterMillis", "kind"]) }
        }), &["facts", "commands", "timers"]),
        "scopeRequired": [],
        "graph": {
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode" },
                { "id": "x", "name": "x", "type": "expressionNode", "content": { "expressions": [
                    { "id": "f", "key": "facts", "value": facts },
                    { "id": "c", "key": "commands", "value": "event.kind == 'submit' ? [{kind: 'notify', order: event.key}] : []" },
                    { "id": "t", "key": "timers", "value": "event.kind == 'open' ? [{afterMillis: 60000, kind: 'remind'}] : []" }
                ] } },
                { "id": "out", "name": "out", "type": "outputNode" }
            ],
            "edges": [
                { "id": "e1", "sourceId": "in", "targetId": "x" },
                { "id": "e2", "sourceId": "x", "targetId": "out" }
            ]
        }
    }))
    .unwrap()
}

struct Harness {
    decision: AdmittedDecision,
    types: Vec<ObjectTypeDef>,
    cell: Cell,
    turns: u64,
}

impl Harness {
    fn new() -> Self {
        let binding = DecisionBinding {
            profile_id: "norn-zen/6".to_owned(),
            environment_hash: "sha256:test".to_owned(),
        };
        Self {
            decision: AdmittedDecision::admit(&rule(), &BTreeMap::new(), binding).unwrap(),
            types: types(),
            cell: MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone()),
            turns: 0,
        }
    }

    /// Decides `kind` for order `key` (the slice is the order's current
    /// status) and commits the output in one turn.
    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn turn(
        &mut self,
        kind: &str,
        key: &str,
        amount: i64,
    ) -> Result<(), DecisionRefusal<Infallible>> {
        let fact = FactKey::new(ORDER, key);
        let slice = load_slice(&self.cell, [fact.clone()]).unwrap();
        let status = slice[&fact]
            .values
            .as_ref()
            .and_then(|values| values.get("status"))
            .and_then(JsonValue::as_str)
            .map(|status| ("status", Value::from(status)));
        let slice_value = Value::object(status);
        let event = Value::object([
            ("kind", Value::from(kind)),
            ("key", Value::from(key)),
            ("amount", Value::Number(Number::from(amount))),
        ]);
        let evaluated = evaluate(
            &self.decision,
            &self.decision.validate_slice(slice_value).unwrap(),
            &self.decision.type_event(event).unwrap(),
            EvaluationBudget::default(),
        )
        .unwrap();
        self.turns += 1;
        let turn = DecisionTurn {
            id: format!("t{}", self.turns),
            now: LogicalTime::from_millis(1_000),
            world: World::Main,
            consumes: None,
        };
        let catalog = TypeCatalog::new(&self.types);
        commit_decision(&mut self.cell, &catalog, &slice, &turn, &evaluated.output).map(|_| ())
    }

    fn status(&self, key: &str) -> Option<JsonValue> {
        self.cell
            .load(&FactKey::new(ORDER, key))
            .unwrap()
            .values
            .and_then(|values| values.get("status").cloned())
    }

    fn visible(&self) -> (usize, usize, usize) {
        (
            self.cell.ledger().len(),
            self.cell.outbox().len(),
            self.cell.timers().len(),
        )
    }
}

#[test]
fn decided_facts_commands_and_timers_land_in_one_commit() {
    let mut harness = Harness::new();
    harness.turn("open", "SO-1", 5).unwrap();
    assert_eq!(
        harness.status("SO-1"),
        Some(JsonValue::String("draft".into()))
    );
    let amount = harness
        .cell
        .load(&FactKey::new(ORDER, "SO-1"))
        .unwrap()
        .values
        .unwrap()["amount"]
        .clone();
    assert_eq!(amount, JsonValue::from(5));
    assert_eq!(
        harness.cell.timers().len(),
        1,
        "the reminder timer is scheduled"
    );

    harness.turn("submit", "SO-1", 5).unwrap();
    assert_eq!(
        harness.status("SO-1"),
        Some(JsonValue::String("submitted".into()))
    );
    assert_eq!(
        harness.cell.outbox().len(),
        1,
        "the notification is enqueued"
    );
    assert_eq!(
        harness.cell.outbox()[0].payload,
        serde_json::from_value::<JsonObject>(json!({ "kind": "notify", "order": "SO-1" })).unwrap()
    );
}

#[test]
fn a_stale_from_state_is_a_typed_refusal_with_nothing_committed() {
    let mut harness = Harness::new();
    harness.turn("open", "SO-1", 5).unwrap();
    harness.turn("submit", "SO-1", 5).unwrap();
    let before = harness.visible();
    let refusal = harness.turn("submit", "SO-1", 5).unwrap_err();
    assert!(
        matches!(refusal, DecisionRefusal::NotFromState { .. }),
        "{refusal}"
    );
    assert_eq!(harness.visible(), before);
    assert_eq!(
        harness.status("SO-1"),
        Some(JsonValue::String("submitted".into()))
    );
}

#[test]
fn a_false_lifecycle_guard_is_a_typed_refusal_with_nothing_committed() {
    let mut harness = Harness::new();
    harness.turn("open", "SO-2", 0).unwrap();
    let before = harness.visible();
    let refusal = harness.turn("submit", "SO-2", 0).unwrap_err();
    assert_eq!(refusal.code(), "decision.guard_false", "{refusal}");
    assert_eq!(harness.visible(), before);
    assert_eq!(
        harness.status("SO-2"),
        Some(JsonValue::String("draft".into()))
    );
}
