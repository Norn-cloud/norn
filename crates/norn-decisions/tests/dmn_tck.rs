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

//! The DMN TCK applicable subset (T6-DMN-1, #2688; CI job `dmn-tck`).
//!
//! `tests/dmn_tck/cases/` holds the compliance-level-2 decision-table models
//! of the DMN TCK at the commit pinned in `tests/dmn_tck/manifest.json`,
//! translated by `tests/dmn_tck/translate.py` into RuleIR v2 decide rules with
//! the TCK's own test cases; the manifest lists every included case id and
//! every excluded model with its reason. Original cases for the four known-gap
//! models remain blocking; no-hit witnesses require their exact known wrong
//! output, and fail if fixed so their exclusions must shrink. `tests/dmn_tck/norn/` adds Norn cases
//! (no hit, null input, error propagation, row order, conflicting branch
//! writes). A passing subset is evidence for the FEEL-free decision-table
//! subset, not DMN or FEEL conformance.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_expr::EvaluationBudget;
use serde_json::Value as Json;
use support::{admit_fixture, check_fixture, read_json, run, value};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dmn_tck")
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("fixture dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn manifest_is_pinned_and_matches_the_translated_cases() {
    let manifest = read_json(&dir().join("manifest.json"));
    assert_eq!(
        manifest["commit"], "0456e1a2fa5b9c2382cc7380cc6b95f0031e4793",
        "TCK pin changed: review the full model inventory"
    );
    assert_eq!(manifest["tck"], "https://github.com/dmn-tck/tck");
    let included = manifest["included"].as_array().unwrap();
    let gaps = manifest["known_gaps"].as_array().unwrap();
    let mut models = BTreeSet::new();
    let mut case_ids = BTreeSet::new();
    let mut witnesses = BTreeSet::new();
    for entry in included.iter().chain(gaps) {
        let model = entry["model"].as_str().unwrap();
        assert!(models.insert(model.to_owned()), "duplicate model: {model}");
        let fixture = read_json(&dir().join("cases").join(format!("{model}.json")));
        assert_eq!(fixture["rule"]["ruleId"], format!("dmn_tck.{model}"));
        let listed: Vec<&Json> = entry["cases"].as_array().unwrap().iter().collect();
        let present: Vec<&Json> = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|case| &case["name"])
            .collect();
        assert_eq!(listed, present, "{model}: case ids");
        for case in listed {
            assert!(case_ids.insert(case.as_str().unwrap().to_owned()));
        }
    }
    let files: BTreeSet<String> = json_files(&dir().join("cases"))
        .iter()
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(models, files, "manifest and cases/ inventory disagree");
    let original_models: BTreeSet<String> = [
        "0004-simpletable-U",
        "0005-simpletable-A",
        "0006-simpletable-P1",
        "0007-simpletable-P2",
        "0010-multi-output-U",
        "0108-first-hitpolicy",
        "0109-ruleOrder-hitpolicy",
        "0110-outputOrder-hitpolicy",
        "0111-first-hitpolicy-singleoutputcol",
        "0112-ruleOrder-hitpolicy-singleinoutcol",
        "0113-outputOrder-hitpolicy-singleinoutcol",
        "0114-min-collect-hitpolicy",
        "0115-sum-collect-hitpolicy",
        "0116-count-collect-hitpolicy",
        "0117-multi-any-hitpolicy",
        "0118-multi-priority-hitpolicy",
        "0119-multi-collect-hitpolicy",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(models, original_models, "original model inventory changed");
    assert_eq!(case_ids.len(), 51, "original upstream cases changed");
    for entry in gaps {
        let model = entry["model"].as_str().unwrap();
        let issue = match model {
            "0108-first-hitpolicy" | "0109-ruleOrder-hitpolicy" => 2929,
            "0110-outputOrder-hitpolicy" | "0119-multi-collect-hitpolicy" => 2930,
            _ => panic!("unauthorized model exclusion: {model}"),
        };
        assert_eq!(entry["issue"], issue);
        let reason = entry["reason"].as_str().unwrap();
        assert!(!reason.is_empty());
        assert_eq!(
            manifest["excluded"]["compliance-level-2"][model],
            format!("#{issue}: {reason} (guarded by a no-hit witness)")
        );
        let witness_name = entry["witness"].as_str().unwrap();
        assert_eq!(witness_name, format!("{model}.json"));
        assert!(witnesses.insert(witness_name.to_owned()));
        let witness = read_json(&dir().join("witnesses").join(witness_name));
        for field in ["model", "issue", "reason"] {
            assert_eq!(entry[field], witness[field], "{model}: witness {field}");
        }
        for output in ["expect", "knownActual"] {
            let members: BTreeSet<&str> = witness[output]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(members, BTreeSet::from(["facts", "commands", "timers"]));
            for member in members {
                assert!(witness[output][member].is_array());
            }
        }
        assert_ne!(witness["expect"], witness["knownActual"]);
    }
    let witness_files: BTreeSet<String> = json_files(&dir().join("witnesses"))
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        witnesses, witness_files,
        "unlisted or missing gap witnesses"
    );
    let excluded_models = manifest["excluded"]["compliance-level-2"]
        .as_object()
        .unwrap();
    for entry in included {
        assert!(!excluded_models.contains_key(entry["model"].as_str().unwrap()));
    }
    let unsupported_models: BTreeSet<&str> = [
        "0001-input-data-string",
        "0002-input-data-number",
        "0003-input-data-string-allowed-values",
        "0008-LX-arithmetic",
        "0009-invocation-arithmetic",
        "0100-feel-constants",
        "0101-feel-constants",
        "0102-feel-constants",
        "0105-feel-math",
        "0106-feel-ternary-logic",
        "0107-feel-ternary-logic-not",
    ]
    .into_iter()
    .chain(gaps.iter().map(|entry| entry["model"].as_str().unwrap()))
    .collect();
    assert_eq!(
        excluded_models
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        unsupported_models,
        "unlisted or extra upstream exclusion"
    );
    assert_eq!(included.len() + excluded_models.len(), 28);
    assert_eq!(manifest["excluded"]["compliance-level-3"]["count"], 120);
    let excluded = manifest["excluded"]["compliance-level-2"]
        .as_object()
        .unwrap();
    assert!(
        excluded
            .values()
            .all(|reason| reason.as_str().is_some_and(|r| !r.is_empty()))
    );
    assert!(included.len() >= 13, "applicable subset shrank");
}

