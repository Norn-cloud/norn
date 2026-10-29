#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]
//! `evaluate_explained` reports the decision-table rows that matched; the
//! output is the same as `evaluate`'s.

use std::collections::BTreeMap;

use norn_decisions::{AdmittedDecision, DecisionBinding, MatchedRow, evaluate, evaluate_explained};
use norn_expr::{EvaluationBudget, Number, Value};
use serde_json::{Value as Json, json};

fn closed(properties: Json) -> Json {
    let required: Vec<Json> = properties
        .as_object()
        .expect("object")
        .keys()
        .map(|key| Json::String(key.clone()))
        .collect();
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn decision() -> AdmittedDecision {
    let command = closed(json!({ "actionId": { "type": "string" } }));
    let record = closed(json!({ "id": { "type": "string" } }));
    let rule: norn_ir::RuleRoot = serde_json::from_value(json!({
        "ruleId": "test.rule.explain",
        "ruleVersion": "2.0",
        "contract": "decide",
        "inputSchema": closed(json!({
            "slice": closed(json!({ "total": { "type": "integer" } })),
            "event": closed(json!({ "kind": { "type": "string" } }))
        })),
        "outputSchema": closed(json!({
            "facts": { "type": "array", "items": closed(json!({
                "op": { "type": "string", "enum": ["create"] },
                "objectType": { "type": "string", "enum": ["test.Thing"] },
                "key": { "type": "string" },
                "values": record
            })) },
            "commands": { "type": "array", "items": command },
            "timers": { "type": "array", "items": closed(json!({ "afterMillis": { "type": "integer" } })) }
        })),
        "defaults": { "facts": [], "timers": [] },
        "scopeRequired": [],
        "graph": {
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode" },
                { "id": "band", "name": "band", "type": "decisionTableNode", "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "c", "name": "Over", "type": "expression" }],
                    "outputs": [{ "id": "o", "name": "Commands", "field": "commands" }],
                    "rules": [
                        { "_id": "big", "c": "slice.total > 100", "o": "[{actionId: 'big'}]" },
                        { "_id": "small", "c": "", "o": "[{actionId: 'small'}]" }
                    ]
                } },
                { "id": "out", "name": "out", "type": "outputNode" }
            ],
            "edges": [
                { "id": "e1", "sourceId": "in", "targetId": "band" },
                { "id": "e2", "sourceId": "band", "targetId": "out" }
            ]
        }
    }))
    .expect("rule");
    let binding = DecisionBinding {
        profile_id: "norn-zen/6".to_owned(),
        environment_hash: "sha256:test".to_owned(),
    };
    AdmittedDecision::admit(&rule, &BTreeMap::new(), binding).expect("admits")
}

fn run(total: i64, explained: bool) -> norn_decisions::Evaluated {
    let decision = decision();
    let slice = decision
        .validate_slice(Value::object([(
            "total",
            Value::Number(Number::from(total)),
        )]))
        .expect("slice");
    let event = decision
        .type_event(Value::object([("kind", Value::from("check"))]))
        .expect("event");
    let evaluate = if explained {
        evaluate_explained
    } else {
        evaluate
    };
    evaluate(&decision, &slice, &event, EvaluationBudget::default()).expect("evaluates")
}

fn row(index: u64, id: &str) -> MatchedRow {
    MatchedRow {
        node_id: "band".to_owned(),
        node_name: "band".to_owned(),
        index,
        row_id: Some(id.to_owned()),
    }
}

#[test]
fn explained_evaluation_reports_the_matched_row() {
    let big = run(500, true);
    assert_eq!(big.matched_rows, [row(0, "big")]);
    assert_eq!(
        big.output.commands[0]["actionId"],
        norn_ir::RuntimeValue::String("big".to_owned())
    );
    let small = run(5, true);
    assert_eq!(small.matched_rows, [row(1, "small")]);
}

#[test]
fn plain_evaluation_is_unchanged_and_reports_no_rows() {
    let plain = run(500, false);
    let explained = run(500, true);
    assert!(plain.matched_rows.is_empty());
    assert_eq!(plain.output, explained.output);
}
