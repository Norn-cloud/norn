//! Per-file non-test LOC cap and shrink-only baseline canaries (#1612).
#![allow(
    clippy::expect_used,
    reason = "test helpers fail fast on unreadable fixtures"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use norn_skeleton_guard::Violation;
use norn_skeleton_guard::loc_cap::{
    MAX_NON_TEST_LOC, check_loc_cap, loc_baseline_json, loc_counts, non_test_loc,
};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn render(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn rules_of(violations: &[Violation]) -> Vec<&'static str> {
    violations.iter().map(|v| v.rule).collect()
}

fn repeat_lines(count: usize) -> String {
    let mut out = String::new();
    for idx in 0..count {
        writeln!(out, "const LINE_{idx}: usize = {idx};").expect("write line");
    }
    out
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn workspace_non_test_loc_matches_cap_and_baseline() {
    let baseline =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("loc-baseline.json"))
            .expect("read loc-baseline.json");
    let found = loc_counts(&workspace_root()).expect("scan crates/");
    let v = check_loc_cap(&found, &baseline).expect("parse loc-baseline.json");
    assert!(
        v.is_empty(),
        "LOC cap violations:\n{}\n\ncurrent files over {MAX_NON_TEST_LOC} non-test LOC:\n{}",
        render(&v),
        serde_json::to_string_pretty(&loc_baseline_json(&found)).expect("json")
    );
}

#[test]
fn new_file_over_cap_fails_and_at_cap_passes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let baseline = loc_baseline_json(&BTreeMap::new()).to_string();

    write_file(
        dir.path(),
        "crates/demo/src/at_cap.rs",
        &repeat_lines(MAX_NON_TEST_LOC),
    );
    let ok = check_loc_cap(&loc_counts(dir.path()).expect("scan"), &baseline).expect("check");
    assert!(ok.is_empty(), "{}", render(&ok));

    write_file(
        dir.path(),
        "crates/demo/src/over_cap.rs",
        &repeat_lines(MAX_NON_TEST_LOC.saturating_add(1)),
    );
    let bad = check_loc_cap(&loc_counts(dir.path()).expect("scan"), &baseline).expect("check");
    assert_eq!(rules_of(&bad), ["loc-cap-exceeded"], "{}", render(&bad));
    assert!(
        bad.first()
            .is_some_and(|v| v.message.contains("crates/demo/src/over_cap.rs")),
        "{}",
        render(&bad)
    );
}

#[test]
fn allowlisted_file_growing_by_one_line_fails_and_shrinking_passes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ceiling = MAX_NON_TEST_LOC.saturating_add(25);
    let rel = "crates/demo/src/legacy.rs";
    write_file(dir.path(), rel, &repeat_lines(ceiling));

    let initial = loc_counts(dir.path()).expect("scan");
    assert_eq!(initial.get(rel), Some(&ceiling));
    let baseline = loc_baseline_json(&initial).to_string();
    assert_eq!(
        check_loc_cap(&initial, &baseline).expect("check"),
        Vec::<Violation>::new()
    );

    // Growing by +1 line fails closed.
    write_file(dir.path(), rel, &repeat_lines(ceiling.saturating_add(1)));
    let grew = check_loc_cap(&loc_counts(dir.path()).expect("scan"), &baseline).expect("check");
    assert_eq!(rules_of(&grew), ["loc-cap-exceeded"], "{}", render(&grew));
    assert!(
        grew.first()
            .is_some_and(|v| v.message.contains(&ceiling.to_string())),
        "{}",
        render(&grew)
    );

    // Shrinking by -1 line is accepted.
    write_file(dir.path(), rel, &repeat_lines(ceiling.saturating_sub(1)));
    let shrunk = check_loc_cap(&loc_counts(dir.path()).expect("scan"), &baseline).expect("check");
    assert!(shrunk.is_empty(), "{}", render(&shrunk));

    // Dropping below the 1,000 cap is also accepted.
    write_file(dir.path(), rel, &repeat_lines(10));
    let tiny = check_loc_cap(&loc_counts(dir.path()).expect("scan"), &baseline).expect("check");
    assert!(tiny.is_empty(), "{}", render(&tiny));
}

