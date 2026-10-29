//! Norn `strict-errors`: failing decision-table cells and switch conditions.
//!
//! Every test runs in both configurations: with the feature off it pins upstream
//! behaviour (fallback / fall-through), with it on it asserts a typed abort.

use serde_json::{json, Value};
use zen_engine::model::GraphContent;
use zen_engine::{
    Decision, EvaluationError, EvaluationOptions, StrictErrorSite, StrictEvaluationError,
};

const STRICT: bool = true;

/// Fails at runtime: subtracting a number from a string is an "Unsupported type" error.
const FAILING: &str = "customer.name - 1 > 0";

fn decision(nodes: Value, edges: Value) -> Decision {
    let content: GraphContent =
        serde_json::from_value(json!({ "nodes": nodes, "edges": edges })).unwrap();
    Decision::from(content)
}

fn edge(id: &str, source: &str, target: &str, handle: Option<&str>) -> Value {
    json!({ "id": id, "type": "edge", "sourceId": source, "targetId": target, "sourceHandle": handle })
}

/// First-hit table: row 1 denies when `deny_cell` holds, row 2 is the catch-all allow.
fn deny_table(deny_cell: &str, deny_output: &str, hit_policy: &str) -> Decision {
    decision(
        json!([
            { "id": "in", "name": "in", "type": "inputNode" },
            { "id": "table", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": hit_policy,
                "inputs": [{ "id": "cond", "name": "cond", "type": "expression" }],
                "outputs": [{ "id": "out", "name": "decision", "field": "decision", "type": "expression" }],
                "rules": [
                    { "_id": "deny", "cond": deny_cell, "out": deny_output },
                    { "_id": "fallback", "cond": "", "out": "'allow'" }
                ]
            }},
            { "id": "out", "name": "out", "type": "outputNode" }
        ]),
        json!([
            edge("e1", "in", "table", None),
            edge("e2", "table", "out", None)
        ]),
    )
}

fn input() -> zen_engine::Variable {
    json!({ "customer": { "name": "Ada" } }).into()
}

fn strict_error(err: Box<EvaluationError>, node: &str) -> StrictEvaluationError {
    let EvaluationError::NodeError {
        node_id, source, ..
    } = *err
    else {
        panic!("expected NodeError, got {err:?}");
    };
    assert_eq!(&*node_id, node);
    source
        .downcast_ref::<StrictEvaluationError>()
        .unwrap_or_else(|| panic!("source should be StrictEvaluationError, got {source:?}"))
        .clone()
}

async fn run(decision: &Decision, trace: bool) -> Result<Value, Box<EvaluationError>> {
    let options = EvaluationOptions {
        trace,
        ..Default::default()
    };
    decision
        .evaluate_with_opts(input(), options)
        .await
        .map(|r| r.result.into())
}

#[tokio::test]
async fn failing_deny_predicate_cannot_yield_fallback() {
    for hit_policy in ["first", "collect"] {
        let table = deny_table(FAILING, "'deny'", hit_policy);
        for trace in [false, true] {
            let result = run(&table, trace).await;
            if STRICT {
                let err = strict_error(result.unwrap_err(), "table");
                assert_eq!(err.site, StrictErrorSite::DecisionTableInput);
                assert_eq!(&*err.id, "cond");
                assert_eq!(&*err.expression, FAILING);
            } else {
                let expected = match hit_policy {
                    "first" => json!({ "decision": "allow" }),
                    _ => json!([{ "decision": "allow" }]),
                };
                assert_eq!(result.unwrap(), expected, "{hit_policy} trace={trace}");
            }
        }
    }
}

#[tokio::test]
async fn failing_deny_output_cannot_yield_fallback() {
    let table = deny_table("true", "customer.name - 1", "first");
    for trace in [false, true] {
        let result = run(&table, trace).await;
        if STRICT {
            let err = strict_error(result.unwrap_err(), "table");
            assert_eq!(err.site, StrictErrorSite::DecisionTableOutput);
            assert_eq!(&*err.id, "out");
        } else {
            assert_eq!(result.unwrap(), json!({ "decision": "allow" }));
        }
    }
}

