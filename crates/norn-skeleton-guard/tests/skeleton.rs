//! Workspace skeleton guard (P0-SKEL-2): `cargo test -p norn-skeleton-guard`.
//!
//! The `*_detects_*` tests inject deliberate violations into a copy of the
//! real `cargo metadata` output and assert the guard reports them, so the
//! guard is proven to fail without committing a violation.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test helpers fail fast on malformed cargo metadata fixtures"
)]

use std::path::{Path, PathBuf};

use cargo_metadata::{Metadata, MetadataCommand};
use norn_skeleton_guard::{
    CRATES, NON_CARGO_SURFACES, RAW_JSON_BYTES_OWNER, Ring, Violation, ZenPin, check_baseline,
    check_dependencies, check_layout, check_norn_ir_validators, check_ring0_io_free,
    check_validators_against, check_zen_lock, check_zen_packages, check_zen_vendor,
    pub_validate_fns, raw_json_byte_calls, raw_json_byte_calls_in,
};
use serde_json::{Value, json};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn metadata() -> Metadata {
    MetadataCommand::new()
        .manifest_path(workspace_root().join("Cargo.toml"))
        .no_deps()
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

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// The fork tag zen came from before it was vendored: no longer admitted.
const FORK_TAG_SOURCE: &str = "git+https://github.com/Norn-cloud/zen?tag=norn-v2.0.1-12";

fn zen_pin() -> ZenPin {
    ZenPin::checked_in().expect("zen-pin.json parses")
}

/// The Norn profile features of `krate` from `zen-pin.json` (empty if absent).
fn profile_features(krate: &str) -> Vec<String> {
    zen_pin()
        .profile
        .get(krate)
        .map(|set| set.iter().cloned().collect())
        .unwrap_or_default()
}

/// The vendored directory of zen crate `krate` in the real workspace, as
/// `cargo metadata` reports path dependencies.
fn vendored_dir(krate: &str) -> Option<String> {
    zen_pin()
        .crate_dir(metadata().workspace_root.as_std_path(), krate)
        .map(|dir| dir.display().to_string())
}

/// Returns metadata with an extra dependency `from → to` injected.
///
/// `kind` is `None` (normal), `Some("dev")`, or `Some("build")`. `source`
/// defaults to none for `norn-*` and `zen-*`, and crates.io otherwise. A
/// `zen-*` dependency is a path dependency on its vendored directory (none
/// for a crate that is not vendored) with the Norn profile (default features
/// off, exactly the profile features); other crates keep defaults on.
fn with_dep_as(from: &str, to: &str, kind: Option<&str>, source: Option<&str>) -> Metadata {
    let mut value = serde_json::to_value(metadata()).expect("serialize metadata");
    let packages = value["packages"].as_array_mut().expect("packages");
    let pkg = packages
        .iter_mut()
        .find(|pkg| pkg["name"] == from)
        .expect("package present");
    let zen = to.starts_with("zen-");
    let default_source = if to.starts_with("norn-") || zen {
        None
    } else {
        Some(CRATES_IO)
    };
    let path = if zen { vendored_dir(to) } else { None };
    pkg["dependencies"]
        .as_array_mut()
        .expect("dependencies")
        .push(json!({
            "name": to,
            "source": source.or(default_source).map_or(Value::Null, |src| json!(src)),
            "req": "*",
            "kind": kind,
            "rename": null,
            "optional": false,
            "uses_default_features": !zen,
            "features": if zen { profile_features(to) } else { Vec::new() },
            "target": null,
            "registry": null,
            "path": path
        }));
    serde_json::from_value(value).expect("deserialize metadata")
}

fn with_dep(from: &str, to: &str) -> Metadata {
    with_dep_as(from, to, None, None)
}

fn rules_of(violations: &[Violation]) -> Vec<&'static str> {
    violations.iter().map(|v| v.rule).collect()
}

#[test]
fn workspace_dependencies_match_skeleton() {
    let violations = check_dependencies(&metadata());
    assert!(
        violations.is_empty(),
        "skeleton violations:\n{}",
        render(&violations)
    );
}

#[test]
fn every_crate_and_surface_has_boundary_files() {
    let violations = check_layout(&workspace_root(), &metadata());
    assert!(
        violations.is_empty(),
        "missing boundary files:\n{}",
        render(&violations)
    );
}

#[test]
fn required_surface_boundary_files_are_enforced() {
    let dir = tempfile::tempdir().expect("layout fixture");
    let mut metadata = metadata();
    metadata.workspace_members.clear();
    assert_eq!(
        check_layout(dir.path(), &metadata).len(),
        NON_CARGO_SURFACES
            .len()
            .checked_mul(2)
            .expect("bounded surface count")
    );
    for surface in NON_CARGO_SURFACES {
        let path = dir.path().join(surface);
        std::fs::create_dir_all(&path).expect("surface");
        std::fs::write(path.join("README.md"), "").expect("README");
        std::fs::write(path.join("boundary.json"), "").expect("boundary");
    }
    assert!(check_layout(dir.path(), &metadata).is_empty());
}

#[test]
fn norn_ir_validate_fns_only_shrink() {
    let found =
        pub_validate_fns(&workspace_root().join("crates/norn-ir/src")).expect("scan norn-ir");
    let violations = check_norn_ir_validators(&found);
    assert!(
        violations.is_empty(),
        "norn-ir validate_* drift:\n{}",
        render(&violations)
    );
}

/// #2882: hashed bytes must be canonical JSON; no production code outside
/// `norn-canonical` may encode raw `serde_json` bytes.
#[test]
fn raw_json_bytes_only_in_norn_canonical() {
    let mut found = Vec::new();
    let crates_dir = workspace_root().join("crates");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .expect("read crates/")
        .map(|entry| entry.expect("crates/ entry").path())
        .collect();
    dirs.sort();
    for dir in dirs {
        if dir
            .file_name()
            .is_some_and(|name| name == RAW_JSON_BYTES_OWNER)
        {
            continue;
        }
        let src = dir.join("src");
        if src.is_dir() {
            found.extend(raw_json_byte_calls_in(&src).expect("scan crate sources"));
        }
    }
    assert!(
        found.is_empty(),
        "raw serde_json byte encoding outside norn-canonical; hash via \
         norn_canonical::canonical_json_of / domain_hash_hex instead (#2882):\n{}",
        found.join("\n")
    );
}

#[test]
fn guard_detects_raw_json_bytes() {
    let cases: [(&str, &[&str]); 7] = [
        (
            r"
            use serde_json::{to_writer, Value};
            fn a(v: &Value) -> Vec<u8> { serde_json::to_vec(v).unwrap() }
            fn b(v: &Value) -> String { serde_json::to_string(v).unwrap() }
            ",
            &["serde_json::to_writer", "serde_json::to_vec"],
        ),
        (
            "use serde_json as json; fn a() { json::to_vec(&1); }",
            &["serde_json::to_vec"],
        ),
        (
            "use serde_json::{self as j}; fn a() { j::to_vec_pretty(&1); }",
            &["serde_json::to_vec_pretty"],
        ),
        (
            "extern crate serde_json as sj; fn a() { sj::to_writer(w, &1); }",
            &["serde_json::to_writer"],
        ),
        (
            "use serde_json::*; fn a() { to_vec(&1); }",
            &["serde_json::*"],
        ),
        (
            "fn a() { hash(&format!(\"{:?}\", serde_json::to_vec(&1))); }",
            &["serde_json::to_vec"],
        ),
        (
            "fn a() { let my_serde_json = 1; my_serde_json::to_vec_x(); serde_json::to_string(&1); }",
            &[],
        ),
    ];
    for (source, expected) in cases {
        let found = raw_json_byte_calls(source).expect("parses");
        assert_eq!(found, expected, "{source}");
    }
}

