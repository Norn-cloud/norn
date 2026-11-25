use crate::support::{create_test_loader, load_test_data};
use serde_json::json;
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Builder;
use zen_engine::{
    Decision, DecisionGraphValidationError, EvaluationError, EvaluationOptions, Variable,
};

mod support;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_from_content() {
    let table_content = load_test_data("table.json");
    let decision = Decision::from(table_content);

    let context = json!({ "input": 5 });
    let result = decision.evaluate(context.into()).await;

    assert_eq!(result.unwrap().result, json!({"output": 0}).into());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_from_content_recursive() {
    let recursive_content = load_test_data("recursive-table1.json");
    let decision = Decision::from(recursive_content);

    let context = json!({});
    let result = decision.evaluate(context.clone().into()).await;
    match result.unwrap_err().deref() {
        EvaluationError::NodeError {
            node_id, source, ..
        } => {
            assert_eq!(node_id.deref(), "0b8dcf6b-fc04-47cb-bf82-bda764e6c09b");
            assert!(source.to_string().contains("Loader failed"));
        }
        _ => panic!("Depth limit not exceeded"),
    }

    let with_loader = decision.with_loader(Arc::new(create_test_loader()));
    let new_result = with_loader.evaluate(context.clone().into()).await;
    match new_result.unwrap_err().deref() {
        EvaluationError::NodeError { source, .. } => {
            assert_eq!(source.to_string(), "Depth limit exceeded")
        }
        _ => panic!("Depth limit not exceeded"),
    }
}

#[test]
fn decision_expression_node() {
    let rt = Builder::new_current_thread().build().unwrap();
    let decision = Decision::from(load_test_data("expression.json"));
    let context = json!({
        "numbers": [1, 5, 15, 25],
        "firstName": "John",
        "lastName": "Doe"
    });

    let result = rt.block_on(decision.evaluate(context.into()));
    assert_eq!(
        result.unwrap().result,
        json!({
            "largeNumbers": [15, 25],
            "smallNumbers": [1, 5],
            "fullName": "John Doe",
            "deep": {
                "nested": {
                    "sum": 46
                }
            }
        })
        .into()
    )
}

#[test]
fn decision_validation() {
    let cyclic_decision = Decision::from(load_test_data("error-cyclic.json"));
    let cyclic_error = cyclic_decision.validate().unwrap_err();
    assert!(matches!(
        cyclic_error,
        DecisionGraphValidationError::CyclicGraph
    ));

    let missing_input_decision = Decision::from(load_test_data("error-missing-input.json"));
    let missing_input_error = missing_input_decision.validate().unwrap_err();
    assert!(matches!(
        missing_input_error,
        DecisionGraphValidationError::InvalidInputCount(_)
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_missing_cell_key_is_treated_as_empty() {
    // Editors may store sparse rules (only the cells that were filled in).
    // A missing input cell key behaves like an empty cell ("any") and a
    // missing output cell key simply writes nothing for that column.
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "Age", "field": "age" }],
                    "outputs": [
                        { "id": "o1", "name": "Result", "field": "result" },
                        { "id": "o2", "name": "Extra", "field": "extra" }
                    ],
                    "rules": [
                        { "_id": "r1", "o1": "'hit'" }
                    ]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let result = decision.evaluate(json!({ "age": 1 }).into()).await.unwrap();
    assert_eq!(result.result, json!({ "result": "hit" }).into());

    let traced = decision
        .evaluate_with_opts(
            json!({ "age": 1 }).into(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(traced.result, result.result);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_column_collect_from_file() {
    let decision = Decision::from(load_test_data("table-collect-columns.json"));

    let result = decision
        .evaluate(json!({ "customer": { "age": 70 } }).into())
        .await
        .unwrap();
    assert_eq!(
        result.result,
        json!({
            "customer": {
                "tier": "senior",
                "tags": ["senior-discount", "adult", "age-verified", "customer"]
            }
        })
        .into()
    );

    let minor = decision
        .evaluate(json!({ "customer": { "age": 12 } }).into())
        .await
        .unwrap();
    assert_eq!(
        minor.result,
        json!({ "customer": { "tags": ["customer"] } }).into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_collect_rows_do_not_alias() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "collect",
                    "inputs": [{ "id": "i1", "name": "Name", "field": "person.name" }],
                    "outputs": [
                        { "id": "o1", "name": "Person", "field": "person" },
                        { "id": "o2", "name": "Age", "field": "person.age" }
                    ],
                    "rules": [
                        { "_id": "r1", "i1": "", "o1": "person", "o2": "2" },
                        { "_id": "r2", "i1": "", "o1": "person", "o2": "3" },
                        { "_id": "r3", "i1": "", "o1": "person", "o2": "4" }
                    ]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let result = decision
        .evaluate(json!({ "person": { "name": "Ann" } }).into())
        .await
        .unwrap();
    assert_eq!(
        result.result,
        json!([
            { "person": { "name": "Ann", "age": 2 } },
            { "person": { "name": "Ann", "age": 3 } },
            { "person": { "name": "Ann", "age": 4 } }
        ])
        .into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn node_handlers_do_not_mutate_aliased_inputs() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "e1", "name": "e1", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x1", "key": "first", "value": "a + 1" }]
                }
            },
            {
                "id": "sw", "name": "sw", "type": "switchNode",
                "content": {
                    "hitPolicy": "first",
                    "statements": [{ "id": "s1", "condition": "" }]
                }
            },
            {
                "id": "e2", "name": "e2", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x2", "key": "second", "value": "first + 1" }]
                }
            },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "passThrough": true,
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "A", "field": "a" }],
                    "outputs": [{ "id": "o1", "name": "Result", "field": "dtResult" }],
                    "rules": [{ "_id": "r1", "i1": "", "o1": "'hit'" }]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "ed1", "sourceId": "in", "targetId": "e1" },
            { "id": "ed2", "sourceId": "e1", "targetId": "sw" },
            { "id": "ed3", "sourceId": "sw", "targetId": "e2", "sourceHandle": "s1" },
            { "id": "ed4", "sourceId": "e2", "targetId": "dt" },
            { "id": "ed5", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let input: Variable = json!({ "a": 1, "nested": { "k": "v" } }).into();
    let snapshot = input.to_value();

    let response = decision
        .evaluate_with_opts(
            input.clone(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(input.to_value(), snapshot);
    assert_eq!(
        response.result,
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2, "second": 3, "dtResult": "hit" })
            .into()
    );

    let trace = response.trace.unwrap().into_graph().unwrap();
    let output_of = |id: &str| trace.get(id).unwrap().output.clone();

    assert_eq!(
        output_of("in"),
        json!({ "a": 1, "nested": { "k": "v" } }).into()
    );
    assert_eq!(
        output_of("e1"),
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2 }).into()
    );
    assert_eq!(
        output_of("sw"),
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2 }).into()
    );
    assert_eq!(
        output_of("e2"),
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2, "second": 3 }).into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn merged_inputs_do_not_mutate_parent_node_outputs() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "pa", "name": "pa", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x1", "key": "pa", "value": "10" }]
                }
            },
            {
                "id": "pb", "name": "pb", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x2", "key": "pb", "value": "20" }]
                }
            },
            {
                "id": "join", "name": "join", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x3", "key": "sum", "value": "pa + pb" }]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "ed1", "sourceId": "in", "targetId": "pa" },
            { "id": "ed2", "sourceId": "in", "targetId": "pb" },
            { "id": "ed3", "sourceId": "pa", "targetId": "join" },
            { "id": "ed4", "sourceId": "pb", "targetId": "join" },
            { "id": "ed5", "sourceId": "join", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let input: Variable = json!({ "a": 1 }).into();
    let snapshot = input.to_value();

    let response = decision
        .evaluate_with_opts(
            input.clone(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(input.to_value(), snapshot);
    assert_eq!(
        response.result,
        json!({ "a": 1, "pa": 10, "pb": 20, "sum": 30 }).into()
    );

    let trace = response.trace.unwrap().into_graph().unwrap();
    let output_of = |id: &str| trace.get(id).unwrap().output.clone();

    assert_eq!(output_of("in"), json!({ "a": 1 }).into());
    assert_eq!(output_of("pa"), json!({ "a": 1, "pa": 10 }).into());
    assert_eq!(output_of("pb"), json!({ "a": 1, "pb": 20 }).into());
}

/// Norn (#1711): `copy = orig` then `copy.status = 'new'` writes copy-on-write: the
/// input record, later reads of `orig.status`, and the trace of `copy` keep `old`.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn dotted_assignment_never_mutates_the_assigned_record() {
    let content: Result<zen_engine::model::GraphContent, _> = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "x", "name": "x", "type": "expressionNode",
                "content": {
                    "expressions": [
                        { "id": "x1", "key": "copy", "value": "orig" },
                        { "id": "x2", "key": "copy.status", "value": "'new'" },
                        { "id": "x3", "key": "origStatus", "value": "orig.status" },
                        { "id": "x4", "key": "copyStatus", "value": "$.copy.status" },
                        { "id": "x5", "key": "nested", "value": "orig" },
                        { "id": "x6", "key": "nested.inner.flag", "value": "true" },
                        { "id": "x7", "key": "innerFlag", "value": "orig.inner.flag" }
                    ]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "x" },
            { "id": "e2", "sourceId": "x", "targetId": "out" }
        ]
    }));
    let Ok(content) = content else {
        panic!("graph should decode: {content:?}");
    };
    let decision = Decision::from(content);
    let input: Variable =
        json!({ "orig": { "id": 7, "status": "old", "inner": { "flag": false } } }).into();
    let snapshot = input.to_value();
    let options = EvaluationOptions {
        trace: true,
        ..Default::default()
    };
    let response = match decision.evaluate_with_opts(input.clone(), options).await {
        Ok(response) => response,
        Err(err) => panic!("evaluation failed: {err:?}"),
    };

    assert_eq!(input.to_value(), snapshot);
    assert_eq!(
        response.result.to_value(),
        json!({
            "copy": { "id": 7, "status": "new", "inner": { "flag": false } },
            "origStatus": "old",
            "copyStatus": "new",
            "nested": { "id": 7, "status": "old", "inner": { "flag": true } },
            "innerFlag": false
        })
    );
    let copy_trace = response
        .trace
        .and_then(|trace| trace.into_graph())
        .and_then(|graph| graph.get("x").and_then(|node| node.trace_data.clone()))
        .map(|data| data.to_value());
    let copy_trace = copy_trace.unwrap_or_default();
    assert_eq!(
        copy_trace["copy"]["result"],
        json!({ "id": 7, "status": "old", "inner": { "flag": false } })
    );
}
