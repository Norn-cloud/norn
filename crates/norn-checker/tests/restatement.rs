#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The restatement golden fixtures (`fixtures/restatement/cases.json`), byte
//! for byte, with their hashes. The same fixtures run through the WASM ABI
//! (`norn-checker-wasm/tests/restatement.rs`) and in Studio.

use norn_checker::metric::restate::{
    RestateError, restate_dashboard, restate_question, restatement_sha256,
};
use norn_ir::{MetricDefinition, QuestionIR};
use serde_json::Value;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect("fixture"))
        .expect("JSON")
}

fn fixtures() -> (Value, Vec<MetricDefinition>) {
    let cases = read("fixtures/restatement/cases.json");
    let metrics = serde_json::from_value(read(cases["metrics"].as_str().expect("metrics path")))
        .expect("metric declarations");
    (cases, metrics)
}

#[test]
fn every_golden_question_restates_byte_for_byte_with_its_hash() {
    let (cases, metrics) = fixtures();
    let questions = cases["questions"].as_array().expect("questions");
    assert!(questions.len() >= 4);
    for case in questions {
        let question: QuestionIR =
            serde_json::from_value(case["question"].clone()).expect("QuestionIR");
        let text = restate_question(&question, &metrics).expect("restates");
        assert_eq!(
            text,
            case["restatement"].as_str().expect("text"),
            "{}",
            case["name"]
        );
        assert_eq!(
            restatement_sha256(&text),
            case["restatementSha256"].as_str().expect("hash"),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn every_golden_dashboard_restates_its_title_then_one_numbered_line_per_tile() {
    let (cases, metrics) = fixtures();
    for case in cases["dashboards"].as_array().expect("dashboards") {
        let text = restate_dashboard(&case["payload"], &metrics).expect("restates");
        assert_eq!(
            text,
            case["restatement"].as_str().expect("text"),
            "{}",
            case["name"]
        );
        assert_eq!(
            restatement_sha256(&text),
            case["restatementSha256"].as_str().expect("hash")
        );
    }
}

#[test]
fn the_hash_is_lower_case_hex_sha256_of_the_utf8_bytes() {
    // SHA-256 of "abc" (FIPS 180-2 test vector).
    assert_eq!(
        restatement_sha256("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(restatement_sha256("÷").len(), 64);
}

#[test]
fn an_undeclared_metric_or_a_malformed_dashboard_is_refused_not_guessed() {
    let (cases, metrics) = fixtures();
    let mut question = cases["questions"][0]["question"].clone();
    question["aggregate"] = serde_json::json!([{ "metric": "demo.purity" }]);
    let question: QuestionIR = serde_json::from_value(question).expect("QuestionIR");
    assert_eq!(
        restate_question(&question, &metrics),
        Err(RestateError::UnknownMetric("demo.purity".to_owned()))
    );
    assert!(matches!(
        restate_dashboard(&serde_json::json!({ "tiles": [] }), &metrics),
        Err(RestateError::Dashboard(_))
    ));
    assert!(matches!(
        restate_dashboard(
            &serde_json::json!({ "title": "x", "tiles": [{ "title": "t" }] }),
            &metrics
        ),
        Err(RestateError::Dashboard(_))
    ));
}