#[test]
fn ring0_crates_are_the_wasm32_set() {
    // Keep in lockstep with the `wasm32` job in .github/workflows/rust.yml.
    let ring0: Vec<&str> = CRATES
        .iter()
        .filter(|rule| rule.ring == Ring::Zero)
        .map(|rule| rule.name)
        .collect();
    let workflow = std::fs::read_to_string(workspace_root().join("scripts/ci/core.sh"))
        .expect("read rust.yml");
    for name in ring0 {
        assert!(
            workflow.contains(&format!("-p {name} ")) || workflow.contains(&format!("-p {name}\n")),
            "ring-0 crate `{name}` missing from the wasm32 CI job"
        );
    }
}

/// Workspace members kept out of `default-members` because they pull large
/// native builds; CI covers them via `--workspace` or a dedicated `-p` job.
const NON_DEFAULT_MEMBERS: &[&str] = &["norn-analytics-duckdb"];

#[test]
fn default_members_are_all_members_except_native_heavy_crates() {
    let metadata = metadata();
    let names = |ids: &[cargo_metadata::PackageId]| -> Vec<String> {
        let mut out: Vec<String> = ids.iter().map(|id| metadata[id].name.to_string()).collect();
        out.sort();
        out
    };
    let expected: Vec<String> = names(&metadata.workspace_members)
        .into_iter()
        .filter(|name| !NON_DEFAULT_MEMBERS.contains(&name.as_str()))
        .collect();
    assert_eq!(
        names(&metadata.workspace_default_members),
        expected,
        "root Cargo.toml `default-members` must list every workspace member except {NON_DEFAULT_MEMBERS:?}"
    );
}

#[test]
fn guard_detects_ring_inversion() {
    let v = check_dependencies(&with_dep("norn-kernel", "norn-cell"));
    assert_eq!(rules_of(&v), ["norn-edge"], "{}", render(&v));
}

#[test]
fn guard_detects_ring0_host_dependency() {
    for host in ["tokio", "rquickjs", "reqwest", "wasmtime-wasi"] {
        let v = check_dependencies(&with_dep("norn-kernel", host));
        assert!(
            rules_of(&v).contains(&"ring0-host"),
            "{host}: {}",
            render(&v)
        );
    }
}

#[test]
fn guard_detects_rusqlite_outside_norn_cell() {
    let v = check_dependencies(&with_dep("norn-cli", "rusqlite"));
    assert_eq!(rules_of(&v), ["single-owner"], "{}", render(&v));
    let v = check_dependencies(&metadata());
    assert!(v.is_empty(), "{}", render(&v));
}

#[test]
fn guard_confines_workers_rs_to_the_cloudflare_adapter() {
    let v = check_dependencies(&with_dep("norn-api", "worker"));
    assert!(rules_of(&v).contains(&"single-owner"), "{}", render(&v));
    let v = check_dependencies(&with_dep("norn-ir", "worker"));
    assert!(rules_of(&v).contains(&"single-owner"), "{}", render(&v));
    assert!(rules_of(&v).contains(&"ring0-unlisted"), "{}", render(&v));
    let v = check_dependencies(&metadata());
    assert!(v.is_empty(), "{}", render(&v));
}

#[test]
fn guard_enforces_norn_cell_native_dependencies_are_optional_normal_dependencies() {
    let valid = check_dependencies(&metadata());
    assert!(valid.is_empty(), "{}", render(&valid));

    let required = edit_package(&metadata(), "norn-cell", |pkg| {
        let sqlite = pkg["dependencies"]
            .as_array_mut()
            .expect("dependencies")
            .iter_mut()
            .find(|dep| dep["name"] == "rusqlite" && dep["kind"] == "normal")
            .expect("normal rusqlite dependency");
        sqlite["optional"] = json!(false);
    });
    let violations = check_dependencies(&required);
    assert!(
        violations.iter().any(|v| {
            v.rule == "norn-cell-native-feature"
                && v.message.contains("rusqlite")
                && v.message.contains("optional normal")
        }),
        "{}",
        render(&violations)
    );

    let build_only = edit_package(&metadata(), "norn-cell", |pkg| {
        let sqlite = pkg["dependencies"]
            .as_array_mut()
            .expect("dependencies")
            .iter_mut()
            .find(|dep| dep["name"] == "rusqlite" && dep["kind"] == "normal")
            .expect("normal rusqlite dependency");
        sqlite["kind"] = json!("build");
    });
    let violations = check_dependencies(&build_only);
    assert!(
        violations.iter().any(|v| {
            v.rule == "norn-cell-native-feature"
                && v.message.contains("rusqlite")
                && v.message.contains("optional normal")
        }),
        "{}",
        render(&violations)
    );
}

#[test]
fn guard_enforces_norn_cell_native_is_default_and_only_native_exposure() {
    let wrong_default = edit_package(&metadata(), "norn-cell", |pkg| {
        pkg["features"]["default"] = json!([]);
    });
    let violations = check_dependencies(&wrong_default);
    assert!(
        violations
            .iter()
            .any(|v| { v.rule == "norn-cell-native-feature" && v.message.contains("default =") }),
        "{}",
        render(&violations)
    );

    let missing_native_dependency = edit_package(&metadata(), "norn-cell", |pkg| {
        let native = pkg["features"]["native"]
            .as_array_mut()
            .expect("native feature");
        native.retain(|feature| feature != "dep:object_store");
    });
    let violations = check_dependencies(&missing_native_dependency);
    assert!(
        violations.iter().any(|v| {
            v.rule == "norn-cell-native-feature" && v.message.contains("object_store")
        }),
        "{}",
        render(&violations)
    );

    let portable_leak = edit_package(&metadata(), "norn-cell", |pkg| {
        pkg["features"]["portable"] = json!(["dep:tempfile"]);
    });
    let violations = check_dependencies(&portable_leak);
    assert!(
        violations.iter().any(|v| {
            v.rule == "norn-cell-native-feature"
                && v.message.contains("portable")
                && v.message.contains("tempfile")
        }),
        "{}",
        render(&violations)
    );
}

#[test]
fn guard_refuses_temporal_clock_and_locale_features() {
    let base = metadata();
    assert!(check_dependencies(&base).is_empty());
    let package_name = "norn-expr";
    let rule = "norn-expr-temporal-features";
    for (defaults, features) in [
        (true, vec!["std".to_owned()]),
        (false, vec!["std".to_owned(), "clock".to_owned()]),
        (false, vec!["std".to_owned(), "unstable-locales".to_owned()]),
    ] {
        let changed = edit_dependency(&base, package_name, "chrono", |dependency| {
            dependency["uses_default_features"] = json!(defaults);
            dependency["features"] = json!(features);
        });
        let violations = check_dependencies(&changed);
        assert!(
            rules_of(&violations).contains(&rule),
            "{package_name}: {}",
            render(&violations)
        );
    }
}

