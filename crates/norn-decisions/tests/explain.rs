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
        profile_id: "norn-zen/7".to_owned(),
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
    assert_eq!(plain.matched_rows, []);
    assert_eq!(plain.output, explained.output);
}

/// A decide graph whose `collect` table runs in loop mode over `slice.items`.
fn looped_collect() -> Result<AdmittedDecision, Box<dyn std::error::Error>> {
    let command = closed(json!({ "actionId": { "type": "string" } }));
    let record = closed(json!({ "id": { "type": "string" } }));
    let item = closed(json!({ "n": { "type": "integer" } }));
    let rule: norn_ir::RuleRoot = serde_json::from_value(json!({
        "ruleId": "test.rule.explain_loop",
        "ruleVersion": "2.0",
        "contract": "decide",
        "inputSchema": closed(json!({
            "slice": closed(json!({ "items": { "type": "array", "items": item } })),
            "event": closed(json!({ "kind": { "type": "string" } }))
        })),
        "outputSchema": closed(json!({
            "facts": { "type": "array", "items": record },
            "commands": { "type": "array", "items": command },
            "timers": { "type": "array", "items": record }
        })),
        "defaults": { "facts": [], "timers": [] },
        "scopeRequired": [],
        "graph": {
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode" },
                { "id": "hits", "name": "hits", "type": "decisionTableNode", "content": {
                    "hitPolicy": "collect",
                    "executionMode": "loop",
                    "inputField": "slice.items",
                    "outputPath": "hits",
                    "inputs": [{ "id": "c", "name": "Size", "type": "expression" }],
                    "outputs": [{ "id": "o", "name": "Tag", "field": "tag" }],
                    "rules": [
                        { "_id": "positive", "c": "n > 0", "o": "'positive'" },
                        { "_id": "big", "c": "n > 10", "o": "'big'" },
                        { "_id": "any", "c": "", "o": "'any'" }
                    ]
                } },
                { "id": "x", "name": "x", "type": "expressionNode", "content": {
                    "expressions": [{ "id": "c", "key": "commands", "value": "[]" }]
                } },
                { "id": "out", "name": "out", "type": "outputNode" }
            ],
            "edges": [
                { "id": "e1", "sourceId": "in", "targetId": "hits" },
                { "id": "e2", "sourceId": "hits", "targetId": "x" },
                { "id": "e3", "sourceId": "x", "targetId": "out" }
            ]
        }
    }))?;
    let binding = DecisionBinding {
        profile_id: "norn-zen/7".to_owned(),
        environment_hash: "sha256:test".to_owned(),
    };
    Ok(AdmittedDecision::admit(&rule, &BTreeMap::new(), binding)?)
}

/// #1708: a looped `collect` table traces one list of matched rows per
/// iteration; the explanation reports every matched row, in iteration and
/// then row order.
#[test]
fn looped_collect_tables_report_every_matched_row_in_order()
-> Result<(), Box<dyn std::error::Error>> {
    let decision = looped_collect()?;
    let items = [20_i64, 5]
        .into_iter()
        .map(|n| Value::object([("n", Value::Number(Number::from(n)))]))
        .collect();
    let slice = decision.validate_slice(Value::object([("items", Value::Array(items))]))?;
    let event = decision.type_event(Value::object([("kind", Value::from("check"))]))?;
    let explained = evaluate_explained(&decision, &slice, &event, EvaluationBudget::default())?;
    let hit = |index: u64, id: &str| MatchedRow {
        node_id: "hits".to_owned(),
        node_name: "hits".to_owned(),
        index,
        row_id: Some(id.to_owned()),
    };
    assert_eq!(
        explained.matched_rows,
        [
            hit(0, "positive"),
            hit(1, "big"),
            hit(2, "any"),
            hit(0, "positive"),
            hit(2, "any"),
        ]
    );
    Ok(())
}
