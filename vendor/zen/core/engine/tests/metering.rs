//! Norn `metering`: graph evaluation under a deterministic operation budget.

use serde_json::{json, Value};
use zen_engine::meter::{BudgetExhausted, Meter};
use zen_engine::model::GraphContent;
use zen_engine::{Decision, EvaluationError, EvaluationOptions};

fn decision() -> Decision {
    let content: GraphContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode" },
            { "id": "expr", "name": "expr", "type": "expressionNode", "content": {
                "expressions": [
                    { "id": "x1", "key": "grid", "value": "flatMap([0..size], map([0..size], # * 2))" }
                ]
            }},
            { "id": "table", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": [{ "id": "c", "name": "c", "field": "len(grid)", "type": "expression" }],
                "outputs": [{ "id": "o", "name": "o", "field": "tier", "type": "expression" }],
                "rules": [
                    { "_id": "r1", "c": "> 10000", "o": "'huge'" },
                    { "_id": "r2", "c": "> 100", "o": "'big'" },
                    { "_id": "r3", "c": "", "o": "'small'" }
                ]
            }},
            { "id": "out", "name": "out", "type": "outputNode" }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "expr" },
            { "id": "e2", "sourceId": "expr", "targetId": "table" },
            { "id": "e3", "sourceId": "table", "targetId": "out" }
        ]
    }))
    .unwrap();
    Decision::from(content)
}

async fn run(
    decision: &Decision,
    size: u64,
    limit: u64,
    trace: bool,
) -> (Result<Value, Box<EvaluationError>>, Meter) {
    let meter = Meter::new(limit);
    let options = EvaluationOptions {
        trace,
        ..Default::default()
    };
    let result = decision
        .evaluate_metered(json!({ "size": size }).into(), options, meter.clone())
        .await
        .map(|r| r.result.into());
    (result, meter)
}

#[tokio::test]
async fn full_cost_is_independent_of_trace_and_compilation() {
    let plain = decision();
    let mut compiled = decision();
    compiled.compile();

    let mut costs = Vec::new();
    for decision in [&plain, &compiled] {
        for trace in [false, true] {
            let (result, meter) = run(decision, 19, u64::MAX, trace).await;
            assert_eq!(result.unwrap(), json!({ "tier": "big" }));
            costs.push(meter.used());
        }
    }
    assert!(costs.iter().all(|c| *c == costs[0]), "{costs:?}");
    assert!(costs[0] > 400, "{costs:?}");
}

#[tokio::test]
async fn small_budget_fails_with_a_typed_error_at_a_stable_count() {
    let plain = decision();
    let mut compiled = decision();
    compiled.compile();

    let mut failures = Vec::new();
    for decision in [&plain, &compiled] {
        for trace in [false, true] {
            let (result, meter) = run(decision, 19, 500, trace).await;
            let err = result.unwrap_err();
            let EvaluationError::BudgetExhausted(exhausted) = *err else {
                panic!("expected BudgetExhausted, got {err:?}");
            };
            assert_eq!(meter.exhausted(), Some(exhausted));
            failures.push(exhausted);
        }
    }
    assert!(failures.iter().all(|f| *f == failures[0]), "{failures:?}");
    assert_eq!(failures[0].limit, 500);
}

#[tokio::test]
async fn exhaustion_inside_a_table_cell_is_not_swallowed() {
    // Budget large enough for the expression node, too small for the table: upstream
    // turns a failing cell into a non-match (`small`), metering must still fail.
    let (full, meter) = run(&decision(), 19, u64::MAX, false).await;
    full.unwrap();
    let total = meter.used();

    // Every budget short of the total fails, including those that run out inside the
    // table's cells (the last few dozen units are the table and the output node).
    for limit in total - 40..total {
        let (result, _) = run(&decision(), 19, limit, false).await;
        let err = result.unwrap_err();
        assert!(
            matches!(*err, EvaluationError::BudgetExhausted(BudgetExhausted { limit: l, .. }) if l == limit),
            "limit {limit}: {err:?}"
        );
    }
}

#[tokio::test]
async fn unmetered_evaluation_is_unchanged() {
    let result = decision()
        .evaluate(json!({ "size": 19 }).into())
        .await
        .unwrap();
    assert_eq!(Value::from(result.result), json!({ "tier": "big" }));
}

/// A transform-attributes loop over an input array with a node that does no VM work.
fn transform_loop() -> Decision {
    let content: GraphContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode" },
            { "id": "expr", "name": "expr", "type": "expressionNode", "content": {
                "expressions": [],
                "inputField": "items",
                "executionMode": "loop"
            }},
            { "id": "out", "name": "out", "type": "outputNode" }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "expr" },
            { "id": "e2", "sourceId": "expr", "targetId": "out" }
        ]
    }))
    .unwrap();
    Decision::from(content)
}

async fn run_loop(items: usize, limit: u64) -> (Result<Value, Box<EvaluationError>>, Meter) {
    let meter = Meter::new(limit);
    let input = json!({ "items": vec![json!({}); items] });
    let result = transform_loop()
        .evaluate_metered(input.into(), Default::default(), meter.clone())
        .await
        .map(|r| r.result.into());
    (result, meter)
}

#[tokio::test]
async fn transform_loop_is_charged_per_element() {
    // Cost grows by exactly one unit per element (plus the fixed graph overhead).
    let (small, small_meter) = run_loop(10, u64::MAX).await;
    small.unwrap();
    let (large, large_meter) = run_loop(1_000, u64::MAX).await;
    large.unwrap();
    assert_eq!(large_meter.used() - small_meter.used(), 990);

    // A large array exhausts a small budget, up front and with a typed error.
    let (result, meter) = run_loop(100_000, 1_000).await;
    let err = result.unwrap_err();
    let EvaluationError::BudgetExhausted(exhausted) = *err else {
        panic!("expected BudgetExhausted, got {err:?}");
    };
    assert_eq!(exhausted.limit, 1_000);
    assert!(exhausted.used > 100_000, "{exhausted:?}");
    assert_eq!(meter.exhausted(), Some(exhausted));
}
