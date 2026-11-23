//! Ring-0 / Ring-1 architectural boundary guards (#1612, #1613).
//!
//! Enforces two structural invariants on Ring-0 crates (including the five core
//! semantic crates [`RING0_CORE_CRATES`]):
//!
//! 1. **Dependency boundary ([`check_ring0_dependencies`]):** host-adapter
//!    crates ([`RING0_FORBIDDEN_HOST_ADAPTERS`]), OLAP/lake crates
//!    ([`RING0_FORBIDDEN_OLAP_LAKE`]), and UI/frontend crates
//!    ([`RING0_FORBIDDEN_UI_FRAMEWORKS`], [`RING0_FORBIDDEN_DIRECT_UI_CRATES`])
//!    are forbidden from entering any Ring-0 crate directly (any dependency
//!    kind) or transitively (normal/build dependencies when `cargo metadata`
//!    includes the resolve graph).
//! 2. **UI/widget module freeze ([`ring0_ui_modules`] + [`check_ring0_ui_modules`]):**
//!    no new UI/widget module file or inline `mod` may enter any Ring-0 crate.
//!    The remaining Ring-1 UI/widget modules still inside `norn-checker`
//!    (`widget_ux/`, `envelope/widget.rs`, `ui_{catalog,interaction,lifecycle,machine,view}.rs`)
//!    are frozen as a shrink-only migration list in `baseline.json`
//!    `ring0_ui_modules` until `#1613` extracts them to `norn-ui-checker`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cargo_metadata::{DependencyKind, Metadata};
use syn::visit::Visit;

use crate::{BaselineError, CRATES, Ring, ScanError, Violation, crate_rule, violation};

/// The five core Ring-0 crates explicitly pinned by #1612 (all must remain
/// classified as [`Ring::Zero`] in [`CRATES`]).
pub const RING0_CORE_CRATES: &[&str] = &[
    "norn-canonical",
    "norn-ir",
    "norn-expr",
    "norn-checker",
    "norn-kernel",
];

/// Host-adapter and runtime crates forbidden from entering any Ring-0 crate
/// (matched by exact crate name or `-`/`_` prefix).
pub const RING0_FORBIDDEN_HOST_ADAPTERS: &[&str] = &[
    "norn-cell",
    "norn-cell-do",
    "norn-cell-rivet",
    "norn-api",
    "norn-cli",
    "norn-checker-wasm",
    "norn-dataplane",
    "rusqlite",
    "libsqlite3-sys",
    "axum",
    "tokio",
    "worker",
    "rivetkit",
    "hyper",
    "tower",
    "reqwest",
    "sqlx",
    "mio",
    "socket2",
    "async-std",
    "smol",
    "ureq",
    "wasmtime",
    "rquickjs",
    "helix-db",
];

/// OLAP, analytical query, and data-lake crates forbidden from entering any
/// Ring-0 crate (matched by exact crate name or `-`/`_` prefix).
pub const RING0_FORBIDDEN_OLAP_LAKE: &[&str] = &[
    "duckdb",
    "libduckdb-sys",
    "datafusion",
    "arrow",
    "parquet",
    "polars",
    "object_store",
    "iceberg",
    "deltalake",
    "clickhouse",
    "lance",
    "vortex",
    "norn-analytics",
];

/// UI/frontend frameworks and Ring-1 UI checker crates forbidden both directly
/// and transitively from entering any Ring-0 crate.
pub const RING0_FORBIDDEN_UI_FRAMEWORKS: &[&str] = &[
    "norn-ui-checker",
    "yew",
    "leptos",
    "dioxus",
    "egui",
    "iced",
    "tauri",
];

/// Browser/WASM UI bindings forbidden as direct dependencies of any Ring-0
/// crate (`norn-checker-wasm` and `norn-cell-do` own the WASM boundary).
pub const RING0_FORBIDDEN_DIRECT_UI_CRATES: &[&str] = &["wasm-bindgen", "web-sys", "js-sys"];

