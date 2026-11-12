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
    // Tool diagnostics have a separate emission authority; checker codes still
    // require a Rust emission regardless of TypeScript mentions.
    let tool = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/norn-ts");
    #[allow(
        clippy::disallowed_methods,
        reason = "test-only scan of the TypeScript CLI diagnostics; not a runtime capability"
    )]
    let output = std::process::Command::new("bun")
        .arg(tool.join("src/cli-code-registry.ts"))
        .output()
        .expect("Bun is required to validate CLI diagnostic emission sites");
    assert!(
        output.status.success(),
        "CLI emission scanner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let codes: Vec<String> = serde_json::from_slice(&output.stdout).expect("CLI emissions JSON");
    add_tool_emissions(&mut found, codes);
    found
}

fn add_tool_emissions(found: &mut BTreeMap<String, BTreeSet<String>>, codes: Vec<String>) {
    for code in codes {
        if code.starts_with("cli.") {
            found
                .entry(code)
                .or_default()
                .insert("tools/norn-ts".to_owned());
        }
    }
}

#[test]
fn checker_code_only_mentioned_in_typescript_is_not_emitted() {
    let mut found = BTreeMap::new();
    add_tool_emissions(&mut found, vec!["entity.validation.only_in_ts".to_owned()]);
    assert!(!found.contains_key("entity.validation.only_in_ts"));
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
    let mut codes: Vec<serde_json::Value> = files
        .iter()
        .map(|path| {
            serde_json::from_str(&fs::read_to_string(path).expect("code bytes")).expect("code JSON")
        })
        .collect();
    let guidance: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../conformance/diagnostic-guidance.json"))
            .expect("guidance");
    for code in &mut codes {
        let guide = guidance
            .iter()
            .find(|guide| guide.get("code") == code.get("code"))
            .expect("registered guidance");
        let fields = code.as_object_mut().expect("code object");
        for key in ["fix", "no_fix"] {
            if let Some(value) = guide.get(key) {
                fields.insert(key.to_owned(), value.clone());
            }
        }
    }
    assert_eq!(
        codes::conformance_manifest()["codes"],
        serde_json::json!(codes)
    );
}

#[test]
fn registry_generation_refuses_invalid_entries() {
    let base = serde_json::json!({"code":"toy.validation.invalid", "tier":"T2", "severity":"error", "message":"synthetic \"quoted\" \\ path \u{03b1}", "since":"0.2.0", "fix":"Correct the synthetic declaration"});
    for change in [
        "valid",
        "no_fix",
        "missing-guidance",
        "empty-guidance",
        "duplicate-guidance",
        "orphan-guidance",
        "both-guidance",
        "blank-guidance",
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
        let directory = temp.join("codes");
        fs::create_dir(&directory).expect("code directory");
        let path = directory.join("toy.validation.invalid.json");
        let mut value = base.clone();
        match change {
            "no_fix" => {
                value.as_object_mut().expect("object").remove("fix");
                value["no_fix"] = "No supported alternative exists".into();
            }
            "missing-guidance" => {
                value.as_object_mut().expect("object").remove("fix");
            }
            "both-guidance" => value["no_fix"] = "Conflicting guidance".into(),
            "blank-guidance" => value["fix"] = " ".into(),
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
        let mut guide = serde_json::json!({"code":"toy.validation.invalid"});
        for key in ["fix", "no_fix"] {
            if let Some(v) = value.as_object_mut().expect("object").remove(key) {
                guide
                    .as_object_mut()
                    .expect("guide object")
                    .insert(key.to_owned(), v);
            }
        }
        let guidance = temp.join("guidance.json");
        let guides = match change {
            "empty-guidance" => serde_json::json!([]),
            "duplicate-guidance" => serde_json::json!([guide, guide]),
            "orphan-guidance" => {
                serde_json::json!([guide, {"code":"toy.other.missing", "no_fix":"No matching declaration"}])
            }
            _ => serde_json::json!([guide]),
        };
        fs::write(&guidance, guides.to_string()).expect("guidance fixture");
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
        let result = registry_build::generate(&directory, &guidance);
        fs::remove_dir_all(temp).expect("cleanup fixture");
        assert_eq!(
            result.is_ok(),
            matches!(change, "valid" | "no_fix"),
            "{change}: {result:?}"
        );
    }
}

#[test]
fn cycle_advice_is_reserved_for_cycle_diagnostics() {
    for entry in REGISTRY {
        if entry
            .fix
            .is_some_and(|fix| fix.contains("cyclic dependency"))
        {
            let detail = entry.code.rsplit('.').next().expect("code detail");
            assert!(
                detail == "cycle" || detail.ends_with("_cycle"),
                "{} has unrelated cycle advice",
                entry.code
            );
        }
    }
}

#[test]
fn registry_is_strictly_ordered_for_binary_search() {
    assert!(
        REGISTRY
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a.code < b.code))
    );
    assert!(
        REGISTRY
            .iter()
            .all(|entry| codes::lookup(entry.code).is_some())
    );
}