#[tokio::test]
async fn non_boolean_predicate_is_an_error_in_strict_mode() {
    let table = deny_table("customer.name", "'deny'", "first");
    let result = run(&table, false).await;
    if STRICT {
        let err = strict_error(result.unwrap_err(), "table");
        assert!(err.message.contains("expected a boolean"), "{err}");
    } else {
        assert_eq!(result.unwrap(), json!({ "decision": "allow" }));
    }
}

#[tokio::test]
async fn passing_table_is_unchanged() {
    let table = deny_table("customer.name == 'Ada'", "'deny'", "first");
    assert_eq!(
        run(&table, false).await.unwrap(),
        json!({ "decision": "deny" })
    );
}

/// Switch: statement 1 (`deny` branch) fails, statement 2 is the default `allow` branch.
fn deny_switch(hit_policy: &str, condition: &str) -> Decision {
    let branch = |id: &str, value: &str| {
        json!({ "id": id, "name": id, "type": "expressionNode", "content": {
            "expressions": [{ "id": format!("{id}-x"), "key": "decision", "value": value }]
        }})
    };
    decision(
        json!([
            { "id": "in", "name": "in", "type": "inputNode" },
            { "id": "switch", "name": "switch", "type": "switchNode", "content": {
                "hitPolicy": hit_policy,
                "statements": [
                    { "id": "s-deny", "condition": condition },
                    { "id": "s-default", "condition": "" }
                ]
            }},
            branch("deny", "'deny'"),
            branch("allow", "'allow'"),
            { "id": "out", "name": "out", "type": "outputNode" }
        ]),
        json!([
            edge("e1", "in", "switch", None),
            edge("e2", "switch", "deny", Some("s-deny")),
            edge("e3", "switch", "allow", Some("s-default")),
            edge("e4", "deny", "out", None),
            edge("e5", "allow", "out", None)
        ]),
    )
}

#[tokio::test]
async fn failing_switch_branch_aborts() {
    for hit_policy in ["first", "collect"] {
        let switch = deny_switch(hit_policy, FAILING);
        for trace in [false, true] {
            let result = run(&switch, trace).await;
            if STRICT {
                let err = strict_error(result.unwrap_err(), "switch");
                assert_eq!(err.site, StrictErrorSite::SwitchCondition);
                assert_eq!(&*err.id, "s-deny");
                assert_eq!(&*err.expression, FAILING);
            } else {
                assert_eq!(
                    result.unwrap(),
                    json!({ "decision": "allow" }),
                    "{hit_policy} trace={trace}"
                );
            }
        }
    }
}

#[tokio::test]
async fn passing_switch_is_unchanged() {
    let switch = deny_switch("first", "customer.name == 'Ada'");
    assert_eq!(
        run(&switch, false).await.unwrap(),
        json!({ "decision": "deny" })
    );
}

/// An 8+ row table is indexable (`compile()` builds a table index). The index decides
/// `in [...]` cells without running them, so an object input would be pruned instead of
/// failing; strict evaluation must still raise the typed error.
fn indexed_table() -> Decision {
    let mut rules: Vec<Value> = (0..9)
        .map(|i| json!({ "_id": format!("r{i}"), "c": format!("in [{}, {}]", 2 * i, 2 * i + 1), "o": "'deny'" }))
        .collect();
    rules.push(json!({ "_id": "fallback", "c": "", "o": "'allow'" }));
    decision(
        json!([
            { "id": "in", "name": "in", "type": "inputNode" },
            { "id": "table", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": [{ "id": "c", "name": "c", "field": "customer", "type": "expression" }],
                "outputs": [{ "id": "o", "name": "decision", "field": "decision", "type": "expression" }],
                "rules": rules
            }},
            { "id": "out", "name": "out", "type": "outputNode" }
        ]),
        json!([
            edge("e1", "in", "table", None),
            edge("e2", "table", "out", None)
        ]),
    )
}

