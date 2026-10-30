#![allow(
    clippy::arithmetic_side_effects,
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

//! The zen-engine admitted-node corpus (`tests/zen_corpus/`, CI job
//! `zen-corpus`): every node type of the Norn profile evaluated through
//! `norn-decisions`, the checker's valid `RuleIR` v2 fixtures end to end,
//! strict-error, budget, and output-schema refusals with zero effects, and
//! load-time refusals of everything outside the profile.
//!
//! Fixture shape: `rule` (an inline payload) or `ruleFile` (a repo-relative
//! envelope), optional `subDecisions` (key -> rule), then either
//! `admissionError` (the expected refusal) or `cases`, each with `slice`,
//! `event`, an optional `budget`, and either `expect` (`facts`, `commands`,
//! `timers`) or `error`.

mod support;

use std::path::Path;

use support::check_fixture;

#[test]
fn zen_admitted_node_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/zen_corpus");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("corpus dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 12, "corpus shrank: {paths:?}");
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in &paths {
        checked += check_fixture(path, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} corpus failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked >= 24, "only {checked} corpus checks ran");
}

#[test]
fn lean_l1_hit_policy_corpus_matches_real_evaluator() {
    let corpus = include_str!("../../../verification/kernel-l1/corpus.tsv");
    let original: serde_json::Value =
        serde_json::from_str(include_str!("zen_corpus/strict_table_deny_row.json")).unwrap();
    let mut count = 0;
    for line in corpus.lines().filter(|line| line.starts_with("table\t")) {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 4);
        let amount: i64 = fields[2].parse().unwrap();
        let mut fixture = original.clone();
        let nodes = fixture["rule"]["graph"]["nodes"].as_array_mut().unwrap();
        let table = &mut nodes[1]["content"];
        table["hitPolicy"] = serde_json::json!(fields[1]);
        table["outputPath"] = serde_json::json!("hit");
        table["rules"] = serde_json::json!([
            {"_id":"negative", "c_over":"slice.amount <= 0", "o_decision":"'-1'"},
            {"_id":"always", "c_over":"", "o_decision":"'1'"},
            {"_id":"positive", "c_over":"slice.amount > 0", "o_decision":"'2'"}
        ]);
        nodes[2]["content"]["expressions"][1]["value"] =
            serde_json::json!(if fields[1] == "first" {
                "[{actionId: hit.decision}]"
            } else {
                "map(hit, {actionId: #.decision})"
            });
        let decision = support::admit_fixture(&fixture).unwrap();
        let evaluated = support::run(
            &decision,
            support::value(&serde_json::json!({"amount":amount,"limit":0})),
            support::value(&serde_json::json!({"kind":"test"})),
            norn_expr::EvaluationBudget::default(),
        )
        .unwrap();
        let actual = serde_json::to_value(evaluated.output).unwrap();
        let expected: Vec<_> = fields[3]
            .split(',')
            .map(|value| serde_json::json!({"actionId":value}))
            .collect();
        assert_eq!(actual["commands"], serde_json::json!(expected), "{line}");
        assert_eq!(actual["facts"], serde_json::json!([]));
        assert_eq!(actual["timers"], serde_json::json!([]));
        count += 1;
    }
    assert_eq!(count, 10);
}