#[test]
fn non_test_loc_excludes_cfg_test_items_and_test_files_accurately() {
    // Inline #[cfg(test)] mod tests at the end does not count toward non-test LOC.
    let mut with_tail_tests = repeat_lines(MAX_NON_TEST_LOC);
    with_tail_tests.push_str("#[cfg(test)]\nmod tests {\n");
    with_tail_tests.push_str(&repeat_lines(500));
    with_tail_tests.push_str("}\n");
    assert_eq!(non_test_loc(&with_tail_tests), MAX_NON_TEST_LOC);

    // An external `#[cfg(test)] mod sub_tests;` near the top must NOT hide
    // production code below it.
    let top_decl = format!(
        "#[cfg(test)]\nmod sub_tests;\n{}",
        repeat_lines(MAX_NON_TEST_LOC.saturating_add(5))
    );
    assert_eq!(non_test_loc(&top_decl), MAX_NON_TEST_LOC.saturating_add(5));

    // A `#[cfg(test)]` string inside a comment or string literal must not stop counting.
    let commented = format!(
        "// #[cfg(test)]\nconst S: &str = \"#[cfg(test)]\\nmod fake {{}}\";\n{}",
        repeat_lines(10)
    );
    assert_eq!(non_test_loc(&commented), 12);

    // Integration tests under tests/ and #[cfg(test)]-only submodules are excluded.
    let dir = tempfile::tempdir().expect("tempdir");
    write_file(
        dir.path(),
        "crates/demo/src/lib.rs",
        "#[cfg(test)]\nmod helper_tests;\npub fn ok() {}\n",
    );
    write_file(
        dir.path(),
        "crates/demo/src/helper_tests.rs",
        &repeat_lines(1_500),
    );
    write_file(
        dir.path(),
        "crates/demo/tests/integration.rs",
        &repeat_lines(2_000),
    );
    let counts = loc_counts(dir.path()).expect("scan");
    assert_eq!(counts.get("crates/demo/src/lib.rs"), Some(&1));
    assert!(!counts.contains_key("crates/demo/src/helper_tests.rs"));
    assert!(!counts.contains_key("crates/demo/tests/integration.rs"));
}

#[test]
fn malformed_loc_baselines_are_refused() {
    let found = BTreeMap::new();
    for bad in [
        json!({}),
        json!({ "max_non_test_loc": 1_500, "files": {} }),
        json!({ "max_non_test_loc": MAX_NON_TEST_LOC }),
        json!({ "max_non_test_loc": MAX_NON_TEST_LOC, "files": { "crates/a/src/lib.rs": 1_000 } }),
        json!({ "max_non_test_loc": MAX_NON_TEST_LOC, "files": { "crates/a/src/lib.rs": 0 } }),
    ] {
        assert!(
            check_loc_cap(&found, &bad.to_string()).is_err(),
            "{bad} must be refused"
        );
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only guard exercising its own CI shrink-only shell gate in a temp git repo"
)]
fn run_shrink_gate(repo: &Path) -> (bool, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/baseline-shrink-only.sh");
    let output = std::process::Command::new("bash")
        .arg(script)
        .arg("HEAD")
        .current_dir(repo)
        .output()
        .expect("run baseline-shrink-only.sh");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[expect(
    clippy::disallowed_methods,
    reason = "initialize a disposable git repo to test baseline-shrink-only.sh"
)]
fn init_shrink_repo(
    base_baseline: &serde_json::Value,
    base_loc: &serde_json::Value,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_file(
        root,
        "crates/norn-skeleton-guard/baseline.json",
        &base_baseline.to_string(),
    );
    write_file(
        root,
        "crates/norn-skeleton-guard/loc-baseline.json",
        &base_loc.to_string(),
    );
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test"][..],
        &["add", "."][..],
        &["commit", "--quiet", "-m", "base"][..],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git")
                .success()
        );
    }
    dir
}

