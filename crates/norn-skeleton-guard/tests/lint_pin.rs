//! The lint set can only get stricter: the workspace matches `lint-pin.json`,
//! and every way of weakening it is caught on a throwaway tree.
#![allow(
    clippy::expect_used,
    reason = "test helpers fail fast on unreadable fixtures"
)]

use std::path::{Path, PathBuf};

use norn_skeleton_guard::Violation;
use norn_skeleton_guard::lint_pin::{check_lint_pin, current_pin};
use norn_skeleton_guard::ratchet::{
    INNER_ALLOW, check_ratchets, count_source, ratchet_counts, ratchets_json,
};

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

#[test]
fn workspace_lint_set_matches_pin() {
    let pin = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("lint-pin.json"))
        .expect("read lint-pin.json");
    let root = workspace_root();
    let v = check_lint_pin(&root, &pin).expect("pin readable");
    assert!(
        v.is_empty(),
        "lint set weakened:\n{}\n\ncurrent lint set (raise the pin to it only in a gated PR):\n{}",
        render(&v),
        serde_json::to_string_pretty(&current_pin(&root).expect("current")).expect("json")
    );
}

const WORKSPACE: &str = r#"[workspace]
members = []

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
"#;

const GUARD: &str = r#"[package]
name = "guard"

[lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[lints.clippy]
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
"#;

const CLIPPY: &str = r#"allow-unwrap-in-tests = true
allow-dbg-in-tests = false
too-many-arguments-threshold = 7
disallowed-methods = [
  { path = "std::process::exit", reason = "x" },
]
"#;

const MEMBER: &str = "[package]\nname = \"a\"\n\n[lints]\nworkspace = true\n";
const LIB: &str = "#![deny(clippy::disallowed_types)]\n";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn seed(root: &Path) {
    write(root, "Cargo.toml", WORKSPACE);
    write(root, "crates/norn-skeleton-guard/Cargo.toml", GUARD);
    write(root, "clippy.toml", CLIPPY);
    write(root, "crates/a/Cargo.toml", MEMBER);
    write(root, "crates/a/src/lib.rs", LIB);
}

/// Applies `edit` to `rel` (relative to a freshly seeded tree pinned before
/// the edit) and returns the violations.
fn after(rel: &str, edit: impl FnOnce(&str) -> String) -> Vec<Violation> {
    let dir = tempfile::tempdir().expect("tempdir");
    seed(dir.path());
    let pin = current_pin(dir.path()).expect("pin").to_string();
    assert_eq!(
        check_lint_pin(dir.path(), &pin).expect("ok"),
        Vec::<Violation>::new()
    );
    let text = std::fs::read_to_string(dir.path().join(rel)).expect("read");
    write(dir.path(), rel, &edit(&text));
    check_lint_pin(dir.path(), &pin).expect("ok")
}

#[test]
fn weakening_the_workspace_lints_fails_and_raising_passes() {
    let cases: [(&str, &str, &str, &[&str]); 7] = [
        (
            "Cargo.toml",
            "unwrap_used = \"deny\"",
            "unwrap_used = \"warn\"",
            &["lint-lowered"],
        ),
        (
            "Cargo.toml",
            "unsafe_code = \"forbid\"",
            "unsafe_code = \"allow\"",
            &["lint-lowered"],
        ),
        (
            "Cargo.toml",
            "unwrap_used = \"deny\"\n",
            "",
            &["lint-removed"],
        ),
        (
            "Cargo.toml",
            "priority = -1",
            "priority = 1",
            &["lint-changed"],
        ),
        (
            "Cargo.toml",
            "missing_docs = \"warn\"",
            "missing_docs = \"deny\"",
            &[],
        ),
        (
            "Cargo.toml",
            "unwrap_used = \"deny\"",
            "unwrap_used = \"deny\"\nexpect_used = \"warn\"",
            &[],
        ),
        (
            "crates/norn-skeleton-guard/Cargo.toml",
            "unwrap_used = \"deny\"",
            "unwrap_used = \"allow\"",
            &["lint-lowered"],
        ),
    ];
    for (rel, from, to, want) in cases {
        let v = after(rel, |text| text.replace(from, to));
        assert_eq!(rules_of(&v), want, "{rel}: {from} -> {to}\n{}", render(&v));
    }
}

#[test]
fn weakening_clippy_toml_fails_and_tightening_passes() {
    let cases: [(&str, &str, &[&str]); 6] = [
        (
            "allow-dbg-in-tests = false",
            "allow-dbg-in-tests = true",
            &["clippy-toml-weakened"],
        ),
        (
            "allow-dbg-in-tests = false",
            "allow-dbg-in-tests = false\nallow-print-in-tests = true",
            &["clippy-toml-weakened"],
        ),
        (
            "allow-unwrap-in-tests = true",
            "allow-unwrap-in-tests = false",
            &[],
        ),
        (
            "  { path = \"std::process::exit\", reason = \"x\" },\n",
            "",
            &["clippy-toml-weakened"],
        ),
        (
            "]\n",
            "  { path = \"std::env::var\", reason = \"y\" },\n]\n",
            &[],
        ),
        (
            "too-many-arguments-threshold = 7",
            "too-many-arguments-threshold = 12",
            &["clippy-toml-changed"],
        ),
    ];
    for (from, to, want) in cases {
        let v = after("clippy.toml", |text| text.replace(from, to));
        assert_eq!(rules_of(&v), want, "{from} -> {to}\n{}", render(&v));
    }
}

#[test]
fn crate_root_levels_and_member_lint_tables_are_held() {
    let removed = after("crates/a/src/lib.rs", |_| String::new());
    assert_eq!(
        rules_of(&removed),
        ["inner-level-lowered"],
        "{}",
        render(&removed)
    );
    let lowered = after("crates/a/src/lib.rs", |text| text.replace("deny", "warn"));
    assert_eq!(
        rules_of(&lowered),
        ["inner-level-lowered"],
        "{}",
        render(&lowered)
    );
    let raised = after("crates/a/src/lib.rs", |text| text.replace("deny", "forbid"));
    assert!(raised.is_empty(), "{}", render(&raised));
    let own = after("crates/a/Cargo.toml", |text| {
        format!("{text}\n[lints.rust]\nunsafe_code = \"allow\"\n")
    });
    assert_eq!(rules_of(&own), ["crate-lints"], "{}", render(&own));

    // Conditional `#![cfg_attr(..., deny(...))]` must NEVER satisfy a pinned floor.
    for conditional in [
        "#![cfg_attr(any(), deny(clippy::disallowed_types))]\n",
        "#![cfg_attr(feature = \"never\", deny(clippy::disallowed_types))]\n",
        "#![cfg_attr(true, deny(clippy::disallowed_types))]\n",
    ] {
        let v = after("crates/a/src/lib.rs", |_| conditional.to_owned());
        assert_eq!(
            rules_of(&v),
            ["inner-level-lowered"],
            "{conditional}\n{}",
            render(&v)
        );
    }
}

#[test]
fn shadow_clippy_toml_files_are_rejected() {
    for shadow in [
        ".clippy.toml",
        "crates/a/clippy.toml",
        "crates/a/.clippy.toml",
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        seed(dir.path());
        let pin = current_pin(dir.path()).expect("pin").to_string();
        write(dir.path(), shadow, "allow-unwrap-in-tests = true\n");
        let v = check_lint_pin(dir.path(), &pin).expect("ok");
        assert_eq!(
            rules_of(&v),
            ["shadow-clippy-toml"],
            "shadow `{shadow}`:\n{}",
            render(&v)
        );
    }
}

#[test]
fn weakening_rivet_workspace_lints_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed(dir.path());
    write(
        dir.path(),
        "crates/norn-cell-rivet/Cargo.toml",
        &format!("{MEMBER}\n{WORKSPACE}"),
    );
    let pin = current_pin(dir.path()).expect("pin").to_string();
    assert_eq!(
        check_lint_pin(dir.path(), &pin).expect("ok"),
        Vec::<Violation>::new()
    );
    let weakened = format!(
        "{MEMBER}\n{}",
        WORKSPACE.replace("unwrap_used = \"deny\"", "unwrap_used = \"warn\"")
    );
    write(dir.path(), "crates/norn-cell-rivet/Cargo.toml", &weakened);
    let v = check_lint_pin(dir.path(), &pin).expect("ok");
    assert_eq!(rules_of(&v), ["lint-lowered"], "{}", render(&v));
}

