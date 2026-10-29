#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Invalid-fixture conformance and JSON-output snapshots (T1-CHK-2, #2525;
//! the fixture manifest pattern of #2425).
//!
//! `fixtures/invalid/manifest.json` lists each invalid artifact with the
//! reason it is invalid, the JSON pointer the error is reported at
//! (`error_at`), and the expected code. A checker that accepts any entry, or
//! reports it under another code or location, is nonconformant. The full
//! [`norn_checker::diagnostics_to_json`] output for each fixture is pinned in
//! `tests/snapshots/`.

use std::{fs, path::Path};

use norn_checker::{check_artifact_json, codes, diagnostics_to_json};
use serde_json::Value;

const UPDATE_ENV: &str = "NORN_CHECKER_UPDATE_GOLDENS";

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate sits two levels below the repo root")
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn field<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("manifest entry {entry} is missing {key}"))
}

#[test]
fn invalid_fixtures_are_rejected_at_the_declared_code_and_pointer() {
    check_invalid_corpus("fixtures/invalid", "");
}

/// The canonical `RuleIR` v2 / Norn-profile rejection corpus (T3-DEC-1,
/// #2669), which the `norn-decisions` evaluator gate (T3-DEC-2) reuses: every
/// fixture is rejected at its declared code and pointer, and every T2 code of
/// the profile has a fixture.
#[test]
fn rule_ir_v2_rejection_corpus_is_rejected_at_the_declared_code_and_pointer() {
    use norn_checker::jdm_profile::codes as profile;
    let covered = check_invalid_corpus("fixtures/rule-ir-v2/invalid", "rule-ir-v2");
    for code in [
        profile::JDM_FUNCTION_NODE_REJECTED,
        profile::JDM_CUSTOM_NODE_REJECTED,
        profile::JDM_UNKNOWN_NODE_TYPE,
        profile::JDM_HIT_POLICY_REJECTED,
        profile::JDM_DUPLICATE_ID,
        profile::JDM_IO_NODE_COUNT,
        profile::JDM_DANGLING_EDGE,
        profile::JDM_INVALID_EDGE,
        profile::JDM_CYCLE,
        profile::JDM_UNREACHABLE_NODE,
        profile::JDM_SWITCH_DEFAULT,
        profile::JDM_SWITCH_UNCONNECTED_BRANCH,
        profile::JDM_UNKNOWN_COLUMN,
        profile::JDM_INVALID_EXPRESSION,
        profile::JDM_UNRESOLVED_IMPORT,
        profile::JDM_UNRESOLVED_SUB_DECISION,
        profile::JDM_RECURSIVE_SUB_DECISION,
        profile::INVALID_DECISION_CONTRACT,
        profile::INVALID_SCHEMA,
        norn_checker::lifecycle_writes::codes::INVALID_FACT_CHANGE,
    ] {
        assert!(covered.contains(code), "{code} has no rejection fixture");
    }
}

/// The valid `RuleIR` v2 corpus — every admitted node type and both hit
/// policies of tables and switches, and a decide graph with the decision
/// contract — checks clean.
#[test]
fn rule_ir_v2_valid_corpus_checks_clean() {
    let dir = repo_root().join("fixtures/rule-ir-v2/valid");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("fixture directory should be readable")
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "the valid RuleIR v2 corpus is empty");
    let mut node_types = std::collections::BTreeSet::new();
    let mut policies = std::collections::BTreeSet::new();
    let mut contracts = std::collections::BTreeSet::new();
    for file in files {
        let artifact = read_json(&file);
        assert_eq!(
            check_artifact_json(&artifact),
            [],
            "{} should check clean",
            file.display()
        );
        let payload = &artifact["payload"];
        contracts.insert(payload["contract"].as_str().unwrap_or_default().to_owned());
        for node in payload["graph"]["nodes"].as_array().expect("nodes") {
            let node_type = node["type"].as_str().expect("node type").to_owned();
            if let Some(policy) = node["content"]["hitPolicy"].as_str() {
                policies.insert(format!("{node_type}/{policy}"));
            }
            node_types.insert(node_type);
        }
    }
    let expected_types: std::collections::BTreeSet<String> = [
        "inputNode",
        "outputNode",
        "expressionNode",
        "decisionTableNode",
        "switchNode",
        "decisionNode",
    ]
    .map(str::to_owned)
    .into();
    assert_eq!(node_types, expected_types);
    let expected_policies: std::collections::BTreeSet<String> = [
        "decisionTableNode/first",
        "decisionTableNode/collect",
        "switchNode/first",
        "switchNode/collect",
    ]
    .map(str::to_owned)
    .into();
    assert_eq!(policies, expected_policies);
    assert_eq!(
        contracts,
        ["decide", "sub_decision"].map(str::to_owned).into()
    );
}