#[test]
fn shrink_only_script_rejects_loc_ratchet_and_ui_growth() {
    let base_baseline = json!({
        "norn_edges": [],
        "third_party": [],
        "norn_ir_validators": [],
        "ring0_ui_modules": ["crates/norn-checker/src/ui_view.rs"],
        "ratchets": {
            "ignore-without-issue": {},
            "test-sleep": {},
            "test-without-assertion": {},
            "too-many-lines-allow": { "crates/a/src/lib.rs": 2 },
            "inner-allow": {}
        }
    });
    let base_loc = json!({
        "max_non_test_loc": MAX_NON_TEST_LOC,
        "files": { "crates/a/src/lib.rs": 1_200 }
    });
    let dir = init_shrink_repo(&base_baseline, &base_loc);
    let root = dir.path();

    let (ok, out) = run_shrink_gate(root);
    assert!(ok, "{out}");

    // Shrinking ceilings, ratchet counts, and UI lists passes.
    write_file(
        root,
        "crates/norn-skeleton-guard/baseline.json",
        &json!({
            "norn_edges": [],
            "third_party": [],
            "norn_ir_validators": [],
            "ring0_ui_modules": [],
            "ratchets": {
                "ignore-without-issue": {},
                "test-sleep": {},
                "test-without-assertion": {},
                "too-many-lines-allow": { "crates/a/src/lib.rs": 1 },
                "inner-allow": {}
            }
        })
        .to_string(),
    );
    write_file(
        root,
        "crates/norn-skeleton-guard/loc-baseline.json",
        &json!({ "max_non_test_loc": MAX_NON_TEST_LOC, "files": { "crates/a/src/lib.rs": 1_150 } })
            .to_string(),
    );
    let (ok, out) = run_shrink_gate(root);
    assert!(ok, "{out}");

    // Raising a loc-baseline ceiling or adding a new file fails.
    write_file(
        root,
        "crates/norn-skeleton-guard/loc-baseline.json",
        &json!({ "max_non_test_loc": MAX_NON_TEST_LOC, "files": { "crates/a/src/lib.rs": 1_201 } })
            .to_string(),
    );
    let (ok, out) = run_shrink_gate(root);
    assert!(!ok && out.contains("ceiling rose"), "{out}");

    write_file(
        root,
        "crates/norn-skeleton-guard/loc-baseline.json",
        &json!({
            "max_non_test_loc": MAX_NON_TEST_LOC,
            "files": { "crates/a/src/lib.rs": 1_200, "crates/b/src/lib.rs": 1_050 }
        })
        .to_string(),
    );
    let (ok, out) = run_shrink_gate(root);
    assert!(!ok && out.contains("new entry"), "{out}");

    // Adding a UI module to ring0_ui_modules fails.
    write_file(
        root,
        "crates/norn-skeleton-guard/loc-baseline.json",
        &base_loc.to_string(),
    );
    write_file(
        root,
        "crates/norn-skeleton-guard/baseline.json",
        &json!({
            "norn_edges": [],
            "third_party": [],
            "norn_ir_validators": [],
            "ring0_ui_modules": [
                "crates/norn-checker/src/ui_view.rs",
                "crates/norn-checker/src/ui_new.rs"
            ],
            "ratchets": base_baseline["ratchets"]
        })
        .to_string(),
    );
    let (ok, out) = run_shrink_gate(root);
    assert!(!ok && out.contains("ring0_ui_modules"), "{out}");
}

#[test]
fn shrink_only_script_rejects_empty_and_malformed_json() {
    let base_baseline = json!({
        "norn_edges": [],
        "third_party": [],
        "norn_ir_validators": [],
        "ring0_ui_modules": ["crates/norn-checker/src/ui_view.rs"],
        "ratchets": {
            "ignore-without-issue": {},
            "test-sleep": {},
            "test-without-assertion": {},
            "too-many-lines-allow": { "crates/a/src/lib.rs": 2 },
            "inner-allow": {}
        }
    });
    let base_loc = json!({
        "max_non_test_loc": MAX_NON_TEST_LOC,
        "files": { "crates/a/src/lib.rs": 1_200 }
    });
    let dir = init_shrink_repo(&base_baseline, &base_loc);
    let root = dir.path();

    // Empty, malformed, or wrong-typed JSON files must fail closed standalone.
    for (rel, bad_payload) in [
        ("crates/norn-skeleton-guard/loc-baseline.json", ""),
        ("crates/norn-skeleton-guard/loc-baseline.json", "null"),
        ("crates/norn-skeleton-guard/loc-baseline.json", "{}"),
        (
            "crates/norn-skeleton-guard/loc-baseline.json",
            r#"{"max_non_test_loc": 1000, "files": null}"#,
        ),
        (
            "crates/norn-skeleton-guard/loc-baseline.json",
            r#"{"max_non_test_loc": "1000", "files": {}}"#,
        ),
        ("crates/norn-skeleton-guard/baseline.json", ""),
        ("crates/norn-skeleton-guard/baseline.json", "null"),
        ("crates/norn-skeleton-guard/baseline.json", "{}"),
        (
            "crates/norn-skeleton-guard/baseline.json",
            r#"{"norn_edges":[],"third_party":[],"norn_ir_validators":[],"ring0_ui_modules":null,"ratchets":{}}"#,
        ),
    ] {
        write_file(
            root,
            "crates/norn-skeleton-guard/baseline.json",
            &base_baseline.to_string(),
        );
        write_file(
            root,
            "crates/norn-skeleton-guard/loc-baseline.json",
            &base_loc.to_string(),
        );
        write_file(root, rel, bad_payload);
        let (ok, out) = run_shrink_gate(root);
        assert!(
            !ok,
            "{rel} with `{bad_payload}` must fail closed; got:\n{out}"
        );
    }
}

