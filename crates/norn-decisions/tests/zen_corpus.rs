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