#[test]
fn guard_requires_pinned_std_only_timezone_data() {
    let base = metadata();
    assert!(check_dependencies(&base).is_empty());

    let defaults_on = edit_dependency(&base, "norn-expr", "chrono-tz", |dependency| {
        dependency["uses_default_features"] = json!(true);
    });
    assert!(rules_of(&check_dependencies(&defaults_on)).contains(&"norn-expr-timezone-features"));

    let extra_feature = edit_dependency(&base, "norn-expr", "chrono-tz", |dependency| {
        dependency["features"] = json!(["std", "serde"]);
    });
    assert!(rules_of(&check_dependencies(&extra_feature)).contains(&"norn-expr-timezone-features"));

    let floating_pin = edit_dependency(&base, "norn-expr", "chrono-tz", |dependency| {
        dependency["req"] = json!("^0.10.4");
    });
    assert!(rules_of(&check_dependencies(&floating_pin)).contains(&"norn-expr-timezone-features"));
}

#[test]
fn guard_enforces_expr_and_decisions_only_lists() {
    assert!(check_dependencies(&with_dep("norn-expr", "zen-expression")).is_empty());
    let v = check_dependencies(&with_dep("norn-expr", "serde"));
    assert_eq!(rules_of(&v), ["ring0-unlisted"], "{}", render(&v));
    for ok in ["zen-engine", "serde", "thiserror", "norn-expr"] {
        let v = check_dependencies(&with_dep("norn-decisions", ok));
        assert!(v.is_empty(), "{ok}: {}", render(&v));
    }
    let v = check_dependencies(&with_dep("norn-decisions", "serde_json"));
    assert_eq!(rules_of(&v), ["ring0-unlisted"], "{}", render(&v));
    let v = check_dependencies(&with_dep("norn-kernel", "zen-engine"));
    assert!(rules_of(&v).contains(&"single-owner"), "{}", render(&v));
    assert!(check_dependencies(&with_dep("norn-kernel", "norn-decisions")).is_empty());
}

#[test]
fn guard_detects_alternate_ring0_evaluator_and_cedar_leak() {
    let v = check_dependencies(&with_dep("norn-checker", "rhai"));
    assert!(rules_of(&v).contains(&"ring0-evaluator"), "{}", render(&v));
    let v = check_dependencies(&with_dep("norn-kernel", "cedar-policy"));
    assert!(rules_of(&v).contains(&"single-owner"), "{}", render(&v));
    assert!(check_dependencies(&with_dep("norn-authz", "cedar-policy")).is_empty());
}

#[test]
fn guard_detects_unlisted_ring0_third_party_dependency() {
    for (krate, dep) in [
        ("norn-kernel", "serde_json"),
        ("norn-checker", "itertools"),
        ("norn-ir", "chrono"),
        ("norn-canonical", "base64"),
    ] {
        let v = check_dependencies(&with_dep(krate, dep));
        assert_eq!(
            rules_of(&v),
            ["ring0-unlisted"],
            "{krate} -> {dep}: {}",
            render(&v)
        );
        let v = check_dependencies(&with_dep_as(krate, dep, Some("build"), None));
        assert_eq!(
            rules_of(&v),
            ["ring0-unlisted"],
            "build {krate} -> {dep}: {}",
            render(&v)
        );
    }
    // Listed crates pass; ring-1 third-party is governed by bans + cargo-deny.
    assert!(check_dependencies(&with_dep("norn-ir", "schemars")).is_empty());
    for ok in ["serde", "thiserror"] {
        let v = check_dependencies(&with_dep("norn-kernel", ok));
        assert!(v.is_empty(), "norn-kernel -> {ok}: {}", render(&v));
    }
    assert!(check_dependencies(&with_dep("norn-cli", "itertools")).is_empty());
}

#[test]
fn guard_detects_dev_dependency_violations() {
    let v = check_dependencies(&with_dep_as("norn-kernel", "norn-cell", Some("dev"), None));
    assert_eq!(rules_of(&v), ["norn-edge"], "{}", render(&v));
    assert!(v[0].message.contains("dev dependency"), "{}", render(&v));
    let v = check_dependencies(&with_dep_as("norn-kernel", "tokio", Some("dev"), None));
    assert_eq!(rules_of(&v), ["ring0-host"], "{}", render(&v));
    let v = check_dependencies(&with_dep_as("norn-expr", "rquickjs", Some("dev"), None));
    assert_eq!(rules_of(&v), ["ring0-host"], "{}", render(&v));
    let v = check_dependencies(&with_dep_as("norn-cli", "rusqlite", Some("dev"), None));
    assert_eq!(rules_of(&v), ["single-owner"], "{}", render(&v));
    // Plain dev tooling in ring 0 is not part of the shipped surface.
    assert!(
        check_dependencies(&with_dep_as("norn-kernel", "proptest", Some("dev"), None)).is_empty()
    );
}

/// Replaces the last dependency of `from` (the injected one) via `edit`.
fn edit_injected(metadata: &Metadata, from: &str, edit: impl FnOnce(&mut Value)) -> Metadata {
    let mut value = serde_json::to_value(metadata).expect("serialize");
    let pkg = value["packages"]
        .as_array_mut()
        .expect("packages")
        .iter_mut()
        .find(|pkg| pkg["name"] == from)
        .expect("package present");
    edit(
        pkg["dependencies"]
            .as_array_mut()
            .and_then(|deps| deps.last_mut())
            .expect("injected dependency"),
    );
    serde_json::from_value(value).expect("deserialize")
}

fn edit_dependency(
    metadata: &Metadata,
    package_name: &str,
    dependency_name: &str,
    edit: impl FnOnce(&mut Value),
) -> Metadata {
    let mut value = serde_json::to_value(metadata).expect("serialize");
    let dependency = value["packages"]
        .as_array_mut()
        .expect("packages")
        .iter_mut()
        .find(|package| package["name"] == package_name)
        .expect("package present")["dependencies"]
        .as_array_mut()
        .expect("dependencies")
        .iter_mut()
        .find(|dependency| dependency["name"] == dependency_name)
        .expect("dependency present");
    edit(dependency);
    serde_json::from_value(value).expect("deserialize")
}

fn edit_package(
    metadata: &Metadata,
    package_name: &str,
    edit: impl FnOnce(&mut Value),
) -> Metadata {
    let mut value = serde_json::to_value(metadata).expect("serialize");
    let pkg = value["packages"]
        .as_array_mut()
        .expect("packages")
        .iter_mut()
        .find(|pkg| pkg["name"] == package_name)
        .expect("package present");
    edit(pkg);
    serde_json::from_value(value).expect("deserialize")
}

