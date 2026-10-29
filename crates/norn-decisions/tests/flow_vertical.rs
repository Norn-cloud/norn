#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
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

//! T7-EFX-3 (#2786): the vertical flow fixture's golden event trace.
//!
//! `tools/norn-ts/fixtures/vertical/route-message.ir.json` is the flow
//! `crm.route_message` compiled from TypeScript (`Effect.gen` source and data
//! builder agree byte for byte, `bun test vertical-fixture`): it performs the
//! T1-FN-1 WASM intent classifier with one retry after a timer, routes on the
//! accepted intent, and catches `UnsupportedLanguage`. This test admits its
//! `decide` rule, delivers the events of `trace.json` one by one against the
//! Run's current state, applies the emitted facts, and checks the Run status,
//! the dispatched commands, the requested timers, and the terminal result.
//! The stale result (a late attempt) and the duplicate result (a redelivery)
//! must emit nothing and leave the Run where it was.

mod support;

use std::collections::BTreeMap;

use norn_decisions::AdmittedDecision;
use norn_expr::{EvaluationBudget, Value};
use norn_ir::RuleRoot;
use serde_json::{Value as Json, json};
use support::{binding, member, read_json, repo_root, run, value};

fn to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Number(number) => serde_json::from_str(&number.to_string()).unwrap(),
        Value::String(text) => Json::String(text.clone()),
        Value::Array(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Object(entries) => Json::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), to_json(item)))
                .collect(),
        ),
    }
}

/// Apply one emitted fact to the Run's state (`create`, `set`, and the
/// generated `to_<state>` lifecycle transitions).
fn apply(state: &mut Option<Json>, fact: &Json) {
    match fact["op"].as_str().unwrap() {
        "create" => *state = Some(fact["values"].clone()),
        "set" => {
            let run = state.as_mut().expect("set on an existing run");
            run[fact["property"].as_str().unwrap()] = fact["value"].clone();
        }
        "transition" => {
            let run = state.as_mut().expect("transition on an existing run");
            let transition = fact["transition"].as_str().unwrap();
            let target = transition
                .strip_prefix("to_")
                .unwrap_or_else(|| panic!("generated transitions are to_<state>: {transition}"));
            run[fact["lifecycle"].as_str().unwrap()] = Json::String(target.to_owned());
        }
        other => panic!("unexpected fact op {other}"),
    }
}

fn decision(trace: &Json) -> AdmittedDecision {
    let artifacts = read_json(&repo_root().join(trace["flow"].as_str().unwrap()));
    let rule = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["kind"] == "rule")
        .expect("the flow's decide rule");
    let root: RuleRoot = serde_json::from_value(rule["payload"].clone()).unwrap();
    AdmittedDecision::admit(&root, &BTreeMap::new(), binding()).expect("admitted")
}

#[test]
fn flow_vertical_trace_matches_the_golden() {
    let trace = read_json(&repo_root().join("tools/norn-ts/fixtures/vertical/trace.json"));
    let decision = decision(&trace);
    let mut failures: Vec<String> = Vec::new();
    for run_trace in trace["runs"].as_array().unwrap() {
        let run_id = run_trace["runId"].as_str().unwrap();
        let mut state: Option<Json> = None;
        let mut requested_timer: Option<Json> = None;
        for step in run_trace["steps"].as_array().unwrap() {
            let label = format!("{run_id} / {}", step["label"].as_str().unwrap());
            let event = if step["deliverTimer"] == json!(true) {
                requested_timer
                    .take()
                    .unwrap_or_else(|| panic!("{label}: no timer was requested"))
            } else {
                step["event"].clone()
            };
            let slice = match &state {
                Some(run) => json!({ "run": run }),
                None => json!({}),
            };
            let before = state.clone();
            let evaluated = run(
                &decision,
                value(&slice),
                value(&event),
                EvaluationBudget::default(),
            )
            .unwrap_or_else(|error| panic!("{label}: {error:?}"));
            let facts: Vec<Json> = member(&evaluated, "facts").iter().map(to_json).collect();
            let commands: Vec<Json> = member(&evaluated, "commands").iter().map(to_json).collect();
            let timers: Vec<Json> = member(&evaluated, "timers").iter().map(to_json).collect();
            for fact in &facts {
                apply(&mut state, fact);
            }
            let expect = &step["expect"];
            let actual = json!({
                "status": state.as_ref().map(|run| run["status"].clone()),
                "commands": commands.iter().map(|c| json!({ "step": c["step"], "attempt": c["attempt"] })).collect::<Vec<_>>(),
                "timers": timers.len(),
                "result": state.as_ref().map(|run| run["slots"]["result"].clone()),
            });
            if expect["noop"] == json!(true) {
                // Stale or duplicate: nothing emitted, the Run unchanged.
                if !facts.is_empty()
                    || !commands.is_empty()
                    || !timers.is_empty()
                    || state != before
                {
                    failures.push(format!("{label}: expected no change, got {actual}"));
                }
                continue;
            }
            let mut expected = json!({
                "status": expect["status"],
                "commands": expect["commands"],
                "timers": expect["timers"],
            });
            let mut observed = actual.clone();
            observed.as_object_mut().unwrap().remove("result");
            if let Some(result) = expect.get("result") {
                expected["result"] = result.clone();
                observed["result"] = actual["result"].clone();
            }
            if observed != expected {
                failures.push(format!("{label}: expected {expected}, got {observed}"));
            }
            // Every perform targets the T1-FN-1 Function the flow declares.
            for command in &commands {
                let manifest =
                    read_json(&repo_root().join(trace["function"]["manifest"].as_str().unwrap()));
                if command["function"] != manifest["functionId"]
                    || command["functionVersion"] != json!(manifest["version"].to_string())
                {
                    failures.push(format!(
                        "{label}: perform {command} does not target the manifest"
                    ));
                }
            }
            requested_timer = timers.first().cloned();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
