#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! The diagnostic code registry is complete and exact (T1-CHK-2, #2525).
//!
//! Codes are emitted as string literals at the emission sites (or in the
//! constants those sites use), so the check scans the crate's non-test
//! source for every dotted lower-case literal. Each one must be either a
//! registered code or an explicitly listed non-code literal (a legacy dotted
//! path or a hash domain separator), and every registered code must be
//! emitted somewhere. Neither direction can pass vacuously: a new code
//! literal fails until it is registered, and a registered code that no source
//! references fails until it is removed from the scan's reach or re-emitted.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use norn_checker::codes::{self, REGISTRY};

const UPDATE_ENV: &str = "NORN_CHECKER_UPDATE_GOLDENS";

/// Dotted lower-case literals in checker source that are not diagnostic
/// codes. Every entry must still occur in the source, and the list must equal
/// `tests/non_code_literals.baseline.json`, which CI keeps shrink-only
/// (`scripts/non-code-literals-shrink-only.sh`): a new code cannot be
/// smuggled past the registry by listing it here.
const NON_CODE_LITERALS: &[&str] = &[
    // Hash domain separators for quantity-ledger commitments.
    "norn.quantity.evaluation.v1",
    "norn.quantity.parameters.v1",
    // Legacy dotted diagnostic paths.
    "payload.approval.required",
    "payload.approval.state",
    "payload.auth.kind",
    "payload.auth.name",
    "payload.auth.placement",
    "payload.auth.scopes",
    "payload.calc.fields",
    "payload.calc.kind",
    "payload.decision.inputs",
    "payload.decision.kind",
    "payload.decision.rows",
    "payload.decision.value",
    "payload.executable.boundary",
    "payload.intent.summary",
    "payload.overlay.kind",
    "payload.overlay.operations",
    "payload.review.state",
    "payload.supersedes.version",
];

/// The registry module itself only lists codes; it emits none.
const REGISTRY_FILE: &str = "codes.rs";

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("source directory should be readable") {
        let path = entry.expect("directory entry should be readable").path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// Every dotted lower-case string literal in non-test, non-comment source,
/// with the files it occurs in.
fn dotted_source_literals() -> BTreeMap<String, BTreeSet<String>> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        !files.is_empty(),
        "no checker source found under {}",
        src.display()
    );

    let literal = regex_lite::Regex::new(r#""([a-z][a-z0-9_]*(?:\.[a-z0-9_]+){2,})""#)
        .expect("literal pattern should compile");
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        if file.file_name().is_some_and(|name| name == REGISTRY_FILE) {
            continue;
        }
        let text = fs::read_to_string(&file).expect("source file should be readable");
        let display = file
            .strip_prefix(&src)
            .expect("file is under src")
            .display()
            .to_string();
        for line in text.lines() {
            // Unit-test modules sit at the end of each file.
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            for capture in literal.captures_iter(line) {
                if let Some(code) = capture.get(1) {
                    found
                        .entry(code.as_str().to_owned())
                        .or_default()
                        .insert(display.clone());
                }
            }
        }
    }
    found
}

#[test]
fn every_emitted_code_is_registered() {
    let found = dotted_source_literals();
    let unregistered: Vec<String> = found
        .iter()
        .filter(|(literal, _)| {
            codes::lookup(literal).is_none() && !NON_CODE_LITERALS.contains(&literal.as_str())
        })
        .map(|(literal, files)| format!("{literal} ({files:?})"))
        .collect();
    assert!(
        unregistered.is_empty(),
        "dotted literals that are neither registered codes (src/codes.rs REGISTRY) nor \
         listed non-code literals:\n{}",
        unregistered.join("\n")
    );
}

#[test]
fn every_registered_code_is_emitted() {
    let found = dotted_source_literals();
    let unreferenced: Vec<&str> = REGISTRY
        .iter()
        .map(|entry| entry.code)
        .filter(|code| !found.contains_key(*code))
        .collect();
    assert!(
        unreferenced.is_empty(),
        "registered codes no checker source emits: {unreferenced:?}"
    );
    let stale: Vec<&&str> = NON_CODE_LITERALS
        .iter()
        .filter(|literal| !found.contains_key(**literal))
        .collect();
    assert!(
        stale.is_empty(),
        "non-code literals no longer in source: {stale:?}"
    );
    assert!(
        NON_CODE_LITERALS
            .iter()
            .all(|literal| codes::lookup(literal).is_none()),
        "a registered code is also listed as a non-code literal"
    );
}

#[test]
fn non_code_literals_match_the_shrink_only_baseline() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/non_code_literals.baseline.json");
    let baseline: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&path).expect("non-code literal baseline should be readable"),
    )
    .expect("non-code literal baseline should be JSON");
    let baseline: BTreeSet<&str> = baseline
        .get("non_code_literals")
        .and_then(serde_json::Value::as_array)
        .expect("baseline lists non_code_literals")
        .iter()
        .map(|literal| literal.as_str().expect("baseline entries are strings"))
        .collect();
    let listed: BTreeSet<&str> = NON_CODE_LITERALS.iter().copied().collect();
    assert_eq!(
        listed.len(),
        NON_CODE_LITERALS.len(),
        "NON_CODE_LITERALS has duplicates"
    );
    let unbaselined: Vec<&&str> = listed.difference(&baseline).collect();
    assert!(
        unbaselined.is_empty(),
        "NON_CODE_LITERALS entries missing from {} (the baseline is shrink-only; register \
         new codes in src/codes.rs instead): {unbaselined:?}",
        path.display()
    );
    let stale: Vec<&&str> = baseline.difference(&listed).collect();
    assert!(
        stale.is_empty(),
        "baseline entries no longer in NON_CODE_LITERALS; remove them from {}: {stale:?}",
        path.display()
    );
}

#[test]
fn conformance_manifest_is_checked_in_and_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/diagnostic-codes.json");
    let manifest = codes::conformance_manifest();
    if std::env::var_os(UPDATE_ENV).is_some() {
        let mut rendered =
            serde_json::to_string_pretty(&manifest).expect("manifest should serialize");
        rendered.push('\n');
        fs::write(&path, rendered).expect("manifest should be writable");
        return;
    }
    // Compared as JSON values, so the check is independent of key order.
    let checked_in: Option<serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    assert!(
        checked_in.as_ref() == Some(&manifest),
        "{} is stale; regenerate with `{UPDATE_ENV}=1 cargo test -p norn-checker --test code_registry`",
        path.display()
    );
}