/// (c) Only the vendored path crate is admitted: the fork at its former tag
/// or any other ref, upstream, crates.io, a path dependency elsewhere, a
/// sibling crate's vendored directory, or a git source carrying the right
/// path is rejected.
#[test]
fn zen_pin_manifest_source_is_vendored_path() {
    let good = with_dep("norn-expr", "zen-expression");
    let v = check_dependencies(&good);
    assert!(v.is_empty(), "{}", render(&v));
    let pin = zen_pin();
    let root = metadata().workspace_root.into_std_path_buf();
    let engine_dir = vendored_dir("zen-engine").expect("zen-engine is vendored");
    let elsewhere = root
        .join(format!("{}-copy", pin.vendor_path))
        .join(&pin.crates["zen-expression"])
        .display()
        .to_string();
    let nested = root
        .join(&pin.vendor_path)
        .join("vendor/zen/core/expression")
        .display()
        .to_string();
    let git_sources = [
        Some(CRATES_IO),
        Some(FORK_TAG_SOURCE),
        Some("git+https://github.com/Norn-cloud/zen.git?tag=norn-v2.0.1-11"),
        Some("git+https://github.com/Norn-cloud/zen"),
        Some("git+https://github.com/Norn-cloud/zen?branch=norn/v2.0.1"),
        Some("git+https://github.com/Norn-cloud/zen?rev=1f6f794aa1d5270cfa84e561b5b3315bc5aacab6"),
        Some("git+https://github.com/Norn-cloud/zen?tag=norn-v2.0.1-12"),
        Some("git+https://github.com/gorules/zen?tag=zen-engine-v2.0.1"),
    ];
    for bad in git_sources {
        // A registry or git source, with the vendored path still set.
        let v = check_dependencies(&with_dep_as("norn-expr", "zen-expression", None, bad));
        assert_eq!(rules_of(&v), ["zen-source"], "{bad:?}: {}", render(&v));
        // The same source as a plain (pathless) dependency.
        let pathless = edit_injected(
            &with_dep_as("norn-expr", "zen-expression", None, bad),
            "norn-expr",
            |dep| dep["path"] = Value::Null,
        );
        let v = check_dependencies(&pathless);
        assert_eq!(rules_of(&v), ["zen-source"], "{bad:?}: {}", render(&v));
    }
    for bad_path in [
        Value::Null,
        json!("/tmp/zen/core/expression"),
        json!(elsewhere),
        json!(nested),
        json!(engine_dir),
    ] {
        let metadata = edit_injected(&good, "norn-expr", |dep| {
            dep["path"] = bad_path.clone();
        });
        let v = check_dependencies(&metadata);
        assert_eq!(rules_of(&v), ["zen-source"], "{bad_path}: {}", render(&v));
    }
}

/// (a) A direct zen dependency may not turn upstream defaults back on or
/// enable any feature outside the Norn profile (e.g. `regex-deprecated`),
/// even when every required profile feature is still present; a `zen-*`
/// package outside the profile is rejected outright.
#[test]
fn zen_pin_rejects_features_outside_profile() {
    let base = with_dep("norn-expr", "zen-expression");
    assert!(check_dependencies(&base).is_empty());
    for extra in [
        "regex-deprecated",
        "runtime-rng",
        "arbitrary_precision",
        "js",
    ] {
        let metadata = edit_injected(&base, "norn-expr", |dep| {
            dep["features"]
                .as_array_mut()
                .expect("features")
                .push(json!(extra));
        });
        let v = check_dependencies(&metadata);
        assert_eq!(rules_of(&v), ["zen-profile"], "{extra}: {}", render(&v));
        assert!(v[0].message.contains(extra), "{}", render(&v));
    }
    let defaults_on = edit_injected(&base, "norn-expr", |dep| {
        dep["uses_default_features"] = json!(true);
    });
    let v = check_dependencies(&defaults_on);
    assert_eq!(rules_of(&v), ["zen-profile"], "{}", render(&v));
    // The vendored crates have no features (#3404), so the folded Norn patches
    // are outside the profile as feature names too.
    for folded in [
        "metering",
        "strict-errors",
        "deterministic-maps",
        "regex-lite",
    ] {
        let metadata = edit_injected(&base, "norn-expr", |dep| {
            dep["features"] = json!([folded]);
        });
        let v = check_dependencies(&metadata);
        assert_eq!(rules_of(&v), ["zen-profile"], "{folded}: {}", render(&v));
    }
    let v = check_dependencies(&with_dep("norn-decisions", "zen-schema-resolvers"));
    assert!(rules_of(&v).contains(&"zen-profile"), "{}", render(&v));
}

const LOCK_HEAD: &str = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";

fn lock_with(name: &str, source: Option<&str>) -> String {
    let source = source.map_or(String::new(), |src| format!("source = \"{src}\"\n"));
    format!("{LOCK_HEAD}\n[[package]]\nname = \"{name}\"\nversion = \"2.0.1\"\n{source}")
}

/// (b) + (c) on the lockfile: every `zen-*` package must be a path package
/// (vendored): the former fork tag at its commit, another commit or tag, a
/// branch, or crates.io is rejected.
#[test]
fn zen_pin_lock_requires_vendored_path_packages() {
    let pin = zen_pin();
    let v = check_zen_lock(&lock_with("zen-expression", None), &pin).expect("parse");
    assert!(v.is_empty(), "{}", render(&v));
    let fork_commit = format!("{FORK_TAG_SOURCE}#{}", pin.commit);
    let other_commit = format!("{FORK_TAG_SOURCE}#{}", "0".repeat(40));
    let other_tag = format!(
        "git+{}?tag=norn-v2.0.1-5#1d1aca0e5f30538ef9111f64a9ccf5b8f897b7ad",
        pin.repository
    );
    let branch = format!("git+{}?branch=norn/v2.0.1#{}", pin.repository, pin.commit);
    let upstream = format!(
        "git+https://github.com/{}?tag={}#{}",
        pin.upstream_repository, pin.upstream_base_tag, pin.upstream_base_commit
    );
    for bad in [
        fork_commit.as_str(),
        FORK_TAG_SOURCE,
        other_commit.as_str(),
        other_tag.as_str(),
        branch.as_str(),
        upstream.as_str(),
        CRATES_IO,
    ] {
        let v = check_zen_lock(&lock_with("zen-types", Some(bad)), &pin).expect("parse");
        assert_eq!(rules_of(&v), ["zen-lock-source"], "{bad:?}: {}", render(&v));
    }
    let v = check_zen_lock(&lock_with("zen-schema-resolvers", None), &pin).expect("parse");
    assert_eq!(rules_of(&v), ["zen-profile"], "{}", render(&v));
    let twice = format!(
        "{}\n[[package]]\nname = \"zen-types\"\nversion = \"2.0.2\"\n",
        lock_with("zen-types", None)
    );
    let v = check_zen_lock(&twice, &pin).expect("parse");
    assert_eq!(rules_of(&v), ["zen-lock-duplicate"], "{}", render(&v));
    assert!(check_zen_lock("not = [toml", &pin).is_err());
}

/// The real workspace (not an injected fixture) consumes the vendored zen:
/// `norn-expr` → `zen-expression` is a normal path dependency on its
/// vendored directory with upstream default features off and exactly its
/// profile features, every `zen-*` package in `Cargo.lock` is a path
/// package, and the vendored tree keeps its licence and crate manifests.
#[test]
fn zen_pin_real_dependency_is_vendored() {
    let metadata = metadata();
    assert!(
        check_dependencies(&metadata).is_empty(),
        "real workspace violates the skeleton"
    );
    let expr = metadata
        .workspace_packages()
        .into_iter()
        .find(|pkg| pkg.name.as_str() == "norn-expr")
        .expect("norn-expr");
    let dep = expr
        .dependencies
        .iter()
        .find(|dep| dep.name == "zen-expression")
        .expect("norn-expr depends on zen-expression");
    assert!(
        zen_pin().is_vendored_dependency(metadata.workspace_root.as_std_path(), dep),
        "{:?} {:?}",
        dep.source,
        dep.path
    );
    assert_eq!(dep.kind, cargo_metadata::DependencyKind::Normal);
    assert!(
        !dep.uses_default_features,
        "the Norn profile disables upstream default features"
    );
    let requested: std::collections::BTreeSet<String> = dep.features.iter().cloned().collect();
    assert_eq!(
        Some(&requested),
        zen_pin().profile.get("zen-expression"),
        "norn-expr must request exactly the zen-expression Norn profile"
    );

    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).expect("Cargo.lock");
    let v = check_zen_lock(&lock, &zen_pin()).expect("parse Cargo.lock");
    assert!(v.is_empty(), "{}", render(&v));
    assert!(
        lock.contains("name = \"zen-expression\""),
        "zen is consumed"
    );
    let v = check_zen_vendor(metadata.workspace_root.as_std_path(), &zen_pin());
    assert!(v.is_empty(), "{}", render(&v));
}

