//! `admit_question_json`: the cell's question admission over the demo
//! analytics world, as the Studio assistant calls it.

use norn_checker_wasm::admit_question_json;
use serde_json::{Value, json};

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect("fixture"))
        .expect("JSON")
}

/// The world's catalog and metrics, and the cell's admitted `demo.yield_week` question.
fn world() -> (String, String, Value) {
    let world = read("fixtures/demo/analytics.world.json");
    (
        world["artifacts"].to_string(),
        world["metrics"].to_string(),
        read("fixtures/demo/answers/scrap-yield.question.json"),
    )
}

fn admit(question: &Value) -> Value {
    let (artifacts, metrics, _) = world();
    serde_json::from_str(
        &admit_question_json(&artifacts, &metrics, &question.to_string()).expect("ABI"),
    )
    .expect("JSON")
}

#[test]
fn the_cells_weekly_yield_question_is_admitted_with_its_plan() {
    let (_, _, question) = world();
    let out = admit(&question);
    assert_eq!(out["format"], "norn-checker-abi/question@1");
    assert_eq!(out["admitted"], true, "{out:#}");
    assert_eq!(out["plan"]["grain"], "demo.Conversion");
    let sums: Vec<_> = out["plan"]["sums"]
        .as_array()
        .expect("sums")
        .iter()
        .map(|sum| sum["property"].clone())
        .collect();
    assert_eq!(sums, vec![json!("recovered"), json!("input")]);
    assert_eq!(out["plan"]["question"]["name"], question["name"]);
}

#[test]
fn a_question_over_an_undeclared_metric_is_refused_with_findings() {
    let (_, _, mut question) = world();
    question["aggregate"] = json!([{ "metric": "demo.purity" }]);
    let out = admit(&question);
    assert_eq!(out["admitted"], false);
    assert!(out["plan"].is_null());
    assert!(
        !out["diagnostics"]["diagnostics"]
            .as_array()
            .expect("findings")
            .is_empty(),
        "{out:#}"
    );
}

#[test]
fn a_filter_on_a_property_the_type_lacks_is_refused() {
    let (_, _, mut question) = world();
    question["filter"] =
        json!([{ "path": { "property": "karat" }, "operator": "eq", "value": "18" }]);
    let out = admit(&question);
    assert_eq!(out["admitted"], false, "{out:#}");
}

#[test]
fn a_world_that_is_not_envelopes_is_refused_not_admitted() {
    let (_, metrics, question) = world();
    let out: Value = serde_json::from_str(
        &admit_question_json("[{\"kind\":\"entity\"}]", &metrics, &question.to_string())
            .expect("ABI"),
    )
    .expect("JSON");
    assert_eq!(out["admitted"], false);
}

#[test]
fn a_question_that_is_not_question_ir_is_an_input_error() {
    let (artifacts, metrics, _) = world();
    assert!(admit_question_json(&artifacts, &metrics, "{\"name\":1}").is_err());
}
