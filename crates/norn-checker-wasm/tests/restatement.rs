#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The restatement ABI (`restateQuestion`, `restateDashboard`) on the golden
//! fixtures: the same text and hash the checker gives natively.

use norn_checker_wasm::{RESTATEMENT_FORMAT, restate_dashboard_json, restate_question_json};
use serde_json::Value;

fn read(path: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )
    .expect("fixture")
}

#[test]
fn the_abi_restates_every_golden_case_with_its_hash() {
    let cases: Value =
        serde_json::from_str(&read("fixtures/restatement/cases.json")).expect("JSON");
    let metrics = read(cases["metrics"].as_str().expect("metrics path"));
    for case in cases["questions"].as_array().expect("questions") {
        let out: Value = serde_json::from_str(
            &restate_question_json(&case["question"].to_string(), &metrics).expect("ABI"),
        )
        .expect("JSON");
        assert_eq!(out["format"], RESTATEMENT_FORMAT);
        assert_eq!(out["text"], case["restatement"], "{}", case["name"]);
        assert_eq!(out["sha256"], case["restatementSha256"], "{}", case["name"]);
    }
    for case in cases["dashboards"].as_array().expect("dashboards") {
        let out: Value = serde_json::from_str(
            &restate_dashboard_json(&case["payload"].to_string(), &metrics).expect("ABI"),
        )
        .expect("JSON");
        assert_eq!(out["text"], case["restatement"]);
        assert_eq!(out["sha256"], case["restatementSha256"]);
    }
}

#[test]
fn invalid_input_is_an_error_naming_the_argument() {
    let metrics = read("fixtures/demo/answers/scrap-yield.metrics.json");
    let error = restate_question_json("{}", &metrics).expect_err("not a QuestionIR");
    assert!(error.to_string().starts_with("invalid question"), "{error}");
    let error = restate_dashboard_json("[]", &metrics).expect_err("not a dashboard");
    assert!(error.to_string().starts_with("invalid payload"), "{error}");
}