/// The vendored crates are workspace members classified as
/// `Ring::Vendored`: they may use each other (single-owner exempt), but a
/// `norn-*` dependency, a banned normal/build crate, a non-vendored zen
/// source, or a zen crate outside the vendored set is reported; a banned
/// crate as a dev-dependency (native test harness) is not.
#[test]
fn vendored_zen_crates_are_classified_and_bounded() {
    let metadata = metadata();
    let pin = zen_pin();
    for krate in pin.crates.keys() {
        assert_eq!(
            norn_skeleton_guard::crate_rule(krate).map(|rule| rule.ring),
            Some(Ring::Vendored),
            "{krate}"
        );
        assert!(
            metadata
                .workspace_packages()
                .iter()
                .any(|pkg| pkg.name.as_str() == krate),
            "{krate} must be a workspace member"
        );
    }
    assert!(check_dependencies(&with_dep("zen-engine", "zen-expression")).is_empty());
    let v = check_dependencies(&with_dep("zen-engine", "norn-ir"));
    assert_eq!(rules_of(&v), ["norn-edge"], "{}", render(&v));
    for banned in ["tokio", "rquickjs", "reqwest", "getrandom", "rhai"] {
        let v = check_dependencies(&with_dep("zen-expression", banned));
        assert_eq!(
            rules_of(&v),
            ["vendored-forbidden"],
            "{banned}: {}",
            render(&v)
        );
        assert!(
            check_dependencies(&with_dep_as("zen-expression", banned, Some("dev"), None))
                .is_empty()
        );
    }
    let v = check_dependencies(&with_dep_as(
        "zen-engine",
        "zen-expression",
        None,
        Some(FORK_TAG_SOURCE),
    ));
    assert_eq!(rules_of(&v), ["zen-source"], "{}", render(&v));
    let v = check_dependencies(&with_dep("zen-engine", "zen-schema-resolvers"));
    assert!(rules_of(&v).contains(&"zen-profile"), "{}", render(&v));
    // Outside the vendored set the single-owner rule still bites.
    let v = check_dependencies(&with_dep("norn-kernel", "zen-expression"));
    assert!(rules_of(&v).contains(&"single-owner"), "{}", render(&v));
}

/// Vendored crates' boundary files are required at the vendor root.
#[test]
fn vendored_boundary_files_live_at_the_vendor_root() {
    let pin = zen_pin();
    let empty = std::env::temp_dir().join(format!("norn-zen-layout-{}", std::process::id()));
    std::fs::create_dir_all(&empty).expect("temp root");
    let v = check_layout(&empty, &metadata());
    for krate in pin.crates.keys() {
        for file in ["README.md", "boundary.json"] {
            assert!(
                v.iter().any(|v| v.rule == "boundary-file"
                    && v.message
                        .starts_with(&format!("vendored {krate} is missing {file}"))
                    && v.message.contains(&pin.vendor_path)),
                "{krate} {file}: {}",
                render(&v)
            );
        }
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Best-effort temporary fixture cleanup"
    )]
    let _ = std::fs::remove_dir_all(&empty);
}

/// Writes a minimal vendored tree (licence + one manifest per crate) under a
/// fresh temporary root, for provenance guard-bites tests.
fn fake_vendor(name: &str, pin: &ZenPin) -> PathBuf {
    let root = std::env::temp_dir().join(format!("norn-zen-vendor-{}-{name}", std::process::id()));
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Best-effort removal of a stale fixture; a missing directory is already clean"
    )]
    let _ = std::fs::remove_dir_all(&root);
    let vendor = root.join(&pin.vendor_path);
    std::fs::create_dir_all(&vendor).expect("vendor dir");
    std::fs::copy(
        workspace_root()
            .join(&pin.vendor_path)
            .join(&pin.license_file),
        vendor.join(&pin.license_file),
    )
    .expect("copy licence");
    for (krate, dir) in &pin.crates {
        let dir = vendor.join(dir);
        std::fs::create_dir_all(&dir).expect("crate dir");
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{krate}\"\nlicense = \"{}\"\n",
                pin.license_spdx
            ),
        )
        .expect("manifest");
    }
    root
}

/// The provenance check bites: a missing or rewritten licence, a missing
/// crate manifest, a renamed crate, or a relicensed crate is reported.
#[test]
fn zen_vendor_provenance_detects_tampering() {
    let pin = zen_pin();
    let root = fake_vendor("tamper", &pin);
    assert!(check_zen_vendor(&root, &pin).is_empty());
    let vendor = root.join(&pin.vendor_path);
    let license = vendor.join(&pin.license_file);
    let expression = vendor
        .join(&pin.crates["zen-expression"])
        .join("Cargo.toml");
    let original_license = std::fs::read(&license).expect("licence");
    let original_manifest = std::fs::read(&expression).expect("manifest");

    std::fs::write(&license, "Copyright 2023 GoRules.io\n").expect("rewrite licence");
    assert_eq!(
        rules_of(&check_zen_vendor(&root, &pin)),
        ["zen-vendor-license"]
    );
    std::fs::remove_file(&license).expect("remove licence");
    assert_eq!(
        rules_of(&check_zen_vendor(&root, &pin)),
        ["zen-vendor-license"]
    );
    std::fs::write(&license, &original_license).expect("restore licence");

    for (manifest, rule) in [
        (
            "[package]\nname = \"zen-expr\"\nlicense = \"MIT\"\n",
            "zen-vendor-crate",
        ),
        (
            "[package]\nname = \"zen-expression\"\nlicense = \"GPL-3.0\"\n",
            "zen-vendor-license",
        ),
    ] {
        std::fs::write(&expression, manifest).expect("edit manifest");
        assert_eq!(
            rules_of(&check_zen_vendor(&root, &pin)),
            [rule],
            "{manifest}"
        );
    }
    std::fs::remove_file(&expression).expect("remove manifest");
    assert_eq!(
        rules_of(&check_zen_vendor(&root, &pin)),
        ["zen-vendor-crate"]
    );
    std::fs::write(&expression, &original_manifest).expect("restore manifest");
    assert!(check_zen_vendor(&root, &pin).is_empty());
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Best-effort temporary fixture cleanup"
    )]
    let _ = std::fs::remove_dir_all(&root);
}