#[test]
fn non_item_cfg_test_attributes_cannot_swallow_production_lines() {
    let over = MAX_NON_TEST_LOC.saturating_add(5);
    let body = repeat_lines(over);

    // Struct field `#[cfg(test)]` followed by >1,000 production lines.
    let struct_field = format!("struct S {{\n    #[cfg(test)]\n    _x: (),\n}}\n{body}");
    assert!(
        non_test_loc(&struct_field) > MAX_NON_TEST_LOC,
        "struct field #[cfg(test)] leaked: counted {}",
        non_test_loc(&struct_field)
    );

    // Enum variant `#[cfg(test)]` followed by >1,000 production lines.
    let enum_variant = format!("enum E {{\n    #[cfg(test)]\n    Variant,\n}}\n{body}");
    assert!(
        non_test_loc(&enum_variant) > MAX_NON_TEST_LOC,
        "enum variant #[cfg(test)] leaked: counted {}",
        non_test_loc(&enum_variant)
    );

    // Function parameter `#[cfg(test)]` followed by >1,000 lines in fn body.
    let mut fn_body = String::new();
    for idx in 0..over {
        writeln!(fn_body, "    let _v_{idx} = {idx};").expect("write stmt");
    }
    let fn_param = format!("pub fn huge(#[cfg(test)] _x: ()) {{\n{fn_body}}}\n");
    assert!(
        non_test_loc(&fn_param) > MAX_NON_TEST_LOC,
        "fn parameter #[cfg(test)] leaked: counted {}",
        non_test_loc(&fn_param)
    );

    // Closure parameter `#[cfg(test)]` followed by >1,000 lines in closure body.
    let closure_param =
        format!("pub fn wrap() {{\n    let _f = |#[cfg(test)] _x: ()| {{\n{fn_body}    }};\n}}\n");
    assert!(
        non_test_loc(&closure_param) > MAX_NON_TEST_LOC,
        "closure parameter #[cfg(test)] leaked: counted {}",
        non_test_loc(&closure_param)
    );

    // Same-line production code next to a `#[cfg(test)]` item still counts.
    let same_line =
        "const A: usize = 1; #[cfg(test)] mod t {}\n#[cfg(test)] mod u {} const B: usize = 2;\n";
    assert_eq!(non_test_loc(same_line), 2);
}

#[test]
fn dual_included_and_src_tests_production_files_are_not_exempted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let baseline = loc_baseline_json(&BTreeMap::new()).to_string();
    let over_body = repeat_lines(MAX_NON_TEST_LOC.saturating_add(5));

    // Dual-included production file: `pub mod huge;` + `#[cfg(test)] #[path = "huge.rs"] mod _alias;`.
    write_file(
        dir.path(),
        "crates/foo/src/lib.rs",
        "pub mod huge;\n#[cfg(test)]\n#[path = \"huge.rs\"]\nmod _alias;\npub mod tests;\n",
    );
    write_file(dir.path(), "crates/foo/src/huge.rs", &over_body);

    // Production file under `crates/foo/src/tests/big.rs` reached via `pub mod tests;`.
    write_file(dir.path(), "crates/foo/src/tests/mod.rs", "pub mod big;\n");
    write_file(dir.path(), "crates/foo/src/tests/big.rs", &over_body);

    let counts = loc_counts(dir.path()).expect("scan");
    assert!(
        counts
            .get("crates/foo/src/huge.rs")
            .is_some_and(|loc| *loc > MAX_NON_TEST_LOC),
        "dual-included file was wrongly exempted: {counts:?}"
    );
    assert!(
        counts
            .get("crates/foo/src/tests/big.rs")
            .is_some_and(|loc| *loc > MAX_NON_TEST_LOC),
        "src/tests/big.rs production file was wrongly exempted: {counts:?}"
    );

    let violations = check_loc_cap(&counts, &baseline).expect("check");
    assert_eq!(
        rules_of(&violations),
        ["loc-cap-exceeded", "loc-cap-exceeded"],
        "{}",
        render(&violations)
    );
    assert!(
        violations
            .iter()
            .any(|v| v.message.contains("crates/foo/src/huge.rs")),
        "{}",
        render(&violations)
    );
    assert!(
        violations
            .iter()
            .any(|v| v.message.contains("crates/foo/src/tests/big.rs")),
        "{}",
        render(&violations)
    );
}
