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
        "dotted literals that are neither registered codes (conformance/diagnostic-codes/) nor \
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
         new codes in conformance/diagnostic-codes/ instead): {unbaselined:?}",
        path.display()
    );
    let stale: Vec<&&str> = baseline.difference(&listed).collect();
    assert!(
        stale.is_empty(),
        "baseline entries no longer in NON_CODE_LITERALS; remove them from {}: {stale:?}",
        path.display()
    );
}

#[path = "../build.rs"]
mod registry_build;

#[test]
fn conformance_directory_matches_the_public_manifest() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/diagnostic-codes");
    let mut files: Vec<_> = fs::read_dir(path)
        .expect("code directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    files.sort();
    let codes: Vec<serde_json::Value> = files
        .iter()
        .map(|path| {
            serde_json::from_str(&fs::read_to_string(path).expect("code bytes")).expect("code JSON")
        })
        .collect();
    assert_eq!(
        codes::conformance_manifest()["codes"],
        serde_json::json!(codes)
    );
}

#[test]
fn registry_generation_refuses_invalid_entries() {
    let base = serde_json::json!({"code":"toy.validation.invalid", "tier":"T2", "severity":"error", "message":"synthetic \"quoted\" \\ path \u{03b1}", "since":"0.2.0"});
    for change in [
        "valid",
        "identity",
        "tier",
        "severity",
        "message",
        "missing",
        "unknown",
        "duplicate-key",
        "directory",
    ] {
        let temp =
            std::env::temp_dir().join(format!("norn-registry-{}-{change}", std::process::id()));
        fs::create_dir(&temp).expect("fixture directory");
        let path = temp.join("toy.validation.invalid.json");
        let mut value = base.clone();
        match change {
            "identity" => value["code"] = serde_json::json!("other.validation.invalid"),
            "tier" => value["tier"] = serde_json::json!("T8"),
            "severity" => value["severity"] = serde_json::json!("fatal"),
            "message" => value["message"] = serde_json::json!(""),
            "missing" => {
                value.as_object_mut().expect("object").remove("since");
            }
            "unknown" => value["extra"] = serde_json::json!(true),
            _ => {}
        }
        if change == "directory" {
            fs::create_dir(&path).expect("invalid directory");
        } else {
            let text = if change == "duplicate-key" {
                format!(
                    "{{\"code\":\"toy.validation.invalid\",{}",
                    value.to_string().strip_prefix('{').expect("object prefix")
                )
            } else {
                value.to_string()
            };
            fs::write(&path, text).expect("fixture entry");
        }
        let result = registry_build::generate(&temp);
        fs::remove_dir_all(temp).expect("cleanup fixture");
        assert_eq!(result.is_ok(), change == "valid", "{change}: {result:?}");
    }
}