/// The provenance is recorded consistently: the root `Cargo.toml` takes each
/// workspace zen crate from its vendored path and names no zen git source,
/// `deny.toml` admits no git source, the fork ledger's "Vendored" row names
/// the vendor path, tag, commit, and import tree, and the tree gate's
/// consumer list covers every crate with a direct `zen-*` dependency (so a
/// new consumer is gated the moment it lands).
#[test]
fn zen_pin_is_recorded_consistently() {
    let pin = zen_pin();
    let root = std::fs::read_to_string(workspace_root().join("Cargo.toml")).expect("Cargo.toml");
    for krate in ["zen-expression", "zen-engine"] {
        let entry = format!(
            "{krate} = {{ path = \"{}/{}\", default-features = false }}",
            pin.vendor_path, pin.crates[krate]
        );
        assert!(
            root.contains(&entry),
            "root Cargo.toml must declare `{entry}`"
        );
    }
    assert!(
        !root.contains("github.com/Norn-cloud/zen") && !root.contains("gorules/zen"),
        "root Cargo.toml must not take zen from git"
    );
    let deny: toml::Table = std::fs::read_to_string(workspace_root().join("deny.toml"))
        .expect("deny.toml")
        .parse()
        .expect("deny.toml parses");
    let allow_git = deny
        .get("sources")
        .and_then(|sources| sources.get("allow-git"))
        .and_then(toml::Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(allow_git, 0, "deny.toml must not admit a git source");
    let provenance: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_root().join("vendor/zen/provenance.json"))
            .expect("vendor provenance"),
    )
    .expect("vendor provenance JSON");
    assert_eq!(provenance["vendor"]["path"], pin.vendor_path);
    assert_eq!(provenance["vendor"]["import_tree"], pin.import_tree);
    assert_eq!(provenance["fork"]["tag"], pin.tag);
    assert_eq!(provenance["fork"]["commit"], pin.commit);

    let metadata = metadata();
    let members: std::collections::BTreeSet<&str> = metadata
        .workspace_packages()
        .iter()
        .map(|pkg| pkg.name.as_str())
        .collect();
    for consumer in &pin.consumers {
        assert!(
            members.contains(consumer.as_str()),
            "unknown consumer {consumer}"
        );
    }
    for pkg in metadata.workspace_packages() {
        if pkg
            .dependencies
            .iter()
            .any(|dep| dep.name.starts_with("zen-"))
            && pkg.name.as_str() != "norn-skeleton-guard"
            && !pin.crates.contains_key(pkg.name.as_str())
        {
            assert!(
                pin.consumers.iter().any(|c| c == pkg.name.as_str()),
                "`{}` depends on zen but is not a tree_gate consumer in zen-pin.json",
                pkg.name
            );
        }
    }
}

/// The tree-gate lists are required and non-empty, so a pin file that loses
/// its ban lists cannot make the gate pass vacuously.
#[test]
fn zen_pin_requires_tree_gate_lists() {
    let pin = zen_pin();
    assert!(!pin.forbidden.is_empty() && !pin.forbidden_wasm32.is_empty());
    assert!(pin.forbidden.iter().any(|c| c == "tokio"));
    assert!(pin.forbidden_wasm32.iter().any(|c| c == "getrandom"));
    let base: Value = serde_json::from_str(norn_skeleton_guard::ZEN_PIN_JSON).expect("pin json");
    for key in ["consumers", "forbidden", "forbidden_wasm32"] {
        for replacement in [None, Some(json!([])), Some(json!("tokio"))] {
            let mut doc = base.clone();
            let gate = doc["tree_gate"].as_object_mut().expect("tree_gate");
            match &replacement {
                None => {
                    gate.remove(key);
                }
                Some(value) => {
                    gate.insert(key.to_owned(), value.clone());
                }
            }
            assert!(
                ZenPin::parse(&doc.to_string()).is_err(),
                "{key} = {replacement:?} must be rejected"
            );
        }
    }
}

/// Provenance fields are required and well-formed, so a pin file cannot
/// drop or blur where the vendored tree came from.
#[test]
fn zen_pin_requires_provenance_fields() {
    let base: Value = serde_json::from_str(norn_skeleton_guard::ZEN_PIN_JSON).expect("pin json");
    let edits: [(&str, Value); 13] = [
        ("/tag", json!("norn-v2.0.1-11")),
        ("/vendor/import_tree", json!("e8beb49")),
        ("/vendor/path", json!("../zen")),
        ("/vendor/path", json!("/vendor/zen")),
        ("/vendor/license/copyright", json!("")),
        ("/vendor/license/file", json!("../LICENSE")),
        ("/vendor/crates/zen-types", json!("core/../../types")),
        ("/fork/tag", json!("v2.0.1")),
        (
            "/fork/commit",
            json!("1F6F794AA1D5270CFA84E561B5B3315BC5AACAB6"),
        ),
        ("/upstream/base_tag", json!("v2.0.1")),
        ("/upstream/base_commit", json!("6c0fbab")),
        ("/vendor/crates", json!({})),
        ("/profile/zen-schema-resolvers", json!([])),
    ];
    for (pointer, value) in edits {
        let mut doc = base.clone();
        let (parent, key) = pointer.rsplit_once('/').expect("pointer");
        doc.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("parent object")
            .insert(key.to_owned(), value.clone());
        assert!(
            ZenPin::parse(&doc.to_string()).is_err(),
            "{pointer} = {value} must be rejected"
        );
    }
    for pointer in [
        "/tag",
        "/vendor/import_tree",
        "/fork/commit",
        "/upstream/base_tag",
    ] {
        let mut doc = base.clone();
        let (parent, key) = pointer.rsplit_once('/').expect("pointer");
        doc.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("parent object")
            .remove(key);
        assert!(
            ZenPin::parse(&doc.to_string()).is_err(),
            "missing {pointer} must be rejected"
        );
    }
}

fn guard_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs `scripts/zen-dependency-tree.sh` against a fixture `cargo tree`
/// output (both targets) and an optional pin override; returns success and
/// combined output.
fn run_tree_gate(fixture: &str, pin_json: Option<&str>, name: &str) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("norn-zen-tree-gate-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let fixture_path = dir.join("tree.txt");
    std::fs::write(&fixture_path, fixture).expect("write fixture");
    let pin_path = match pin_json {
        Some(json) => {
            let path = dir.join("zen-pin.json");
            std::fs::write(&path, json).expect("write pin");
            path
        }
        None => guard_dir().join("zen-pin.json"),
    };
    #[allow(
        clippy::disallowed_methods,
        reason = "test/CI-only guard exercising its own CI shell gate; not a runtime capability"
    )]
    let output = std::process::Command::new("bash")
        .arg(guard_dir().join("scripts/zen-dependency-tree.sh"))
        .arg(&pin_path)
        .env("ZEN_TREE_FIXTURE", &fixture_path)
        .output()
        .expect("run zen-dependency-tree.sh (needs bash + jq)");
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Best-effort temporary fixture cleanup; a missing directory is already clean"
    )]
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

