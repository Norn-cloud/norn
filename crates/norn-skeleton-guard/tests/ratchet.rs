//! Shrink-only ratchets: the workspace matches `baseline.json` `ratchets`, each
//! rule counts what it documents, a seeded violation fails and a removed one
//! forces the baseline down.
#![allow(
    clippy::expect_used,
    reason = "test helpers fail fast on unreadable fixtures"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_skeleton_guard::Violation;
use norn_skeleton_guard::ratchet::{
    IGNORE_WITHOUT_ISSUE, INNER_ALLOW, RatchetCounts, TEST_SLEEP, TEST_WITHOUT_ASSERTION,
    TOO_MANY_LINES_ALLOW, check_ratchets, count_source, links_issue, ratchet_counts, ratchets_json,
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

fn count(source: &str, test_file: bool, rule: &str) -> usize {
    count_source(source, test_file)
        .expect("parses")
        .get(rule)
        .copied()
        .unwrap_or(0)
}

#[test]
fn workspace_ratchets_match_baseline() {
    let baseline =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline.json"))
            .expect("read baseline.json");
    let found = ratchet_counts(&workspace_root()).expect("scan crates/");
    let v = check_ratchets(&found, &baseline).expect("parse baseline");
    assert!(
        v.is_empty(),
        "ratchet drift:\n{}\n\ncounts found now (lower baseline.json `ratchets` to these; never raise them):\n{}",
        render(&v),
        serde_json::to_string_pretty(&ratchets_json(&found)).expect("json")
    );
}

#[test]
fn ignore_needs_an_issue_link() {
    let source = r##"
        #[test] #[ignore] fn a() { assert!(f()); }
        #[test] #[ignore = "slow"] fn b() { assert!(f()); }
        #[test] #[ignore = "#1418: needs a cell"] fn c() { assert!(f()); }
        #[test] #[ignore = "https://github.com/Norn-cloud/norn/issues/1418"] fn d() { assert!(f()); }
        #[test] #[cfg_attr(miri, ignore)] fn e() { assert!(f()); }
        #[test] #[cfg_attr(miri, ignore = "see #77")] fn g() { assert!(f()); }
        proptest! { #[test] #[ignore] fn h(x in 0..1u8) { prop_assert!(x < 1); } }
    "##;
    assert_eq!(count(source, true, IGNORE_WITHOUT_ISSUE), 4);
    assert!(links_issue("#1"));
    assert!(!links_issue("# not an issue"));
}

#[test]
fn sleeps_count_only_in_test_code() {
    let source = r"
        use std::thread::sleep as nap;
        fn production() { std::thread::sleep(D); tokio::time::sleep(D); }
        #[test] fn a() { std::thread::sleep(D); assert!(f()); }
        #[tokio::test] async fn b() { tokio::time::sleep(D).await; nap(D); assert!(f()); }
        #[cfg(test)] mod tests {
            fn helper() { thread::sleep(D); }
            #[test] fn c() { tokio::select! { _ = time::sleep(D) => {} } assert!(f()); }
        }
    ";
    assert_eq!(count(source, false, TEST_SLEEP), 5);
    assert_eq!(
        count("fn f() { std::thread::sleep(D); }", true, TEST_SLEEP),
        1
    );
}

#[test]
fn tests_must_check_something() {
    let none = [
        "#[test] fn a() { let _ = f(); }",
        "#[test] fn a() { assert!(true); }",
        "#[tokio::test] async fn a() { f().await; }",
    ];
    for source in none {
        assert_eq!(count(source, true, TEST_WITHOUT_ASSERTION), 1, "{source}");
    }
    let some = [
        "#[test] fn a() { assert_eq!(f(), 1); }",
        "#[test] fn a() { debug_assert!(f()); }",
        "#[test] fn a() -> Result<(), E> { f()?; Ok(()) }",
        "#[test] #[should_panic] fn a() { f(); }",
        "#[test] fn a() { f().unwrap(); }",
        "#[test] fn a() { f().expect_err(\"refused\"); }",
        "#[test] fn a() { assert_round_trip(f()); }",
        "#[test] fn a() { let Some(x) = f() else { panic!(\"none\") }; }",
        "#[test] fn a() { for x in f() { if x { unreachable!() } } }",
        "#[test] fn a() { tokio::select! { v = f() => assert!(v) } }",
        "#[wasm_bindgen_test] fn a() { cmd().assert().success(); }",
    ];
    for source in some {
        assert_eq!(count(source, true, TEST_WITHOUT_ASSERTION), 0, "{source}");
    }
    assert_eq!(count("fn helper() {}", true, TEST_WITHOUT_ASSERTION), 0);
}

#[test]
fn too_many_lines_allows_are_counted() {
    let source = r#"
        #![allow(clippy::too_many_lines, reason = "x")]
        #[allow(clippy::too_many_lines, clippy::unwrap_used, reason = "x")] fn a() {}
        #[expect(clippy::too_many_lines, reason = "x")] fn b() {}
        #[cfg_attr(test, allow(clippy::too_many_lines, reason = "x"))] fn c() {}
        #[allow(clippy::unwrap_used, reason = "x")] fn d() {}
    "#;
    assert_eq!(count(source, false, TOO_MANY_LINES_ALLOW), 4);
}

/// A throwaway workspace: one crate whose `src/tests.rs` is test code only
/// through `#[cfg(test)] mod tests;`.
fn seed(root: &Path, test_body: &str) {
    let src = root.join("crates/demo/src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("lib.rs"), "#[cfg(test)]\nmod tests;\nmod util;\n").expect("lib.rs");
    std::fs::write(
        src.join("util.rs"),
        "fn wait() { std::thread::sleep(D); }\n",
    )
    .expect("util");
    std::fs::write(src.join("tests.rs"), test_body).expect("tests.rs");
}

fn baseline_with(counts: &RatchetCounts) -> String {
    json!({ "ratchets": ratchets_json(counts) }).to_string()
}

fn rules_of(violations: &[Violation]) -> Vec<&'static str> {
    violations.iter().map(|v| v.rule).collect()
}

#[test]
fn seeded_violation_fails_and_removed_violation_forces_the_baseline_down() {
    let dir = tempfile::tempdir().expect("tempdir");
    let two_sleeps = "fn a() { std::thread::sleep(D); }\nfn b() { std::thread::sleep(D); }\n";
    seed(dir.path(), two_sleeps);
    let found = ratchet_counts(dir.path()).expect("scan");
    let expected: BTreeMap<String, usize> = [("crates/demo/src/tests.rs".to_owned(), 2)].into();
    assert_eq!(found.get(TEST_SLEEP), Some(&expected), "{found:?}");
    let baseline = baseline_with(&found);
    assert_eq!(
        check_ratchets(&found, &baseline).expect("baseline"),
        Vec::<Violation>::new()
    );

    seed(
        dir.path(),
        &format!("{two_sleeps}fn c() {{ tokio::time::sleep(D); }}\n"),
    );
    let more = check_ratchets(&ratchet_counts(dir.path()).expect("scan"), &baseline).expect("ok");
    assert_eq!(rules_of(&more), ["ratchet-exceeded"], "{}", render(&more));
    assert!(
        more.first().is_some_and(|v| v.message.contains("allows 2")),
        "{}",
        render(&more)
    );

    seed(dir.path(), "fn a() { std::thread::sleep(D); }\n");
    let fewer = check_ratchets(&ratchet_counts(dir.path()).expect("scan"), &baseline).expect("ok");
    assert_eq!(rules_of(&fewer), ["ratchet-stale"], "{}", render(&fewer));
    assert!(
        fewer.first().is_some_and(|v| v.message.contains("to 1")),
        "{}",
        render(&fewer)
    );

    seed(dir.path(), "fn a() {}\n");
    let gone = check_ratchets(&ratchet_counts(dir.path()).expect("scan"), &baseline).expect("ok");
    assert_eq!(rules_of(&gone), ["ratchet-stale"], "{}", render(&gone));
    assert!(
        gone.first().is_some_and(|v| v.message.contains("delete")),
        "{}",
        render(&gone)
    );
}

#[test]
fn malformed_baselines_are_refused() {
    let found = RatchetCounts::new();
    for baseline in [
        json!({}),
        json!({ "ratchets": { "made-up": {} } }),
        json!({ "ratchets": { "ignore-without-issue": {}, "test-sleep": {}, "test-without-assertion": {} } }),
        json!({ "ratchets": {
            "ignore-without-issue": { "a.rs": 0 },
            "test-sleep": {},
            "test-without-assertion": {},
            "too-many-lines-allow": {}
        } }),
    ] {
        assert!(
            check_ratchets(&found, &baseline.to_string()).is_err(),
            "{baseline}"
        );
    }
}

#[test]
fn cfg_attr_boolean_predicates_do_not_bypass_ratchets() {
    let too_many = r#"
        #[cfg_attr(true, allow(clippy::too_many_lines, reason = "x"))]
        fn a() {}
        #[cfg_attr(all(true, not(false)), expect(clippy::too_many_lines, reason = "x"))]
        fn b() {}
    "#;
    assert_eq!(count(too_many, false, TOO_MANY_LINES_ALLOW), 2);

    let inner = r#"
        #![cfg_attr(true, allow(clippy::unwrap_used, reason = "x"))]
    "#;
    assert_eq!(count(inner, false, INNER_ALLOW), 1);

    let ignored = r"
        #[test]
        #[cfg_attr(true, ignore)]
        fn a() { assert!(f()); }
        #[test]
        #[cfg_attr(all(true, test), ignore)]
        fn b() { assert!(f()); }
    ";
    assert_eq!(count(ignored, true, IGNORE_WITHOUT_ISSUE), 2);

    // Malformed cfg_attr (missing comma) must fail closed rather than silently returning 0.
    assert!(count_source("#[cfg_attr(true)]\nfn a() {}\n", false).is_err());
}

#[test]
fn aliased_clock_modules_and_item_level_blanket_allows_are_caught() {
    let aliased_sleep = r"
        use std::thread as t;
        use tokio::time::{self as tm};
        #[test]
        fn a() {
            t::sleep(D);
            tm::sleep(D);
            tokio::select! { _ = t::sleep(D) => {} }
            assert!(f());
        }
    ";
    assert_eq!(count(aliased_sleep, false, TEST_SLEEP), 3);

    let item_blankets = r#"
        #[allow(clippy::pedantic, reason = "x")]
        fn a() {}
        #[allow(clippy::all, reason = "x")]
        fn b() {}
        #[cfg_attr(true, expect(clippy::pedantic, reason = "x"))]
        fn c() {}
    "#;
    assert_eq!(count(item_blankets, false, TOO_MANY_LINES_ALLOW), 3);

    // Crate/module-level `#![allow(clippy::pedantic)]` is ratcheted under INNER_ALLOW,
    // not double-counted under TOO_MANY_LINES_ALLOW.
    let inner_pedantic = "#![allow(clippy::pedantic, reason = \"x\")]\n";
    assert_eq!(count(inner_pedantic, false, INNER_ALLOW), 1);
    assert_eq!(count(inner_pedantic, false, TOO_MANY_LINES_ALLOW), 0);
}