/// Check one invalid-fixture corpus (`dir` under the repo root, snapshots
/// under `tests/snapshots/<snapshot_dir>`) and return the codes it covers.
fn check_invalid_corpus(dir: &str, snapshot_dir: &str) -> std::collections::BTreeSet<String> {
    let dir = repo_root().join(dir);
    let mut covered = std::collections::BTreeSet::new();
    let manifest = read_json(&dir.join("manifest.json"));
    let entries = manifest
        .get("fixtures")
        .and_then(Value::as_array)
        .expect("manifest lists fixtures");
    assert!(!entries.is_empty(), "the invalid-fixture manifest is empty");

    // Every fixture file is listed, so none silently drops out.
    let mut on_disk: Vec<String> = fs::read_dir(&dir)
        .expect("fixture directory should be readable")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "manifest.json")
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = entries
        .iter()
        .map(|entry| field(entry, "file").to_owned())
        .collect();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "fixtures/invalid and its manifest disagree"
    );

    let update = std::env::var_os(UPDATE_ENV).is_some();
    let snapshots = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(snapshot_dir);
    for entry in entries {
        let file = field(entry, "file");
        let code = field(entry, "code");
        covered.insert(code.to_owned());
        let error_at = field(entry, "error_at");
        assert!(!field(entry, "reason").is_empty(), "{file}: empty reason");
        assert!(
            codes::lookup(code).is_some(),
            "{file}: {code} is unregistered"
        );

        let diagnostics = check_artifact_json(&read_json(&dir.join(file)));
        assert!(
            diagnostics.iter().any(norn_checker::Diagnostic::is_error),
            "{file}: the checker accepted an invalid fixture"
        );
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == code
                    && diagnostic
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == error_at)
            }),
            "{file}: expected {code} at {error_at}, got {diagnostics:#?}"
        );
        for diagnostic in &diagnostics {
            assert!(
                diagnostic.registration().is_some(),
                "{file}: emitted unregistered code {}",
                diagnostic.code
            );
        }

        let output = diagnostics_to_json(&diagnostics);
        let mut rendered =
            serde_json::to_string_pretty(&output).expect("diagnostics should serialize");
        rendered.push('\n');
        let snapshot = snapshots.join(file);
        if update {
            fs::create_dir_all(&snapshots).expect("snapshot directory should be creatable");
            fs::write(&snapshot, &rendered).expect("snapshot should be writable");
            continue;
        }
        // Compared as JSON values, so the check is independent of key order.
        let checked_in: Option<Value> = fs::read_to_string(&snapshot)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        assert!(
            checked_in.as_ref() == Some(&output),
            "snapshot {} is stale; regenerate with `{UPDATE_ENV}=1 cargo test -p norn-checker \
             --test diagnostic_snapshots` and review the diff\n--- rendered ---\n{rendered}",
            snapshot.display()
        );
    }
    covered
}

#[test]
fn json_output_carries_every_key_even_when_absent() {
    let document = diagnostics_to_json(&[norn_checker::Diagnostic {
        code: norn_checker::DiagnosticCode::new(codes::IR_REQUIRED_FIELD),
        severity: norn_checker::DiagnosticSeverity::Error,
        message: "namespace is required".to_owned(),
        span: None,
        artifact_id: None,
        remediation: None,
    }]);
    assert_eq!(
        document,
        serde_json::json!({
            "format": norn_checker::DIAGNOSTICS_FORMAT,
            "checkerVersion": env!("CARGO_PKG_VERSION"),
            "diagnostics": [{
                "code": "ir.validation.required_field",
                "tier": "T2",
                "severity": "error",
                "message": "namespace is required",
                "span": null,
                "artifactId": null,
                "remediation": null,
            }],
        })
    );
}