/// A `cargo tree --prefix none -f '{p}|{f}'` fixture with zen-engine and
/// zen-expression both selected: zen-expression and zen-types appear more
/// than once (shared by norn-expr and zen-engine), once with cargo's `(*)`
/// dedupe marker. `overrides` replaces a zen package's feature list.
fn both_zen_fixture(overrides: &[(&str, &str)], extra: &[&str]) -> String {
    let pin = zen_pin();
    // The script derives the workspace root from its own location in
    // fixture mode, so vendored rows use the canonical root.
    let root = workspace_root();
    let src_of = |krate: &str| -> String {
        format!(
            "({})",
            pin.crate_dir(&root, krate)
                .expect("vendored crate")
                .display()
        )
    };
    let features = |krate: &str| -> String {
        overrides
            .iter()
            .find(|(name, _)| *name == krate)
            .map_or_else(
                || {
                    pin.profile[krate]
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(",")
                },
                |(_, f)| (*f).to_owned(),
            )
    };
    let (engine, expression, types, tmpl, macros) = (
        src_of("zen-engine"),
        src_of("zen-expression"),
        src_of("zen-types"),
        src_of("zen-tmpl"),
        src_of("zen-macros"),
    );
    let mut lines = vec![
        "norn-expr v0.1.0 (/w/crates/norn-expr)|".to_owned(),
        "norn-decisions v0.1.0 (/w/crates/norn-decisions)|".to_owned(),
        format!("zen-engine v2.0.1 {engine}|{}", features("zen-engine")),
        format!(
            "zen-expression v2.0.1 {expression}|{}",
            features("zen-expression")
        ),
        format!(
            "zen-expression v2.0.1 {expression}|{} (*)",
            features("zen-expression")
        ),
        format!("zen-types v2.0.1 {types}|{}", features("zen-types")),
        format!("zen-types v2.0.1 {types}|{}", features("zen-types")),
        format!("zen-types v2.0.1 {types}|{} (*)", features("zen-types")),
        format!("zen-tmpl v2.0.1 {tmpl}|{}", features("zen-tmpl")),
        format!("zen-macros v2.0.1 (proc-macro) {macros}|"),
        "serde v1.0.228|default,derive,std".to_owned(),
    ];
    lines.extend(extra.iter().map(|line| (*line).to_owned()));
    lines.join("\n") + "\n"
}

/// The gate passes on a tree where zen-engine and zen-expression share zen
/// packages (repeated rows, `(*)` markers), and fails on feature drift, a
/// banned crate, or a pin file whose ban list is gone.
#[test]
fn zen_pin_tree_gate_handles_shared_zen_packages() {
    let (ok, out) = run_tree_gate(&both_zen_fixture(&[], &[]), None, "good");
    assert!(ok, "{out}");
    assert!(out.contains("checking 7 banned crates"), "{out}");
    assert!(out.contains("checking 8 banned crates"), "{out}");
    assert!(out.contains("ok zen-engine ["), "{out}");

    let drift = both_zen_fixture(
        &[(
            "zen-expression",
            "deterministic-maps,metering,regex-deprecated,regex-lite",
        )],
        &[],
    );
    let (ok, out) = run_tree_gate(&drift, None, "drift");
    assert!(!ok && out.contains("regex-deprecated"), "{out}");

    let (ok, out) = run_tree_gate(
        &both_zen_fixture(&[], &["reqwest v0.12.24|default"]),
        None,
        "reqwest",
    );
    assert!(!ok && out.contains("forbidden crate `reqwest`"), "{out}");

    let (ok, out) = run_tree_gate(
        &both_zen_fixture(&[], &["getrandom v0.3.4|wasm_js"]),
        None,
        "getrandom",
    );
    assert!(
        !ok && out.contains("wasm32-unknown-unknown: forbidden crate `getrandom`"),
        "{out}"
    );
    assert!(
        !out.contains("native-fixture: forbidden crate `getrandom`"),
        "{out}"
    );

    // A zen package resolved from the former fork tag, or from a path
    // outside its vendored directory, fails even with profile features.
    let pin = zen_pin();
    let profiled_types = pin.profile["zen-types"]
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let fork_row = format!(
        "zen-types v2.0.1 (https://github.com/Norn-cloud/zen?tag={}#{})|{profiled_types}",
        pin.tag,
        pin.commit.get(..8).expect("40-hex commit")
    );
    let elsewhere_row = format!("zen-types v2.0.1 (/tmp/zen/core/types)|{profiled_types}");
    for (name, row) in [("fork", fork_row), ("elsewhere", elsewhere_row)] {
        let (ok, out) = run_tree_gate(&both_zen_fixture(&[], &[row.as_str()]), None, name);
        assert!(
            !ok && out.contains("is not the vendored crate"),
            "{name}: {out}"
        );
    }

    let (ok, out) = run_tree_gate(
        &both_zen_fixture(&[], &["zen-schema-resolvers v2.0.1|"]),
        None,
        "unprofiled",
    );
    assert!(!ok && out.contains("zen-schema-resolvers"), "{out}");

    let mut no_bans: Value =
        serde_json::from_str(norn_skeleton_guard::ZEN_PIN_JSON).expect("pin json");
    no_bans["tree_gate"]
        .as_object_mut()
        .expect("tree_gate")
        .remove("forbidden");
    let (ok, out) = run_tree_gate(
        &both_zen_fixture(&[], &[]),
        Some(&no_bans.to_string()),
        "nobans",
    );
    assert!(!ok, "missing ban list must fail the gate: {out}");
    no_bans["tree_gate"]["forbidden"] = json!([]);
    let (ok, out) = run_tree_gate(
        &both_zen_fixture(&[], &[]),
        Some(&no_bans.to_string()),
        "emptybans",
    );
    assert!(!ok, "empty ban list must fail the gate: {out}");
}

#[test]
fn allowlists_match_frozen_baseline() {
    let baseline =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline.json"))
            .expect("read baseline.json");
    let v = check_baseline(&baseline).expect("parse baseline");
    assert!(v.is_empty(), "baseline drift:\n{}", render(&v));
}

#[test]
fn guard_detects_allowlist_growth_beyond_baseline() {
    let baseline =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline.json"))
            .expect("read baseline.json");
    let mut value: Value = serde_json::from_str(&baseline).expect("json");
    // Simulate an allowlist entry that the frozen baseline does not contain.
    value["third_party"]
        .as_array_mut()
        .expect("third-party baseline")
        .pop()
        .expect("one retained baseline entry");
    value["norn_ir_validators"]
        .as_array_mut()
        .expect("validators")
        .push(json!("lib.rs::validate_extra"));
    let v = check_baseline(&value.to_string()).expect("parse");
    assert_eq!(
        rules_of(&v),
        ["allowlist-grew", "stale-baseline"],
        "{}",
        render(&v)
    );
}

#[test]
fn guard_detects_dependency_on_unscoped_or_tooling_crates() {
    let v = check_dependencies(&with_dep("norn-kernel", "norn-analytics-duckdb"));
    assert_eq!(rules_of(&v), ["ring0-unlisted"], "{}", render(&v));
    let v = check_dependencies(&with_dep("norn-cli", "norn-skeleton-guard"));
    assert_eq!(rules_of(&v), ["norn-edge"], "{}", render(&v));
}

#[test]
fn test_support_crates_are_dev_dependencies_only() {
    for kind in [None, Some("build")] {
        let v = check_dependencies(&with_dep_as("norn-kernel", "norn-testkit", kind, None));
        assert_eq!(rules_of(&v), ["test-support-dev-only"], "{}", render(&v));
        let v = check_dependencies(&with_dep_as("norn-cli", "norn-testkit", kind, None));
        assert_eq!(rules_of(&v), ["test-support-dev-only"], "{}", render(&v));
    }
    assert!(
        check_dependencies(&with_dep_as(
            "norn-decisions",
            "norn-testkit",
            Some("dev"),
            None
        ))
        .is_empty()
    );
    // Its own third-party dependencies are an exhaustive list.
    for dep in ["reqwest", "tokio", "serde"] {
        let v = check_dependencies(&with_dep("norn-testkit", dep));
        assert!(
            rules_of(&v).contains(&"test-support-unlisted"),
            "{dep}: {}",
            render(&v)
        );
    }
    assert!(
        check_dependencies(&with_dep_as(
            "norn-testkit",
            "serde_json",
            Some("dev"),
            None
        ))
        .is_empty()
    );
    // The test-support crate's own `norn-*` edges still follow the table.
    // 2026-10-14: real-cell Quint replay may use cell only in test code.
    for kind in [None, Some("build")] {
        let v = check_dependencies(&with_dep_as("norn-testkit", "norn-cell", kind, None));
        assert_eq!(rules_of(&v), ["norn-dev-only-edge"], "{}", render(&v));
    }
    assert!(
        check_dependencies(&with_dep_as("norn-testkit", "norn-cell", Some("dev"), None)).is_empty()
    );
}

