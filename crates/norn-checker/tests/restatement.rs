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

#[test]
fn an_inline_calculation_is_refused_not_restated_in_part() {
    let (cases, metrics) = fixtures();
    let mut question = cases["questions"][0]["question"].clone();
    question["aggregate"] = serde_json::json!([{
        "metric": "demo.weight",
        "inline": {
            "name": "demo.weight",
            "agg": "sum",
            "property": "input",
            "aggTimeDimension": { "property": "occurred_at" },
            "x-norn": { "irVersion": "0.1", "additivity": { "kind": "additive" }, "owner": "ops", "certified": false }
        }
    }]);
    let question: QuestionIR = serde_json::from_value(question).expect("QuestionIR");
    assert_eq!(
        restate_question(&question, &metrics),
        Err(RestateError::Inline("demo.weight".to_owned()))
    );
}

/// Questions drawn from small domains, so that two draws often agree on a
/// field: every semantic field varies, and `name` (which names rather than
/// means) varies too. Timestamps are drawn in one spelling per instant; the
/// restatement writes an instant, so `09:30:00Z` and `09:30:00.000Z` read
/// the same by design.
mod semantic {
    use proptest::prelude::*;
    use proptest::sample::select;
    use serde_json::{Value, json};

    pub(super) const FIELDS: [&str; 9] = [
        "name",
        "objectSet",
        "searchAround",
        "aggregate",
        "groupBy",
        "filter",
        "window",
        "freshness",
        "compare",
    ];

    const TIMESTAMPS: [&str; 6] = [
        "2025-12-29T00:00:00Z",
        "2026-07-06T00:00:00Z",
        "2026-07-06T09:30:00Z",
        "2026-07-06T09:30:15Z",
        "2026-07-06T09:30:15.5Z",
        "2026-10-26T00:00:00Z",
    ];

    fn path() -> impl Strategy<Value = Value> {
        (
            proptest::collection::vec(select(vec!["lot", "supplier"]), 0..=2),
            select(vec!["occurred_at", "metal", "input"]),
        )
            .prop_map(|(links, property)| json!({ "links": links, "property": property }))
    }

    fn selection() -> impl Strategy<Value = Value> {
        let rollup = (
            select(vec!["sum", "first", "last", "min", "max"]),
            proptest::collection::vec(path(), 0..=2),
            any::<bool>(),
        )
            .prop_map(|(aggregate, across, over_time)| {
                json!({ "aggregate": aggregate, "across": across, "overTime": over_time })
            });
        (
            select(vec!["demo.yield", "demo.input", "demo.recovered"]),
            proptest::option::of(rollup),
        )
            .prop_map(|(metric, rollup)| match rollup {
                None => json!({ "metric": metric }),
                Some(rollup) => json!({ "metric": metric, "reaggregate": rollup }),
            })
    }

    fn filter() -> impl Strategy<Value = Value> {
        let values = vec![
            json!(null),
            json!(true),
            json!(false),
            json!(5),
            json!(50),
            json!("5"),
            json!("gold"),
            json!("no value"),
            json!(["gold"]),
            json!({ "a": 1 }),
            json!({ "a": "1" }),
        ];
        (
            path(),
            select(vec!["eq", "ne", "lt", "le", "gt", "ge"]),
            select(values),
        )
            .prop_map(|(path, operator, value)| {
                json!({ "path": path, "operator": operator, "value": value })
            })
    }

    fn freshness() -> impl Strategy<Value = Value> {
        prop_oneof![
            select(vec![1000_u64, 1500, 5000, 60000])
                .prop_map(|ms| json!({ "kind": "live", "maxAgeMs": ms })),
            select(vec![1000_u64, 5000])
                .prop_map(|ms| json!({ "kind": "delayed", "maxAgeMs": ms })),
            select(TIMESTAMPS.to_vec()).prop_map(|at| json!({ "kind": "as_of", "at": at })),
        ]
    }