/// Canonical IR schema modules in `norn-ir` that define widget IR data
/// structures shared with Ring-1 `norn-ui-checker` (#1613). Pure data schemas
/// live in `norn-ir`; checking/presentation logic belongs outside Ring-0.
pub const NORN_IR_SCHEMA_WIDGET_MODULES: &[&str] = &[
    "crates/norn-ir/src/widget.rs",
    "crates/norn-ir/src/widget_ux.rs",
];

/// Tracking issue governing extraction of the frozen Ring-1 UI/widget modules
/// from `norn-checker` to `norn-ui-checker`.
pub const RING0_UI_MIGRATION_ISSUE: &str = "#1613";

fn matches_crate_prefix(dep: &str, prefix: &str) -> bool {
    if prefix == "smol" {
        // Avoid false-matching the unrelated `smol_str` small-string crate.
        return dep == "smol";
    }
    dep == prefix
        || dep
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
}

fn forbidden_ring0_category(
    dep_name: &str,
    include_direct_ui: bool,
) -> Option<(&'static str, &'static str, &'static str)> {
    if let Some(prefix) = RING0_FORBIDDEN_HOST_ADAPTERS
        .iter()
        .find(|prefix| matches_crate_prefix(dep_name, prefix))
    {
        return Some(("ring0-host-adapter", "host-adapter", prefix));
    }
    if let Some(prefix) = RING0_FORBIDDEN_OLAP_LAKE
        .iter()
        .find(|prefix| matches_crate_prefix(dep_name, prefix))
    {
        return Some(("ring0-olap-lake", "OLAP/lake", prefix));
    }
    if let Some(prefix) = RING0_FORBIDDEN_UI_FRAMEWORKS
        .iter()
        .find(|prefix| matches_crate_prefix(dep_name, prefix))
    {
        return Some(("ring0-ui-crate", "UI/frontend", prefix));
    }
    if include_direct_ui
        && let Some(prefix) = RING0_FORBIDDEN_DIRECT_UI_CRATES
            .iter()
            .find(|prefix| matches_crate_prefix(dep_name, prefix))
    {
        return Some(("ring0-ui-crate", "UI/frontend", prefix));
    }
    None
}

fn is_ring0(name: &str) -> bool {
    crate_rule(name).is_some_and(|rule| rule.ring == Ring::Zero)
}

/// Checks Ring-0 architectural dependency boundaries against `cargo metadata`:
///
/// - Every crate in [`RING0_CORE_CRATES`] is classified as [`Ring::Zero`] and
///   present in the workspace.
/// - Every Ring-0 crate's direct dependencies (normal, build, dev) exclude
///   [`RING0_FORBIDDEN_HOST_ADAPTERS`], [`RING0_FORBIDDEN_OLAP_LAKE`],
///   [`RING0_FORBIDDEN_UI_FRAMEWORKS`], and [`RING0_FORBIDDEN_DIRECT_UI_CRATES`].
/// - When `metadata.resolve` is present, every Ring-0 crate's transitive
///   normal/build dependency closure also excludes host-adapter, OLAP/lake, and
///   UI framework crates.
#[must_use]
pub fn check_ring0_dependencies(metadata: &Metadata) -> Vec<Violation> {
    let mut out = Vec::new();
    let workspace_pkgs = metadata.workspace_packages();
    let member_names: BTreeSet<&str> = workspace_pkgs.iter().map(|pkg| pkg.name.as_str()).collect();

    for &core in RING0_CORE_CRATES {
        if !is_ring0(core) {
            out.push(violation(
                "ring0-core-misclassified",
                format!("core crate `{core}` must be classified as Ring::Zero in CRATES"),
            ));
        }
        if !member_names.contains(core) {
            out.push(violation(
                "ring0-core-missing",
                format!("core Ring-0 crate `{core}` is missing from workspace members"),
            ));
        }
    }

    for pkg in &workspace_pkgs {
        let pkg_name = pkg.name.as_str();
        if !is_ring0(pkg_name) {
            continue;
        }
        for dep in &pkg.dependencies {
            let dep_name = dep.name.as_str();
            if let Some((rule, category, prefix)) = forbidden_ring0_category(dep_name, true) {
                out.push(violation(
                    rule,
                    format!(
                        "ring-0 crate `{pkg_name}` directly depends on forbidden {category} crate \
                         `{dep_name}` (matched `{prefix}`); Ring-0 must stay pure, sans-IO, and free \
                         of host/OLAP/UI dependencies"
                    ),
                ));
            }
        }
    }

    if let Some(resolve) = &metadata.resolve {
        check_ring0_transitive(metadata, resolve, &mut out);
    }

    out.sort();
    out.dedup();
    out
}