#[test]
fn guard_detects_new_norn_ir_validator_and_stale_allowlist() {
    let mut found = pub_validate_fns(&workspace_root().join("crates/norn-ir/src")).expect("scan");
    assert!(
        found.is_empty(),
        "norn-ir has no validate_* left: {found:?}"
    );
    found.insert("lib.rs::validate_new_thing".to_owned());
    let v = check_norn_ir_validators(&found);
    assert_eq!(rules_of(&v), ["norn-ir-validate"], "{}", render(&v));

    // The stale-entry rule, against a synthetic allowlist.
    let removed = "sealing.rs::validate_seal_upgrade";
    let v = check_validators_against(&found, &["lib.rs::validate_new_thing", removed]);
    assert_eq!(rules_of(&v), ["stale-allowlist"], "{}", render(&v));
    assert!(v[0].message.contains(removed));
}

#[test]
#[ignore = "enforced in CI by the `public-api-boundary` job (cargo public-api needs nightly rustdoc JSON); plan issue #2502"]
fn no_zen_types_in_expr_or_decisions_public_api() {
    // TODO(#2502): the guard runs on stable and cannot read rustdoc JSON, so
    // the rule lives in `crates/norn-skeleton-guard/scripts/public-api-boundary.sh`
    // (CI job `public-api-boundary`), which fails on any `zen_*` path or
    // external `pub use` in norn-expr / norn-decisions. Run it locally with
    // `crates/norn-skeleton-guard/scripts/public-api-boundary.sh`.
}

/// `cargo metadata` with the resolve graph (transitive dependencies).
fn resolved_metadata() -> Metadata {
    MetadataCommand::new()
        .manifest_path(workspace_root().join("Cargo.toml"))
        .exec()
        .expect("cargo metadata")
}

/// Every resolved `zen-*` package, transitive and dev included, is the
/// vendored crate; a source-less copy elsewhere or a git copy is reported.
#[test]
fn zen_pin_resolved_packages_are_the_vendored_crates() {
    let pin = zen_pin();
    let metadata = resolved_metadata();
    let v = check_zen_packages(&metadata, &pin);
    assert!(v.is_empty(), "{}", render(&v));
    assert!(
        metadata
            .packages
            .iter()
            .any(|p| p.name.as_str() == "zen-types"),
        "zen-types resolves"
    );
    let mut value = serde_json::to_value(&metadata).expect("serialize metadata");
    let packages = value["packages"].as_array_mut().expect("packages");
    let original = packages
        .iter()
        .find(|p| p["name"] == "zen-types")
        .cloned()
        .expect("zen-types package");
    let mut elsewhere = original.clone();
    elsewhere["id"] = json!("path+file:///tmp/zen/core/types#zen-types@2.0.1");
    elsewhere["manifest_path"] = json!("/tmp/zen/core/types/Cargo.toml");
    let mut from_git = original;
    from_git["id"] =
        json!("git+https://github.com/Norn-cloud/zen?tag=norn-v2.0.1-11#zen-types@2.0.1");
    from_git["source"] = json!(FORK_TAG_SOURCE);
    packages.push(elsewhere);
    packages.push(from_git);
    let tampered: Metadata = serde_json::from_value(value).expect("deserialize metadata");
    let v = check_zen_packages(&tampered, &pin);
    assert_eq!(
        rules_of(&v),
        ["zen-package-source", "zen-package-source"],
        "{}",
        render(&v)
    );
}

#[test]
fn ring0_stays_io_free_transitively() {
    let metadata = resolved_metadata();
    let violations = check_ring0_io_free(&metadata);
    assert!(violations.is_empty(), "{}", render(&violations));
    // The native dev host does reach tokio, so the check has teeth.
    assert!(
        metadata
            .packages
            .iter()
            .any(|pkg| pkg.name.as_str() == "tokio"),
        "tokio is in the workspace graph (norn-api)"
    );
}

#[test]
fn guard_detects_ring0_reaching_io_transitively() {
    let mut value = serde_json::to_value(resolved_metadata()).expect("serialize metadata");
    let package_id = |name: &str| -> Value {
        value["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .find(|pkg| pkg["name"] == name)
            .map(|pkg| pkg["id"].clone())
            .expect("package present")
    };
    let kernel = package_id("norn-kernel");
    let tokio = package_id("tokio");
    let nodes = value["resolve"]["nodes"].as_array_mut().expect("nodes");
    let node = nodes
        .iter_mut()
        .find(|node| node["id"] == kernel)
        .expect("kernel node");
    node["deps"].as_array_mut().expect("deps").push(
        json!({ "name": "tokio", "pkg": tokio, "dep_kinds": [{ "kind": null, "target": null }] }),
    );
    let metadata: Metadata = serde_json::from_value(value).expect("metadata");
    let violations = check_ring0_io_free(&metadata);
    assert!(
        violations
            .iter()
            .any(|v| v.rule == "ring0-io-transitive" && v.message.contains("norn-kernel")),
        "{}",
        render(&violations)
    );
}

#[test]
fn guard_detects_unresolved_metadata_for_io_check() {
    let violations = check_ring0_io_free(&metadata());
    assert_eq!(violations.len(), 1, "{}", render(&violations));
    assert_eq!(violations[0].rule, "ring0-io-unresolved");
}

#[test]
fn graph_library_edges_follow_the_pure_boundary() {
    for consumer in ["norn-checker", "norn-projections"] {
        let violations = check_dependencies(&with_dep(consumer, "norn-graph"));
        assert!(violations.is_empty(), "{}", render(&violations));
    }
    let violations = check_dependencies(&with_dep("norn-api", "norn-graph"));
    assert!(rules_of(&violations).contains(&"norn-edge"));
    for host in ["norn-cell", "norn-api"] {
        let violations = check_dependencies(&with_dep("norn-graph", host));
        assert!(
            rules_of(&violations).contains(&"norn-edge"),
            "{}",
            render(&violations)
        );
    }
    let violations = check_dependencies(&with_dep("norn-graph", "tokio"));
    assert!(
        rules_of(&violations).contains(&"ring0-host"),
        "{}",
        render(&violations)
    );
}

/// Public boundary data must retain the executable dependency policy exactly.
#[test]
fn machine_boundaries_match_dependency_policy() {
    for rule in norn_skeleton_guard::CRATES {
        if rule.ring == norn_skeleton_guard::Ring::Vendored {
            continue;
        }
        let path = workspace_root()
            .join("crates")
            .join(rule.name)
            .join("boundary.json");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("boundary"))
                .expect("boundary JSON");
        assert_eq!(doc["name"], rule.name);
        assert_eq!(doc["ring"], format!("{:?}", rule.ring));
        assert_eq!(doc["allowed_norn"], serde_json::json!(rule.allowed_norn));
        assert_eq!(doc["dev_only_norn"], serde_json::json!(rule.dev_only_norn));
        assert_eq!(doc["third_party"], serde_json::json!(rule.third_party));
    }
}
