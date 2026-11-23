//! Ring-0 / Ring-1 architectural boundary canaries (#1612, #1613).
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test helpers fail fast on malformed fixtures"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cargo_metadata::{Metadata, MetadataCommand};
use norn_skeleton_guard::Violation;
use norn_skeleton_guard::ring0_boundary::{
    NORN_IR_SCHEMA_WIDGET_MODULES, RING0_CORE_CRATES, RING0_UI_MIGRATION_ISSUE,
    check_ring0_dependencies, check_ring0_ui_modules, is_ui_widget_segment, ring0_ui_modules,
};
use serde_json::{Value, json};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn resolved_metadata() -> Metadata {
    MetadataCommand::new()
        .manifest_path(workspace_root().join("Cargo.toml"))
        .exec()
        .expect("cargo metadata")
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

fn with_dep(base: &Metadata, from: &str, to: &str, kind: Option<&str>) -> Metadata {
    let mut value = serde_json::to_value(base).expect("serialize metadata");
    let pkg = value["packages"]
        .as_array_mut()
        .expect("packages")
        .iter_mut()
        .find(|pkg| pkg["name"] == from)
        .expect("package present");
    pkg["dependencies"]
        .as_array_mut()
        .expect("dependencies")
        .push(json!({
            "name": to,
            "source": Value::Null,
            "req": "*",
            "kind": kind,
            "rename": null,
            "optional": false,
            "uses_default_features": true,
            "features": [],
            "target": null,
            "registry": null,
            "path": Value::Null
        }));
    serde_json::from_value(value).expect("deserialize metadata")
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn ring0_dependencies_and_ui_modules_match_boundary() {
    let metadata = resolved_metadata();
    let dep_violations = check_ring0_dependencies(&metadata);
    assert!(
        dep_violations.is_empty(),
        "Ring-0 dependency violations:\n{}",
        render(&dep_violations)
    );

    let baseline =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline.json"))
            .expect("read baseline.json");
    let found = ring0_ui_modules(&workspace_root()).expect("scan Ring-0 UI modules");
    let ui_violations = check_ring0_ui_modules(&found, &baseline).expect("check UI modules");
    assert!(
        ui_violations.is_empty(),
        "Ring-0 UI/widget module drift:\n{}\n\nfound now:\n{}",
        render(&ui_violations),
        serde_json::to_string_pretty(&found).expect("json")
    );
}

#[test]
fn host_adapters_and_olap_dependencies_are_rejected_in_all_core_ring0_crates() {
    let base = resolved_metadata();
    let host_crates = [
        "norn-cell",
        "norn-cell-do",
        "norn-cell-rivet",
        "norn-api",
        "rusqlite",
        "axum",
        "tokio",
        "worker",
        "rivetkit",
    ];
    let olap_crates = [
        "duckdb",
        "datafusion",
        "arrow-array",
        "parquet",
        "polars",
        "object_store",
        "norn-analytics-duckdb",
    ];
    let ui_crates = ["norn-ui-checker", "wasm-bindgen", "web-sys"];

    for &core in RING0_CORE_CRATES {
        for dep in host_crates {
            let v = check_ring0_dependencies(&with_dep(&base, core, dep, None));
            assert!(
                rules_of(&v).contains(&"ring0-host-adapter"),
                "{core} -> {dep}: {}",
                render(&v)
            );
        }
        for dep in olap_crates {
            let v = check_ring0_dependencies(&with_dep(&base, core, dep, None));
            assert!(
                rules_of(&v).contains(&"ring0-olap-lake"),
                "{core} -> {dep}: {}",
                render(&v)
            );
        }
        for dep in ui_crates {
            let v = check_ring0_dependencies(&with_dep(&base, core, dep, None));
            assert!(
                rules_of(&v).contains(&"ring0-ui-crate"),
                "{core} -> {dep}: {}",
                render(&v)
            );
        }
    }
}

#[test]
fn transitive_host_and_olap_dependencies_into_ring0_are_rejected() {
    let base = resolved_metadata();
    let mut value = serde_json::to_value(&base).expect("serialize metadata");
    let kernel_id = value["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|pkg| pkg["name"] == "norn-kernel")
        .map(|pkg| pkg["id"].clone())
        .expect("norn-kernel package");
    let tokio_id = value["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|pkg| pkg["name"] == "tokio")
        .map(|pkg| pkg["id"].clone())
        .expect("tokio package");

    let node = value["resolve"]["nodes"]
        .as_array_mut()
        .expect("nodes")
        .iter_mut()
        .find(|node| node["id"] == kernel_id)
        .expect("kernel node");
    node["deps"].as_array_mut().expect("deps").push(json!({
        "name": "tokio",
        "pkg": tokio_id,
        "dep_kinds": [{ "kind": null, "target": null }]
    }));

    let tampered: Metadata = serde_json::from_value(value).expect("deserialize");
    let v = check_ring0_dependencies(&tampered);
    assert!(
        v.iter()
            .any(|v| v.rule == "ring0-host-adapter-transitive" && v.message.contains("norn-kernel")),
        "{}",
        render(&v)
    );
}

#[test]
fn new_ui_or_widget_modules_in_ring0_are_rejected_and_removals_drain_baseline() {
    assert!(is_ui_widget_segment("ui_dialog"));
    assert!(is_ui_widget_segment("widget_ux"));
    assert!(is_ui_widget_segment("widget"));
    assert!(is_ui_widget_segment("ui"));
    assert!(!is_ui_widget_segment("quint"));
    assert!(!is_ui_widget_segment("entity_model"));

    let dir = tempfile::tempdir().expect("tempdir");
    // Seed canonical IR widget schemas (allowed) and one frozen checker UI module.
    for schema in NORN_IR_SCHEMA_WIDGET_MODULES {
        write_file(dir.path(), schema, "pub struct Schema;\n");
    }
    let frozen = "crates/norn-checker/src/ui_view.rs";
    write_file(dir.path(), frozen, "pub fn check() {}\n");
    write_file(
        dir.path(),
        "crates/norn-kernel/src/lib.rs",
        "pub fn ok() {}\n",
    );

    let baseline = json!({ "ring0_ui_modules": [frozen] }).to_string();
    let initial = ring0_ui_modules(dir.path()).expect("scan");
    assert_eq!(initial, BTreeSet::from([frozen.to_owned()]));
    assert_eq!(
        check_ring0_ui_modules(&initial, &baseline).expect("check"),
        Vec::<Violation>::new()
    );

    // Adding a new UI/widget file in norn-checker fails closed citing #1613.
    let new_checker_ui = "crates/norn-checker/src/ui_dialog.rs";
    write_file(dir.path(), new_checker_ui, "pub fn dialog() {}\n");
    let v = check_ring0_ui_modules(&ring0_ui_modules(dir.path()).expect("scan"), &baseline)
        .expect("check");
    assert_eq!(rules_of(&v), ["ring0-ui-module"], "{}", render(&v));
    assert!(
        v[0].message.contains(new_checker_ui) && v[0].message.contains(RING0_UI_MIGRATION_ISSUE),
        "{}",
        render(&v)
    );
    std::fs::remove_file(dir.path().join(new_checker_ui)).expect("cleanup");

    // Adding an inline `mod widget_hack {}` inside a Ring-0 crate fails closed too.
    write_file(
        dir.path(),
        "crates/norn-kernel/src/lib.rs",
        "mod widget_hack { pub fn bad() {} }\n",
    );
    let v = check_ring0_ui_modules(&ring0_ui_modules(dir.path()).expect("scan"), &baseline)
        .expect("check");
    assert_eq!(rules_of(&v), ["ring0-ui-module"], "{}", render(&v));
    assert!(v[0].message.contains("widget_hack"), "{}", render(&v));
    write_file(
        dir.path(),
        "crates/norn-kernel/src/lib.rs",
        "pub fn ok() {}\n",
    );

    // Adding an out-of-line `#[path = "presentation.rs"] pub mod ui_dialog;` inside a
    // Ring-0 crate fails closed too.
    write_file(
        dir.path(),
        "crates/norn-kernel/src/presentation.rs",
        "pub fn draw() {}\n",
    );
    write_file(
        dir.path(),
        "crates/norn-kernel/src/lib.rs",
        "#[path = \"presentation.rs\"]\npub mod ui_dialog;\n",
    );
    let v = check_ring0_ui_modules(&ring0_ui_modules(dir.path()).expect("scan"), &baseline)
        .expect("check");
    assert_eq!(rules_of(&v), ["ring0-ui-module"], "{}", render(&v));
    assert!(v[0].message.contains("ui_dialog"), "{}", render(&v));
    std::fs::remove_file(dir.path().join("crates/norn-kernel/src/presentation.rs"))
        .expect("cleanup");
    write_file(
        dir.path(),
        "crates/norn-kernel/src/lib.rs",
        "pub fn ok() {}\n",
    );

    // Removing a frozen module forces draining `baseline.json` `ring0_ui_modules`.
    std::fs::remove_file(dir.path().join(frozen)).expect("remove frozen");
    let v = check_ring0_ui_modules(&ring0_ui_modules(dir.path()).expect("scan"), &baseline)
        .expect("check");
    assert_eq!(rules_of(&v), ["stale-ring0-ui-baseline"], "{}", render(&v));
}