fn check_ring0_transitive(
    metadata: &Metadata,
    resolve: &cargo_metadata::Resolve,
    out: &mut Vec<Violation>,
) {
    let names: BTreeMap<&cargo_metadata::PackageId, &str> = metadata
        .packages
        .iter()
        .map(|pkg| (&pkg.id, pkg.name.as_str()))
        .collect();
    let nodes: BTreeMap<&cargo_metadata::PackageId, &cargo_metadata::Node> =
        resolve.nodes.iter().map(|node| (&node.id, node)).collect();

    for root in metadata.workspace_packages() {
        let root_name = root.name.as_str();
        if !is_ring0(root_name) {
            continue;
        }
        let mut seen: BTreeSet<&cargo_metadata::PackageId> = BTreeSet::new();
        let mut stack = vec![(&root.id, Vec::<&str>::new())];
        while let Some((id, path)) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = nodes.get(id) else {
                continue;
            };
            for dep in &node.deps {
                let shipped = dep.dep_kinds.iter().any(|info| {
                    matches!(info.kind, DependencyKind::Normal | DependencyKind::Build)
                });
                if !shipped {
                    continue;
                }
                let dep_name = names.get(&dep.pkg).copied().unwrap_or(dep.name.as_str());
                let mut via = path.clone();
                via.push(dep_name);
                if let Some((base_rule, category, prefix)) =
                    forbidden_ring0_category(dep_name, false)
                {
                    let rule = match base_rule {
                        "ring0-host-adapter" => "ring0-host-adapter-transitive",
                        "ring0-olap-lake" => "ring0-olap-lake-transitive",
                        _ => "ring0-ui-crate-transitive",
                    };
                    out.push(violation(
                        rule,
                        format!(
                            "ring-0 crate `{root_name}` transitively reaches forbidden {category} \
                             crate `{dep_name}` (matched `{prefix}`) via {}",
                            via.join(" -> ")
                        ),
                    ));
                    continue;
                }
                stack.push((&dep.pkg, via));
            }
        }
    }
}

/// Returns true if a Rust module or directory/file stem names a UI or widget
/// module (`ui`, `ui_*`, `*_ui`, or anything containing `widget`).
#[must_use]
pub fn is_ui_widget_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    lower == "ui" || lower.starts_with("ui_") || lower.ends_with("_ui") || lower.contains("widget")
}

/// Finds inline `mod <name> { ... }` items whose identifier matches
/// [`is_ui_widget_segment`].
#[derive(Default)]
struct InlineUiModVisitor {
    found: Vec<String>,
}

impl<'ast> Visit<'ast> for InlineUiModVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if node.content.is_some() {
            let name = node.ident.to_string();
            if is_ui_widget_segment(&name) {
                self.found.push(name);
            }
        }
        syn::visit::visit_item_mod(self, node);
    }
}