/// A `fix` used by codes of more than one family (the first code segment) is a
/// generic sentence. Each one is listed here with the detail segments it fits,
/// so generic advice cannot reach a code it is wrong for: an integrity
/// mismatch must never be told to "correct the member" that holds the hash.
/// A code with no accurate guidance yet carries a `no_fix` that says so.
const SHARED_FIXES: [(&str, &str); 8] = [
    (
        "Correct the member at the reported path to match its declared type, schema and constraints.",
        r"^invalid(_|$)",
    ),
    (
        "Include the referenced declaration at its exact required identity, kind and version in the effective definition set.",
        r"^(unknown|unresolved|undeclared|dangling)(_|$)",
    ),
    (
        "Keep one declaration per reported identity; remove or rename the duplicate at the reported path.",
        r"duplicate",
    ),
    (
        "Declare the missing member at the reported path using the owning definition schema.",
        r"^missing(_|$)|required",
    ),
    (
        "Replace the unsupported construct with a declaration admitted by the selected profile and run admission again.",
        r"unsupported",
    ),
    (
        "Remove the reported cyclic dependency or reference; declare an acyclic graph and recheck the complete definition set.",
        r"cycle|cyclic",
    ),
    (
        "Reduce the reported declaration or traversal footprint to the selected profile limit and check the complete set again.",
        r"limit|budget|exhausted|too_long|too_many|exceeded",
    ),
    (
        "Reference an existing declared lifecycle property, state or transition with its exact identity.",
        r"lifecycle|transition|state",
    ),
];

fn detail(code: &str) -> &str {
    code.splitn(3, '.').nth(2).expect("code detail")
}

#[test]
fn shared_fix_text_is_listed_and_fits_every_code_that_carries_it() {
    let mut families: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for entry in REGISTRY {
        if let Some(fix) = entry.fix {
            families
                .entry(fix)
                .or_default()
                .insert(entry.code.split('.').next().expect("code family"));
        }
    }
    for entry in REGISTRY {
        let Some(fix) = entry.fix else { continue };
        let listed = SHARED_FIXES.iter().find(|(text, _)| *text == fix);
        if families.get(fix).is_some_and(|family| family.len() > 1) {
            assert!(
                listed.is_some(),
                "{} shares its fix with a code of another family; write specific guidance or list the sentence in SHARED_FIXES: {fix}",
                entry.code
            );
        }
        if let Some((_, pattern)) = listed {
            let fits = regex_lite::Regex::new(pattern).expect("detail pattern");
            assert!(
                fits.is_match(detail(entry.code)),
                "{} carries generic advice that does not fit it: {fix}",
                entry.code
            );
        }
    }
}

/// The words that mark a code as being about integrity: a recorded value that
/// must agree with what it was derived from.
const INTEGRITY_WORDS: [&str; 7] = [
    "mismatch",
    "hash",
    "digest",
    "stale",
    "certificate",
    "receipt",
    "evidence",
];

/// Format errors on a field the author writes by hand (a URI and a hex digest
/// that must be well formed): correcting the member is the right advice there.
const INTEGRITY_FORMAT_CODES: [&str; 1] = ["source_schema.validation.invalid_evidence"];