#[test]
fn inner_allows_are_counted_per_lint() {
    let source = r#"
        #![allow(clippy::unwrap_used, clippy::expect_used, reason = "x")]
        #![cfg_attr(test, expect(dead_code, reason = "x"))]
        #![deny(clippy::panic)]
        #[allow(clippy::indexing_slicing, reason = "an item allow is not crate-level")]
        fn f() {}
        mod m { #![allow(missing_docs, reason = "module-level counts too")] }
    "#;
    let counts = count_source(source, false).expect("parses");
    assert_eq!(counts.get(INNER_ALLOW), Some(&4), "{counts:?}");
}

#[test]
fn a_new_crate_level_allow_fails_the_ratchet() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "crates/a/src/lib.rs",
        "#![allow(missing_docs, reason = \"x\")]\n",
    );
    let found = ratchet_counts(dir.path()).expect("scan");
    let baseline = serde_json::json!({ "ratchets": ratchets_json(&found) }).to_string();
    assert!(
        baseline.contains("crates/a/src/lib.rs: missing_docs"),
        "{baseline}"
    );
    write(
        dir.path(),
        "crates/a/src/lib.rs",
        "#![allow(clippy::pedantic, reason = \"x\")]\n",
    );
    let v = check_ratchets(&ratchet_counts(dir.path()).expect("scan"), &baseline).expect("ok");
    assert_eq!(
        rules_of(&v),
        ["ratchet-exceeded", "ratchet-stale"],
        "{}",
        render(&v)
    );
}
