use crate::support::{create_test_loader, load_test_data};
use serde_json::json;
use std::sync::Arc;
use tokio::runtime::Builder;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::GraphContent;
use zen_engine::Variable;
use zen_engine::{DecisionEngine, EvaluationOptions};

mod support;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_memory_loader() {
    let memory_loader = Arc::new(MemoryLoader::default());
    memory_loader.add("table", load_test_data("table.json"));

    let engine = DecisionEngine::default().with_loader(memory_loader.clone());
    let table = engine
        .evaluate("table", json!({ "input": 12 }).into())
        .await;

    memory_loader.remove("table");
    let not_found = engine.evaluate("table", json!({}).into()).await;

    assert_eq!(table.unwrap().result, json!({"output": 10}).into());
    assert_eq!(not_found.unwrap_err().to_string(), "Loader error");
}

#[test]
fn engine_noop_loader() {
    let rt = Builder::new_current_thread().build().unwrap();
    // Default engine is noop
    let engine = DecisionEngine::default();
    let result = rt.block_on(engine.evaluate("any.json", json!({}).into()));

    assert_eq!(result.unwrap_err().to_string(), "Loader error");
}

#[test]
fn engine_get_decision() {
    let rt = Builder::new_current_thread().build().unwrap();
    let engine = DecisionEngine::default().with_loader(Arc::new(create_test_loader()));

    assert!(rt
        .block_on(engine.get_decision("table.json"))
        .is_ok_and(|inner| inner.is_ok()));
    assert!(rt.block_on(engine.get_decision("any.json")).is_err());
}

#[test]
fn engine_create_decision() {
    let engine = DecisionEngine::default();
    let _ = engine
        .create_decision(Arc::new(load_test_data("table.json").into()))
        .unwrap();
}

#[test]
fn engine_with_trace() {
    let rt = Builder::new_current_thread().build().unwrap();
    let engine = DecisionEngine::default().with_loader(Arc::new(create_test_loader()));

    let table_r = rt.block_on(engine.evaluate("table.json", json!({ "input": 12 }).into()));
    let table_opt_r = rt.block_on(engine.evaluate_with_opts(
        "table.json",
        json!({ "input": 12 }).into(),
        EvaluationOptions {
            trace: true,
            ..Default::default()
        },
    ));

    let table = table_r.unwrap();
    let table_opt = table_opt_r.unwrap();

    assert!(table.trace.is_none());
    assert!(table_opt.trace.is_some());

    let trace = table_opt.trace.unwrap().into_graph().unwrap();
    assert_eq!(trace.len(), 3); // trace for each node
}

#[tokio::test]
async fn engine_switch_node() {
    let engine = DecisionEngine::default().with_loader(Arc::new(create_test_loader()));

    let switch_node_r = engine
        .evaluate("switch-node.json", json!({ "color": "yellow" }).into())
        .await;

    let table = switch_node_r.unwrap();
    println!("{table:?}");
}

#[tokio::test]
async fn decision_table_index_matches_linear_semantics() {
    let table_node = json!({
        "id": "dt-node",
        "type": "decisionTableNode",
        "name": "dt",
        "content": {
            "hitPolicy": "first",
            "inputs": [
                {"id": "c1", "name": "Tier", "field": "tier"},
                {"id": "c2", "name": "Amount", "field": "amount"},
                {"id": "c3", "name": "Active", "field": "active"}
            ],
            "outputs": [{"id": "o1", "name": "Rate", "field": "rate"}],
            "rules": [
                {"_id": "r1", "c1": "'gold'", "c2": "100", "c3": "true", "o1": "1"},
                {"_id": "r2", "c1": "'gold'", "c2": "> 500", "c3": "", "o1": "2"},
                {"_id": "r3", "c1": "'silver'", "c2": "[100..200]", "c3": "", "o1": "3"},
                {"_id": "r4", "c1": "'silver', 'bronze'", "c2": "", "c3": "false", "o1": "4"},
                {"_id": "r5", "c1": "", "c2": "42", "c3": "", "o1": "5"},
                {"_id": "r6", "c1": "'gold'", "c2": "", "c3": "", "o1": "6"},
                {"_id": "r7", "c1": "'bronze'", "c2": "7, 8, 9", "c3": "true", "o1": "7"},
                {"_id": "r8", "c1": "", "c2": "", "c3": "", "o1": "8"},
                {"_id": "r9", "c1": "'platinum'", "c2": "1000", "c3": "true", "o1": "9"}
            ]
        }
    });
    let graph = json!({
        "nodes": [
            {"id": "in", "type": "inputNode", "name": "request"},
            table_node,
            {"id": "out", "type": "outputNode", "name": "response"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "dt-node"},
            {"id": "e2", "sourceId": "dt-node", "targetId": "out"}
        ]
    });

    let probes = [
        json!({"tier": "gold", "amount": 100, "active": true}),
        json!({"tier": "gold", "amount": 600, "active": false}),
        json!({"tier": "silver", "amount": 150, "active": true}),
        json!({"tier": "silver", "amount": 50, "active": false}),
        json!({"tier": "bronze", "amount": 8, "active": true}),
        json!({"tier": "unknown", "amount": 42, "active": false}),
        json!({"tier": "unknown", "amount": 0, "active": false}),
        json!({"tier": "platinum", "amount": 1000, "active": true}),
        json!({"tier": 5, "amount": "x", "active": null}),
    ];

    for hit_policy in ["first", "collect"] {
        let mut graph = graph.clone();
        graph["nodes"][1]["content"]["hitPolicy"] = json!(hit_policy);
        let mut content: GraphContent = serde_json::from_value(graph).unwrap();
        content.compile();
        let decision = DecisionEngine::default()
            .create_decision(Arc::new(content.into()))
            .unwrap();

        for probe in &probes {
            let indexed = decision
                .evaluate(Variable::from(probe))
                .await
                .unwrap()
                .result;
            let linear = decision
                .evaluate_with_opts(
                    Variable::from(probe),
                    EvaluationOptions {
                        trace: true,
                        max_depth: 5,
                    },
                )
                .await
                .unwrap()
                .result;
            assert_eq!(
                serde_json::to_value(&indexed).unwrap(),
                serde_json::to_value(&linear).unwrap(),
                "hit_policy={hit_policy} probe={probe}"
            );
        }
    }
}