/// An integrity mismatch is never repaired by editing the recorded value, so
/// no generic sentence may sit on a code about a mismatch, a hash or digest,
/// stale state, a certificate, a receipt or evidence.
#[test]
fn integrity_codes_carry_no_generic_fix() {
    for entry in REGISTRY {
        // The category and the detail, as in `ledger.certificate.claim_mismatch`.
        let subject = entry.code.split_once('.').map_or("", |(_, rest)| rest);
        let integrity = INTEGRITY_WORDS.iter().any(|word| subject.contains(word));
        if integrity && !INTEGRITY_FORMAT_CODES.contains(&entry.code) {
            assert!(
                !SHARED_FIXES
                    .iter()
                    .any(|(text, _)| entry.fix == Some(*text)),
                "{} is an integrity code with generic advice",
                entry.code
            );
        }
    }
    for (code, forbidden) in [
        ("ledger.certificate.hash_mismatch", "Correct the member"),
        ("mapping.model.source_hash_mismatch", "Correct the member"),
        ("function.receipt.dispatch_mismatch", "Correct the member"),
        ("ledger.commit.evidence_mismatch", "Correct the member"),
    ] {
        let entry = codes::lookup(code).expect("registered integrity code");
        let advice = entry.fix.or(entry.no_fix).expect("guidance");
        assert!(!advice.contains(forbidden), "{code}: {advice}");
        assert!(
            advice.contains("Never") || advice.contains("Regenerate"),
            "{code} must say to regenerate or re-obtain, never to edit: {advice}"
        );
    }
}

/// The one sentence a code carries when no specific guidance is written for it
/// yet. It is a marker, not advice: exact, used only as `no_fix`, and no other
/// guidance may read like it, so a reviewed `no_fix` and the marker cannot blur.
const NO_GUIDANCE_YET: &str =
    "No specific guidance is recorded for this code yet; the message and the reported path say what was refused.";

#[test]
fn the_no_guidance_marker_is_one_exact_sentence() {
    let mut marked = 0_usize;
    for entry in REGISTRY {
        assert_ne!(entry.fix, Some(NO_GUIDANCE_YET), "{}", entry.code);
        if entry.no_fix == Some(NO_GUIDANCE_YET) {
            marked = marked.saturating_add(1);
            continue;
        }
        let advice = entry.fix.or(entry.no_fix).expect("guidance");
        assert!(
            !advice.contains("No specific guidance") && !advice.contains("from this code alone"),
            "{} reads like the marker without being it: {advice}",
            entry.code
        );
    }
    assert!(marked > 0, "the marker is unused");
}

fn source_files(dir: &Path, extension: &str, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("source directory should be readable") {
        let path = entry.expect("directory entry should be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if name != "node_modules" && name != "target" {
                source_files(&path, extension, files);
            }
        } else if path.extension().is_some_and(|found| found == extension) {
            files.push(path);
        }
    }
}

/// Guidance that names an environment variable must name one the code reads:
/// following a fix that sets a variable nothing reads leaves the error in place.
#[test]
fn every_environment_variable_named_in_guidance_is_in_the_source() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    source_files(&root.join("crates"), "rs", &mut files);
    source_files(&root.join("tools/norn-ts/src"), "ts", &mut files);
    let source: String = files
        .iter()
        .map(|path| fs::read_to_string(path).expect("source file should be readable"))
        .collect();
    let variable = regex_lite::Regex::new(r"NORN_[A-Z0-9_]+").expect("variable pattern");
    let mut named = BTreeSet::new();
    for entry in REGISTRY {
        for text in [Some(entry.message), entry.fix, entry.no_fix]
            .into_iter()
            .flatten()
        {
            named.extend(variable.find_iter(text).map(|found| found.as_str()));
        }
    }
    assert!(
        named.contains("NORN_DEV_HOST_BIN") && named.contains("NORN_CHECKER_WASM_PKG"),
        "the scan found no variables: {named:?}"
    );
    for name in named {
        // The name as a whole word: `NORN_HOST_BIN` is not inside `NORN_DEV_HOST_BIN`.
        let bytes = source.as_bytes();
        let whole = source.match_indices(name).any(|(at, _)| {
            let before = at.checked_sub(1).and_then(|index| bytes.get(index));
            let after = at
                .checked_add(name.len())
                .and_then(|index| bytes.get(index));
            ![before, after]
                .into_iter()
                .flatten()
                .any(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        });
        assert!(whole, "guidance names {name}, which no source reads");
    }
}