#[test]
fn dmn_tck_applicable_subset() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in json_files(&dir().join("cases")) {
        checked += check_fixture(&path, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} TCK failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(checked, 51, "all original TCK cases must run");
}

#[test]
fn norn_decision_table_cases() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in json_files(&dir().join("norn")) {
        checked += check_fixture(&path, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} Norn case failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(checked, 10, "all original Norn cases must run");
}

/// Exclusions are exact known semantic mismatches, never arbitrary failures.
#[test]
fn known_gap_witnesses_require_exclusions_to_shrink_when_fixed() {
    let manifest = read_json(&dir().join("manifest.json"));
    for entry in manifest["known_gaps"].as_array().unwrap() {
        let model = entry["model"].as_str().unwrap();
        let fixture = read_json(&dir().join("cases").join(format!("{model}.json")));
        let witness = read_json(
            &dir()
                .join("witnesses")
                .join(entry["witness"].as_str().unwrap()),
        );
        let decision = admit_fixture(&fixture).expect("excluded model must still admit");
        let evaluated = run(
            &decision,
            value(&witness["slice"]),
            value(&witness["event"]),
            EvaluationBudget::default(),
        )
        .unwrap_or_else(|error| panic!("{model}: unrelated evaluation error: {error}"));
        let actual = serde_json::to_value(&evaluated.output).unwrap();
        assert_ne!(
            actual, witness["expect"],
            "{model}: #{} now passes; remove its exclusion and witness, and include the model",
            entry["issue"]
        );
        assert_eq!(
            actual, witness["knownActual"],
            "{model}: output changed beyond the documented #{} gap",
            entry["issue"]
        );
    }
}