#[tokio::test]
async fn indexed_table_cannot_prune_a_failing_cell_into_a_fallback() {
    let plain = indexed_table();
    let mut compiled = indexed_table();
    compiled.compile();
    for (name, table) in [("uncompiled", &plain), ("compiled", &compiled)] {
        for trace in [false, true] {
            // `customer` is an object: `in [0, 1]` on it is a VM type error.
            let result = run(table, trace).await;
            if STRICT {
                let err = strict_error(result.unwrap_err(), "table");
                assert_eq!(err.site, StrictErrorSite::DecisionTableInput, "{name}");
            } else {
                assert_eq!(
                    result.unwrap(),
                    json!({ "decision": "allow" }),
                    "{name} trace={trace}"
                );
            }
        }
        // A matching number still hits its row.
        let ok = table
            .evaluate(json!({ "customer": 5 }).into())
            .await
            .unwrap();
        assert_eq!(
            Value::from(ok.result),
            json!({ "decision": "deny" }),
            "{name}"
        );
    }
}

/// Parent graph whose decision node calls `child`, which contains `failing_node`.
fn parent_of(failing_node: Value) -> Decision {
    use std::sync::Arc;
    use zen_engine::loader::MemoryLoader;

    let child: GraphContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode" },
            failing_node,
            { "id": "out", "name": "out", "type": "outputNode" }
        ],
        "edges": [edge("e1", "in", "inner", None), edge("e2", "inner", "out", None)]
    }))
    .unwrap();
    let loader = Arc::new(MemoryLoader::default());
    loader.add("child", child);

    decision(
        json!([
            { "id": "in", "name": "in", "type": "inputNode" },
            { "id": "sub", "name": "sub", "type": "decisionNode", "content": { "key": "child" } },
            { "id": "out", "name": "out", "type": "outputNode" }
        ]),
        json!([
            edge("e1", "in", "sub", None),
            edge("e2", "sub", "out", None)
        ]),
    )
    .with_loader(loader)
}

/// Returns the child's `EvaluationError` found as the parent decision node's source.
fn child_error(err: Box<EvaluationError>) -> (String, Box<dyn std::error::Error>) {
    let message = err.to_string();
    let EvaluationError::NodeError {
        node_id, source, ..
    } = *err
    else {
        panic!("expected NodeError, got {err:?}");
    };
    assert_eq!(&*node_id, "sub");
    let child = source
        .downcast_ref::<EvaluationError>()
        .expect("sub-decision source should be the child EvaluationError, not a string");
    let EvaluationError::NodeError { node_id, .. } = child else {
        panic!("expected child NodeError, got {child:?}");
    };
    assert_eq!(&**node_id, "inner");
    // The parent's message is unchanged from upstream (the child's message).
    assert_eq!(message, child.to_string());
    (message, source)
}

/// A sub-decision's failure stays in the error source chain, in both modes.
#[tokio::test]
async fn sub_decision_error_is_preserved_in_the_source_chain() {
    let parent = parent_of(json!({
        "id": "inner", "name": "inner", "type": "expressionNode", "content": {
            "expressions": [{ "id": "x", "key": "x", "value": "customer.name - 1" }]
        }
    }));
    let (message, _) = child_error(parent.evaluate(input()).await.unwrap_err());
    assert!(message.contains("customer.name - 1"), "{message}");
}

/// Under `strict-errors`, callers can downcast through the chain to the typed error.
#[tokio::test]
async fn sub_decision_strict_error_is_downcastable() {
    let parent = parent_of(json!({
        "id": "inner", "name": "inner", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{ "id": "cond", "name": "cond", "type": "expression" }],
            "outputs": [{ "id": "o", "name": "decision", "field": "decision", "type": "expression" }],
            "rules": [
                { "_id": "deny", "cond": FAILING, "o": "'deny'" },
                { "_id": "fallback", "cond": "", "o": "'allow'" }
            ]
        }
    }));
    let result = parent.evaluate(input()).await;
    if !STRICT {
        assert_eq!(
            Value::from(result.unwrap().result),
            json!({ "decision": "allow" })
        );
        return;
    }

    let (_, source) = child_error(result.unwrap_err());
    let mut current: Option<&dyn std::error::Error> = Some(source.as_ref());
    let mut strict = None;
    while let Some(e) = current {
        if let Some(s) = e.downcast_ref::<StrictEvaluationError>() {
            strict = Some(s.clone());
        }
        current = e.source();
    }
    let strict = strict.expect("StrictEvaluationError in the source chain");
    assert_eq!(strict.site, StrictErrorSite::DecisionTableInput);
    assert_eq!(&*strict.expression, FAILING);
}