    fn comparison() -> impl Strategy<Value = Option<Value>> {
        proptest::option::of(select(vec![
            json!({ "kind": "previous_period" }),
            json!({ "kind": "plan", "question": "demo.plan_a" }),
            json!({ "kind": "plan", "question": "demo.plan_b" }),
            json!({ "kind": "forecast", "question": "demo.plan_a" }),
            json!({ "kind": "plan", "question": "demo.plan_a",
                    "mapping": [{ "metric": "demo.yield", "referenceMetric": "demo.input" }] }),
        ]))
    }

    pub(super) fn question() -> impl Strategy<Value = Value> {
        (
            (
                select(vec!["demo.a", "demo.b"]),
                select(vec!["demo.Conversion", "demo.Lot", "plant.Conversion"]),
                proptest::collection::vec(select(vec!["lot", "supplier"]), 0..=2),
                proptest::collection::vec(selection(), 1..=2),
            ),
            (
                proptest::collection::vec(
                    (
                        path(),
                        proptest::option::of(select(vec![
                            "day", "week", "month", "quarter", "year",
                        ])),
                    ),
                    0..=2,
                ),
                proptest::collection::vec(filter(), 0..=2),
                (select(TIMESTAMPS.to_vec()), select(TIMESTAMPS.to_vec())),
                freshness(),
                comparison(),
            ),
        )
            .prop_map(
                |(
                    (name, object_set, search_around, aggregate),
                    (group_by, filters, (start, end), freshness, comparison),
                )| {
                    let group_by: Vec<Value> = group_by
                        .into_iter()
                        .map(|(path, grain)| match grain {
                            None => json!({ "path": path }),
                            Some(grain) => json!({ "path": path, "grain": grain }),
                        })
                        .collect();
                    let mut question = json!({
                        "irVersion": "0.1",
                        "name": name,
                        "objectSet": object_set,
                        "searchAround": search_around,
                        "aggregate": aggregate,
                        "groupBy": group_by,
                        "filter": filters,
                        "window": { "start": start, "end": end },
                        "calendar": "iso_utc",
                        "freshness": freshness,
                    });
                    if let (Some(comparison), Some(fields)) = (comparison, question.as_object_mut())
                    {
                        fields.insert("compare".to_owned(), comparison);
                    }
                    question
                },
            )
    }
}

/// What a question means: its IR with the fields that only name it cleared.
fn meaning(question: &QuestionIR) -> QuestionIR {
    let mut meaning = question.clone();
    meaning.name = String::new();
    meaning
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1024))]

    /// Two questions read alike exactly when they mean the same thing: a
    /// change to any one semantic field changes the words, so the signed
    /// hash. `other` supplies the changed field; it often holds the same
    /// value, which must then read alike.
    #[test]
    fn a_change_in_meaning_changes_the_words_and_the_hash(
        base in semantic::question(),
        other in semantic::question(),
        field in proptest::sample::select(semantic::FIELDS.to_vec()),
    ) {
        let (_, metrics) = fixtures();
        let mut changed = base.clone();
        match other.get(field) {
            Some(value) => { changed[field] = value.clone(); }
            None => { changed.as_object_mut().expect("object").remove(field); }
        }
        let read = |value: &Value| -> (QuestionIR, String) {
            let question: QuestionIR = serde_json::from_value(value.clone()).expect("QuestionIR");
            let text = restate_question(&question, &metrics).expect("restates");
            (question, text)
        };
        for (left, right) in [(&base, &changed), (&base, &other)] {
            let (left_question, left_text) = read(left);
            let (right_question, right_text) = read(right);
            let same_meaning = meaning(&left_question) == meaning(&right_question);
            proptest::prop_assert_eq!(
                same_meaning,
                left_text == right_text,
                "field {}:\n{}\n{}", field, left_text, right_text
            );
            proptest::prop_assert_eq!(
                same_meaning,
                restatement_sha256(&left_text) == restatement_sha256(&right_text)
            );
        }
    }
}