/// Scans `src/` of every [`Ring::Zero`] crate under `workspace_root` and
/// returns all UI/widget module files (and any inline UI/widget `mod` blocks
/// in otherwise non-UI files), excluding the canonical IR schema files
/// [`NORN_IR_SCHEMA_WIDGET_MODULES`].
///
/// # Errors
///
/// Returns [`ScanError`] if a Ring-0 source directory cannot be walked or a
/// source file cannot be read or parsed.
pub fn ring0_ui_modules(workspace_root: &Path) -> Result<BTreeSet<String>, ScanError> {
    let mut found = BTreeSet::new();
    for rule in CRATES {
        if rule.ring != Ring::Zero {
            continue;
        }
        let src_dir = workspace_root.join("crates").join(rule.name).join("src");
        if !src_dir.is_dir() {
            continue;
        }
        let walker = walkdir::WalkDir::new(&src_dir)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| entry.file_name() != "target");
        for entry in walker {
            let entry = entry.map_err(ScanError::Walk)?;
            let path = entry.path();
            if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let rel_workspace = path
                .strip_prefix(workspace_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if NORN_IR_SCHEMA_WIDGET_MODULES.contains(&rel_workspace.as_str()) {
                continue;
            }
            let rel_src = path.strip_prefix(&src_dir).unwrap_or(path);
            let path_is_ui = rel_src.components().any(|component| {
                let text = component.as_os_str().to_string_lossy();
                let stem = text.strip_suffix(".rs").unwrap_or(&text);
                is_ui_widget_segment(stem)
            });
            if path_is_ui {
                found.insert(rel_workspace);
                continue;
            }
            let source = std::fs::read_to_string(path)
                .map_err(|err| ScanError::Io(path.to_path_buf(), err))?;
            let file = syn::parse_file(&source)
                .map_err(|err| ScanError::Parse(path.to_path_buf(), err))?;
            let mut visitor = InlineUiModVisitor::default();
            visitor.visit_file(&file);
            for mod_name in visitor.found {
                found.insert(format!("{rel_workspace}::mod {mod_name}"));
            }
        }
    }
    Ok(found)
}

/// Parses `baseline.json` `ring0_ui_modules` as a sorted set of paths.
///
/// # Errors
///
/// Returns [`BaselineError`] if the JSON is malformed or `ring0_ui_modules` is
/// missing or contains non-string entries.
pub fn baseline_ring0_ui_modules(baseline_json: &str) -> Result<BTreeSet<String>, BaselineError> {
    let root: serde_json::Value =
        serde_json::from_str(baseline_json).map_err(|err| BaselineError(err.to_string()))?;
    let items = root
        .get("ring0_ui_modules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BaselineError("missing array `ring0_ui_modules`".to_owned()))?;
    items
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                BaselineError("`ring0_ui_modules` entries must be strings".to_owned())
            })
        })
        .collect()
}

/// Compares discovered Ring-0 UI/widget modules against the shrink-only
/// `ring0_ui_modules` migration list in `baseline.json`:
///
/// - Any module not in `baseline.json` fails closed with `ring0-ui-module`
///   (referencing `#1613` and `norn-ui-checker`).
/// - Any baseline entry no longer present fails with `stale-ring0-ui-baseline`
///   so `#1613` drains the list down to `[]` as modules are extracted.
///
/// # Errors
///
/// Returns [`BaselineError`] if `baseline_json` is malformed.
pub fn check_ring0_ui_modules(
    found: &BTreeSet<String>,
    baseline_json: &str,
) -> Result<Vec<Violation>, BaselineError> {
    let allowed = baseline_ring0_ui_modules(baseline_json)?;
    let mut out = Vec::new();
    for item in found.difference(&allowed) {
        out.push(violation(
            "ring0-ui-module",
            format!(
                "new UI/widget module `{item}` in Ring-0 is forbidden; UI/widget checking \
                 belongs in Ring-1 `norn-ui-checker` ({RING0_UI_MIGRATION_ISSUE}), and \
                 `baseline.json` `ring0_ui_modules` is shrink-only"
            ),
        ));
    }
    for item in allowed.difference(found) {
        out.push(violation(
            "stale-ring0-ui-baseline",
            format!(
                "`{item}` is gone from Ring-0; delete it from `baseline.json` `ring0_ui_modules` \
                 ({RING0_UI_MIGRATION_ISSUE})"
            ),
        ));
    }
    out.sort();
    Ok(out)
}
