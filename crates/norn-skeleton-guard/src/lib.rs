//! Skeleton guard: enforces `knowledge/architecture/build-plan-2026-09-09/SKELETON.md`.
//!
//! The policy below is the single transcription of the SKELETON.md dependency
//! table and hard rules. [`check_dependencies`] evaluates it against
//! `cargo metadata`; [`check_layout`] asserts the boundary files exist;
//! [`pub_validate_fns`] + [`check_norn_ir_validators`] enforce the
//! `validate_*` allowlist for `norn-ir`; and [`check_baseline`] pins every
//! known-violation allowlist to the checked-in `baseline.json`.
//!
//! # What is checked, per dependency kind
//!
//! SKELETON.md: "every dependency not listed is forbidden".
//!
//! - **All direct kinds (normal, build, dev):** the `norn-*` edge table, the
//!   ring-0 host/runtime and alternate-evaluator bans, single-owner crates,
//!   and the vendored `zen-*` crates: path dependencies on exactly the
//!   directories recorded in `zen-pin.json`, default features off, and only
//!   Norn-profile features ([`ZenPin`]). [`check_zen_lock`] additionally holds
//!   every `zen-*` package in `Cargo.lock` to one path (vendored) package,
//!   [`check_zen_packages`] holds every resolved `zen-*` package to the
//!   vendored manifest, and [`check_zen_vendor`] checks the vendored licence
//!   and crate manifests.
//! - **Ring-0, normal + build:** an *exhaustive* third-party allowlist per
//!   crate, transcribed literally from SKELETON.md (crates named in the
//!   crate's arrow or purpose column). Dev-only test tooling such as
//!   `proptest` is not part of the shipped ring-0 surface and is governed by
//!   the bans above.
//! - **Test support (`norn-testkit`):** any workspace crate may take it as a
//!   dev-dependency and none as a normal or build dependency; its own
//!   `norn-*` edges follow the table and its normal/build third-party
//!   dependencies are an exhaustive SKELETON.md allowlist.
//! - **Ring-1, unscoped, tooling:** third-party dependencies are governed by
//!   the hard-rule bans plus `cargo deny check` (licenses, advisories,
//!   sources, and the `bans.deny` `wrappers` in `deny.toml`), not by an
//!   exhaustive list. SKELETON.md does not enumerate them.
//! - **`norn-cell` native adapter:** `rusqlite`, `object_store`, `tokio`, and
//!   `tempfile` must be optional normal dependencies enabled only by its
//!   `native` feature, which is also its default feature.
//!
//! # Known violations are shrink-only
//!
//! [`KNOWN_EDGE_VIOLATIONS`], [`KNOWN_THIRD_PARTY_VIOLATIONS`], and
//! [`NORN_IR_VALIDATE_ALLOWLIST`] fail when an entry no longer matches the
//! code, and must equal `crates/norn-skeleton-guard/baseline.json`. The
//! baseline may only lose entries: CI (`skeleton-guard` job,
//! `scripts/baseline-shrink-only.sh`) rejects a PR whose baseline gains an
//! entry relative to the base branch.

pub mod ci;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use cargo_metadata::{Dependency, DependencyKind, Metadata, Package};
use syn::visit::Visit;

/// Ring classification from SKELETON.md.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ring {
    /// Ring 0 — pure, portable, synchronous, sans-IO semantic core (`wasm32`).
    Zero,
    /// Ring 1 — native driver and host perimeter.
    One,
    /// Listed by SKELETON.md as "unchanged until their scoped plan work".
    Unscoped,
    /// Test/CI-only tooling (this guard).
    Tooling,
    /// Test-support library (`norn-testkit`): any workspace crate may take it
    /// as a dev-dependency; no crate may take it as a normal or build
    /// dependency, so it never reaches a shipped or `wasm32` build.
    TestSupport,
    /// Vendored third-party crates (`vendor/zen`, #3404), recorded in
    /// `zen-pin.json`: they depend on no `norn-*` crate, only on each other
    /// as vendored path crates, and never on a `tree_gate.forbidden` crate as
    /// a normal or build dependency. Their boundary files live at the vendor
    /// root.
    Vendored,
}

/// One crate's row of the SKELETON.md table.
#[derive(Debug)]
pub struct CrateRule {
    /// Cargo package name.
    pub name: &'static str,
    /// Ring classification.
    pub ring: Ring,
    /// `norn-*` crates this crate may depend on (any dependency kind).
    pub allowed_norn: &'static [&'static str],
    /// `norn-*` crates this crate may take only as dev-dependencies (test
    /// support for goldens); a normal or build edge is `norn-dev-only-edge`.
    pub dev_only_norn: &'static [&'static str],
    /// Ring 0 and test support: the exhaustive third-party allowlist for
    /// normal and build dependencies. Ignored for other rings (see crate docs).
    pub third_party: &'static [&'static str],
}

const fn r0(
    name: &'static str,
    allowed_norn: &'static [&'static str],
    third_party: &'static [&'static str],
) -> CrateRule {
    CrateRule {
        name,
        ring: Ring::Zero,
        allowed_norn,
        dev_only_norn: &[],
        third_party,
    }
}

const fn rule(name: &'static str, ring: Ring, allowed_norn: &'static [&'static str]) -> CrateRule {
    CrateRule {
        name,
        ring,
        allowed_norn,
        dev_only_norn: &[],
        third_party: &[],
    }
}

/// The SKELETON.md crate table (rings 0/1 plus unscoped and tooling crates).
pub const CRATES: &[CrateRule] = &[
    r0("norn-graph", &[], &["rust_decimal"]),
    r0("norn-canonical", &[], &[]),
    r0(
        "norn-ir",
        &["norn-canonical"],
        &["serde", "schemars", "rust_decimal", "serde_json"],
    ),
    r0(
        "norn-expr",
        &["norn-ir"],
        &["zen-expression", "chrono", "chrono-tz"],
    ),
    r0(
        "norn-checker",
        &["norn-ir", "norn-expr", "norn-canonical", "norn-graph"],
        &["serde_json", "sha2", "regex-lite", "serde_path_to_error"],
    ),
    r0(
        "norn-decisions",
        &["norn-ir", "norn-expr"],
        &["zen-engine", "serde", "thiserror"],
    ),
    r0("norn-projections", &["norn-ir", "norn-graph"], &[]),
    r0(
        "norn-authz",
        &["norn-ir", "norn-projections"],
        &["cedar-policy"],
    ),
    r0(
        "norn-kernel",
        &[
            "norn-ir",
            "norn-expr",
            "norn-checker",
            "norn-authz",
            "norn-decisions",
        ],
        &["serde", "thiserror"],
    ),
    rule(
        "norn-cell",
        Ring::One,
        &["norn-kernel", "norn-checker", "norn-projections", "norn-ir"],
    ),
    r0("norn-api", &["norn-ir"], &["serde", "serde_json", "utoipa"]),
    rule(
        "norn-cli",
        Ring::One,
        &[
            "norn-api",
            "norn-checker-wasm",
            "norn-ir",
            "norn-expr",
            "norn-decisions",
        ],
    ),
    rule(
        "norn-checker-wasm",
        Ring::One,
        &["norn-checker", "norn-projections", "norn-expr", "norn-ir"],
    ),
    rule("norn-skeleton-guard", Ring::Tooling, &[]),
    CrateRule {
        name: "norn-testkit",
        ring: Ring::TestSupport,
        allowed_norn: &["norn-kernel"],
        dev_only_norn: &["norn-cell", "norn-decisions", "norn-expr", "norn-ir"],
        third_party: &["proptest", "proptest-state-machine"],
    },
    rule("zen-engine", Ring::Vendored, &[]),
    rule("zen-expression", Ring::Vendored, &[]),
    rule("zen-macros", Ring::Vendored, &[]),
    rule("zen-tmpl", Ring::Vendored, &[]),
    rule("zen-types", Ring::Vendored, &[]),
];

/// Optional normal dependencies that belong exclusively to norn-cell's
/// native host adapters, enabled by its `native` feature.
const NORN_CELL_NATIVE_DEPENDENCIES: &[&str] = &["rusqlite", "object_store", "tokio", "tempfile"];

/// Known `norn-*` edges that predate the skeleton: `(from, to, reason)`.
/// Shrink-only; must equal `baseline.json` `norn_edges`.
pub const KNOWN_EDGE_VIOLATIONS: &[(&str, &str, &str)] = &[];

/// Known ring-0 third-party dependencies SKELETON.md does not list:
/// `(crate, dependency, reason)`. Shrink-only; must equal `baseline.json`
/// `third_party`. Each leaves by the code dropping it or by a dated ruling
/// amending SKELETON.md (after which the crate's `third_party` list gains it).
pub const KNOWN_THIRD_PARTY_VIOLATIONS: &[(&str, &str, &str)] = &[
    (
        "norn-canonical",
        "serde",
        "pre-skeleton; SKELETON.md row names no third-party crates",
    ),
    (
        "norn-canonical",
        "serde_json",
        "pre-skeleton; SKELETON.md row names no third-party crates",
    ),
    (
        "norn-canonical",
        "sha2",
        "pre-skeleton; SKELETON.md row names no third-party crates",
    ),
    (
        "norn-canonical",
        "unicode-normalization",
        "pre-skeleton; SKELETON.md row names no third-party crates",
    ),
];

/// Crates no ring-0 crate may depend on directly (any kind): async runtimes,
/// JS engines, network clients, storage engines, and other host APIs.
pub const RING0_FORBIDDEN_PREFIXES: &[&str] = &[
    "tokio",
    "async-std",
    "smol",
    "mio",
    "rquickjs",
    "reqwest",
    "hyper",
    "axum",
    "tower",
    "ureq",
    "rusqlite",
    "libsqlite3-sys",
    "sqlx",
    "duckdb",
    "wasmtime",
    "helix-db",
    "getrandom",
];

/// IO crates — async runtimes, HTTP servers and clients, sockets, storage
/// engines — that no ring-0 crate may reach through normal or build
/// dependencies, **directly or transitively** ([`check_ring0_io_free`]). The
/// native dev host (`norn-api`, `norn-cli`) may use them; the kernel and the
/// rest of ring 0 stay IO-free.
pub const RING0_IO_CRATES: &[&str] = &[
    "tokio",
    "mio",
    "socket2",
    "axum",
    "axum-core",
    "hyper",
    "hyper-util",
    "tower",
    "tower-http",
    "reqwest",
    "rusqlite",
    "libsqlite3-sys",
];

/// Every ring-0 crate's transitive normal/build dependency closure (from the
/// resolved `cargo metadata` graph) excludes [`RING0_IO_CRATES`].
///
/// `metadata` must carry the resolve graph (no `--no-deps`); without it the
/// check reports `ring0-io-unresolved`.
#[must_use]
pub fn check_ring0_io_free(metadata: &Metadata) -> Vec<Violation> {
    let Some(resolve) = &metadata.resolve else {
        return vec![violation(
            "ring0-io-unresolved",
            "cargo metadata has no resolve graph; run it without --no-deps".to_owned(),
        )];
    };
    let names: BTreeMap<&cargo_metadata::PackageId, &str> = metadata
        .packages
        .iter()
        .map(|pkg| (&pkg.id, pkg.name.as_str()))
        .collect();
    let nodes: BTreeMap<&cargo_metadata::PackageId, &cargo_metadata::Node> =
        resolve.nodes.iter().map(|node| (&node.id, node)).collect();
    let mut out = Vec::new();
    for root in metadata.workspace_packages() {
        if crate_rule(root.name.as_str()).is_none_or(|rule| rule.ring != Ring::Zero) {
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
                let name = names.get(&dep.pkg).copied().unwrap_or(dep.name.as_str());
                let mut via = path.clone();
                via.push(name);
                if RING0_IO_CRATES.contains(&name) {
                    out.push(violation(
                        "ring0-io-transitive",
                        format!(
                            "ring-0 crate `{}` reaches IO crate `{name}` via {}",
                            root.name.as_str(),
                            via.join(" → ")
                        ),
                    ));
                    continue;
                }
                stack.push((&dep.pkg, via));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Alternative evaluation engines that are never admitted in ring 0; only
/// `zen-engine` and `zen-expression` are (SKELETON.md hard rules).
pub const RING0_FORBIDDEN_EVALUATORS: &[&str] = &[
    "rhai",
    "cel-interpreter",
    "cel-parser",
    "boa_engine",
    "deno_core",
    "v8",
    "mlua",
    "rlua",
    "starlark",
    "jsonlogic",
    "evalexpr",
];

/// Third-party crates with exactly one Norn owner: `(crate, sole owner)`.
pub const SINGLE_OWNER: &[(&str, &str)] = &[
    ("worker", "norn-cell-do"),
    ("rusqlite", "norn-cell"),
    ("cedar-policy", "norn-authz"),
    ("zen-expression", "norn-expr"),
    ("zen-engine", "norn-decisions"),
    ("helix-db", "norn-retrieval"),
];

/// The vendored zen crate set's provenance record (D19, D32, #3404), checked
/// in as `zen-pin.json` next to this crate's manifest. `vendor/zen` was
/// imported byte-identical from the `Norn-cloud/zen` fork tag with
/// `git subtree add --squash`; later changes are Norn commits in this
/// repository. It is the only place the guard and the integrated
/// dependency-tree gate (`scripts/zen-dependency-tree.sh`) read the vendor
/// path, crate map, provenance, and Norn profile from.
pub const ZEN_PIN_JSON: &str = include_str!("../zen-pin.json");

/// The parsed [`ZEN_PIN_JSON`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZenPin {
    /// Vendored directory, relative to the workspace root (`vendor/zen`).
    pub vendor_path: String,
    /// Git tree id of `vendor_path` at import: the fork tag's tree.
    pub import_tree: String,
    /// SPDX id every vendored crate manifest declares (`MIT`).
    pub license_spdx: String,
    /// The licence file, relative to `vendor_path`.
    pub license_file: String,
    /// The copyright line the licence file must keep.
    pub license_copyright: String,
    /// Each admitted zen crate and its directory relative to `vendor_path`.
    pub crates: BTreeMap<String, String>,
    /// Fork repository the import came from (`https://github.com/Norn-cloud/zen`).
    pub repository: String,
    /// The fork tag that was imported (`norn-v<upstream>-<N>`).
    pub tag: String,
    /// The 40-hex commit that tag resolves to.
    pub commit: String,
    /// Upstream repository the fork is based on (`gorules/zen`).
    pub upstream_repository: String,
    /// Upstream release tag the fork is based on (`zen-engine-v<version>`).
    pub upstream_base_tag: String,
    /// The 40-hex commit of the upstream base tag.
    pub upstream_base_commit: String,
    /// Norn profile: the exact resolved feature set per `zen-*` package. A
    /// `zen-*` package outside this map is not admitted.
    pub profile: BTreeMap<String, BTreeSet<String>>,
    /// Integrated consumers the dependency-tree gate resolves together.
    pub consumers: Vec<String>,
    /// Crates banned from the integrated tree on every target.
    pub forbidden: Vec<String>,
    /// Crates additionally banned on `wasm32-unknown-unknown`.
    pub forbidden_wasm32: Vec<String>,
}

/// Malformed `zen-pin.json` or `Cargo.lock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinError(pub String);

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PinError {}

fn json_str(root: &serde_json::Value, pointer: &str) -> Result<String, PinError> {
    root.pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| PinError(format!("zen-pin.json: missing string `{pointer}`")))
}

fn json_strings(value: &serde_json::Value, what: &str) -> Result<Vec<String>, PinError> {
    value
        .as_array()
        .ok_or_else(|| PinError(format!("zen-pin.json: `{what}` is not an array")))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| PinError(format!("zen-pin.json: `{what}` has a non-string entry")))
        })
        .collect()
}

fn json_object<'a>(
    root: &'a serde_json::Value,
    pointer: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, PinError> {
    root.pointer(pointer)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| PinError(format!("zen-pin.json: missing object `{pointer}`")))
}

fn is_git_object_id(id: &str) -> bool {
    id.len() == 40
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A non-empty relative `/`-separated path with no `.`, `..`, or empty
/// component, so a vendored path cannot escape the workspace.
fn is_plain_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

/// Field-level checks of the provenance record: the fork tag is `norn-v*`,
/// the upstream base tag is `zen-engine-v*`, every git object id is 40
/// lowercase hex, every path is plain relative, and no licence field is empty.
fn check_provenance_fields(
    tag: &str,
    upstream_base_tag: &str,
    ids: &[(&str, &str)],
    paths: &[&str],
    license: &[&str],
) -> Result<(), PinError> {
    if !tag.starts_with("norn-v") {
        return Err(PinError(format!(
            "zen-pin.json: fork tag `{tag}` is not a norn-v* tag"
        )));
    }
    if !upstream_base_tag.starts_with("zen-engine-v") {
        return Err(PinError(format!(
            "zen-pin.json: upstream base tag `{upstream_base_tag}` is not a zen-engine-v* tag"
        )));
    }
    if let Some((what, id)) = ids.iter().find(|(_, id)| !is_git_object_id(id)) {
        return Err(PinError(format!(
            "zen-pin.json: {what} `{id}` is not 40 lowercase hex"
        )));
    }
    if paths.iter().any(|path| !is_plain_relative_path(path)) {
        return Err(PinError(
            "zen-pin.json: vendor path and licence file must be plain relative paths".to_owned(),
        ));
    }
    if license.iter().any(|field| field.is_empty()) {
        return Err(PinError(
            "zen-pin.json: the vendored licence must name its SPDX id and copyright".to_owned(),
        ));
    }
    Ok(())
}

impl ZenPin {
    /// Parses a `zen-pin.json` document.
    ///
    /// # Errors
    ///
    /// Returns [`PinError`] if the JSON is malformed, a field is missing, the
    /// top-level `tag` differs from `fork.tag`, the fork tag is not `norn-v*`, the upstream base tag is not
    /// `zen-engine-v*`, a commit or tree id is not 40 lowercase hex, a
    /// vendored path is not a plain relative path, the crate map and the
    /// profile name different crates, or a tree-gate list (`consumers`,
    /// `forbidden`, `forbidden_wasm32`) is missing or empty.
    pub fn parse(text: &str) -> Result<Self, PinError> {
        let root: serde_json::Value =
            serde_json::from_str(text).map_err(|err| PinError(format!("zen-pin.json: {err}")))?;
        let vendor_path = json_str(&root, "/vendor/path")?;
        let import_tree = json_str(&root, "/vendor/import_tree")?;
        let license_spdx = json_str(&root, "/vendor/license/spdx")?;
        let license_file = json_str(&root, "/vendor/license/file")?;
        let license_copyright = json_str(&root, "/vendor/license/copyright")?;
        let repository = json_str(&root, "/fork/repository")?;
        let tag = json_str(&root, "/fork/tag")?;
        // `tag` mirrors `fork.tag` for `.github/workflows/zen-upstream-watch.yml`,
        // whose step is frozen in `.github/ci/escape-hatch-baseline.json`.
        let watch_tag = json_str(&root, "/tag")?;
        if watch_tag != tag {
            return Err(PinError(format!(
                "zen-pin.json: `tag` `{watch_tag}` must equal `fork.tag` `{tag}` (read by zen-upstream-watch.yml)"
            )));
        }
        let commit = json_str(&root, "/fork/commit")?;
        let upstream_repository = json_str(&root, "/upstream/repository")?;
        let upstream_base_tag = json_str(&root, "/upstream/base_tag")?;
        let upstream_base_commit = json_str(&root, "/upstream/base_commit")?;
        check_provenance_fields(
            &tag,
            &upstream_base_tag,
            &[
                ("fork commit", commit.as_str()),
                ("upstream base commit", upstream_base_commit.as_str()),
                ("vendor import tree", import_tree.as_str()),
            ],
            &[vendor_path.as_str(), license_file.as_str()],
            &[license_spdx.as_str(), license_copyright.as_str()],
        )?;
        let crates = json_object(&root, "/vendor/crates")?
            .iter()
            .map(|(krate, dir)| {
                let dir = dir.as_str().filter(|dir| is_plain_relative_path(dir)).ok_or_else(|| {
                    PinError(format!(
                        "zen-pin.json: vendored crate `{krate}` needs a plain relative directory"
                    ))
                })?;
                Ok((krate.clone(), dir.to_owned()))
            })
            .collect::<Result<BTreeMap<_, _>, PinError>>()?;
        let profile = json_object(&root, "/profile")?
            .iter()
            .map(|(krate, features)| {
                json_strings(features, krate)
                    .map(|list| (krate.clone(), list.into_iter().collect::<BTreeSet<_>>()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        if crates.is_empty() || !crates.keys().eq(profile.keys()) {
            return Err(PinError(format!(
                "zen-pin.json: vendored crates {:?} must be exactly the profiled crates {:?}",
                crates.keys().collect::<Vec<_>>(),
                profile.keys().collect::<Vec<_>>()
            )));
        }
        let required_list = |pointer: &str| -> Result<Vec<String>, PinError> {
            let list = json_strings(
                root.pointer(pointer).unwrap_or(&serde_json::Value::Null),
                pointer,
            )?;
            if list.is_empty() {
                return Err(PinError(format!(
                    "zen-pin.json: `{pointer}` must not be empty (the tree gate would pass vacuously)"
                )));
            }
            Ok(list)
        };
        let consumers = required_list("/tree_gate/consumers")?;
        let forbidden = required_list("/tree_gate/forbidden")?;
        let forbidden_wasm32 = required_list("/tree_gate/forbidden_wasm32")?;
        Ok(Self {
            vendor_path,
            import_tree,
            license_spdx,
            license_file,
            license_copyright,
            crates,
            repository,
            tag,
            commit,
            upstream_repository,
            upstream_base_tag,
            upstream_base_commit,
            profile,
            consumers,
            forbidden,
            forbidden_wasm32,
        })
    }

    /// The checked-in pin ([`ZEN_PIN_JSON`]).
    ///
    /// # Errors
    ///
    /// Returns [`PinError`] if the checked-in file is malformed.
    pub fn checked_in() -> Result<Self, PinError> {
        Self::parse(ZEN_PIN_JSON)
    }

    /// The directory of vendored crate `krate` under `workspace_root`, or
    /// `None` if `krate` is not a vendored zen crate.
    #[must_use]
    pub fn crate_dir(&self, workspace_root: &Path, krate: &str) -> Option<PathBuf> {
        self.crates
            .get(krate)
            .map(|dir| workspace_root.join(&self.vendor_path).join(dir))
    }

    /// Returns true if `dep` is the vendored crate of the same name: a path
    /// dependency (no registry or git `source`) on exactly its vendored
    /// directory. The fork or upstream at any git ref, crates.io, and a path
    /// anywhere else are rejected.
    #[must_use]
    pub fn is_vendored_dependency(&self, workspace_root: &Path, dep: &Dependency) -> bool {
        dep.source.is_none()
            && self
                .crate_dir(workspace_root, dep.name.as_str())
                .zip(dep.path.as_ref())
                .is_some_and(|(expected, actual)| actual.as_std_path() == expected.as_path())
    }
}

fn is_zen_crate(name: &str) -> bool {
    name.starts_with("zen-") || name.starts_with("zen_")
}

/// Checks every `zen-*` package in `Cargo.lock` (D32, #3404): each must be a
/// path package (no `source` line, so the fork, upstream, or crates.io cannot
/// come back through the lockfile) and must be a vendored crate of the Norn
/// profile.
///
/// # Errors
///
/// Returns [`PinError`] if `lock` is not a valid `Cargo.lock`.
pub fn check_zen_lock(lock: &str, pin: &ZenPin) -> Result<Vec<Violation>, PinError> {
    let root: toml::Table = lock
        .parse()
        .map_err(|err| PinError(format!("Cargo.lock: {err}")))?;
    let packages = root
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| PinError("Cargo.lock: no [[package]] array".to_owned()))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(toml::Value::as_str) else {
            return Err(PinError("Cargo.lock: package without a name".to_owned()));
        };
        if !is_zen_crate(name) {
            continue;
        }
        if let Some(source) = package.get("source").and_then(toml::Value::as_str) {
            out.push(violation(
                "zen-lock-source",
                format!(
                    "Cargo.lock `{name}` must be the vendored path crate under {}; found source {source:?}",
                    pin.vendor_path
                ),
            ));
        }
        if !pin.profile.contains_key(name) {
            out.push(violation(
                "zen-profile",
                format!("Cargo.lock contains `{name}`, which is not a package of the Norn zen profile (zen-pin.json)"),
            ));
        }
        if !seen.insert(name) {
            out.push(violation(
                "zen-lock-duplicate",
                format!(
                    "Cargo.lock contains `{name}` more than once; only the vendored crate may resolve"
                ),
            ));
        }
    }
    out.sort();
    Ok(out)
}

/// Checks every `zen-*` package of the resolved graph (`metadata` from
/// `cargo metadata` *without* `--no-deps`, so the transitive, build and dev
/// dependencies of every workspace member count, not only the tree-gate
/// consumers): each must have no registry or git source and its manifest
/// must be exactly the vendored crate's `Cargo.toml`. A source-less lock
/// entry alone does not prove provenance: a path dependency outside
/// `vendor/zen` is source-less too.
#[must_use]
pub fn check_zen_packages(metadata: &Metadata, pin: &ZenPin) -> Vec<Violation> {
    let root = metadata.workspace_root.as_std_path();
    let mut out = Vec::new();
    for package in &metadata.packages {
        let name = package.name.as_str();
        if !is_zen_crate(name) {
            continue;
        }
        let expected = pin.crate_dir(root, name).map(|dir| dir.join("Cargo.toml"));
        let vendored = package.source.is_none()
            && expected.as_deref() == Some(package.manifest_path.as_std_path());
        if !vendored {
            out.push(violation(
                "zen-package-source",
                format!(
                    "resolved package `{name} {}` must be the vendored crate {expected:?}; found source {:?} and manifest {}",
                    package.version,
                    package.source.as_ref().map(|src| src.repr.as_str()),
                    package.manifest_path
                ),
            ));
        }
    }
    out.sort();
    out
}

/// Checks the vendored tree against its provenance record: the MIT licence
/// file is present with its grant and copyright line, and each admitted
/// crate's manifest exists at its recorded directory, names that crate, and
/// declares the recorded licence.
#[must_use]
pub fn check_zen_vendor(workspace_root: &Path, pin: &ZenPin) -> Vec<Violation> {
    let mut out = Vec::new();
    let vendor = workspace_root.join(&pin.vendor_path);
    let license_path = vendor.join(&pin.license_file);
    match std::fs::read_to_string(&license_path) {
        Ok(text)
            if text.contains(&pin.license_copyright)
                && text.contains("Permission is hereby granted, free of charge") => {}
        Ok(_) => out.push(violation(
            "zen-vendor-license",
            format!(
                "{} must keep the MIT grant and `{}`",
                license_path.display(),
                pin.license_copyright
            ),
        )),
        Err(err) => out.push(violation(
            "zen-vendor-license",
            format!("{}: {err}", license_path.display()),
        )),
    }
    for (krate, dir) in &pin.crates {
        let manifest_path = vendor.join(dir).join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .map_err(|err| err.to_string())
            .and_then(|text| text.parse::<toml::Table>().map_err(|err| err.to_string()));
        let package = match manifest {
            Ok(manifest) => manifest.get("package").cloned(),
            Err(err) => {
                out.push(violation(
                    "zen-vendor-crate",
                    format!("{}: {err}", manifest_path.display()),
                ));
                continue;
            }
        };
        let field = |key: &str| {
            package
                .as_ref()
                .and_then(|package| package.get(key))
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        };
        if field("name").as_deref() != Some(krate.as_str()) {
            out.push(violation(
                "zen-vendor-crate",
                format!(
                    "{} must declare package `{krate}`; found {:?}",
                    manifest_path.display(),
                    field("name")
                ),
            ));
        }
        if field("license").as_deref() != Some(pin.license_spdx.as_str()) {
            out.push(violation(
                "zen-vendor-license",
                format!(
                    "{} must declare license = \"{}\"; found {:?}",
                    manifest_path.display(),
                    pin.license_spdx,
                    field("license")
                ),
            ));
        }
    }
    out.sort();
    out
}

/// Directories outside Cargo that must carry README.md + AGENTS.md; `apps/admin`
/// is optional only while absent.
pub const NON_CARGO_SURFACES: &[&str] = &["tools/norn-ts", "packages/norn-sdk", "sdks"];

/// `norn-ir` non-private `validate_*` functions that predate the skeleton, as
/// `<path relative to crates/norn-ir/src>::<fn name>`.
///
/// SKELETON.md: "`norn-ir` contains no validation logic beyond serde/schemars
/// derivation; `validate_*` functions move to `norn-checker`." T1-CHK-1 (#2524)
/// moved every one, so the allowlist is empty; shrink-only, must equal
/// `baseline.json` `norn_ir_validators`.
pub const NORN_IR_VALIDATE_ALLOWLIST: &[&str] = &[];

/// One broken skeleton rule.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    /// Stable rule identifier.
    pub rule: &'static str,
    /// Human-readable detail.
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.rule, self.message)
    }
}

fn violation(rule: &'static str, message: String) -> Violation {
    Violation { rule, message }
}

/// Looks up a crate's rule by package name.
#[must_use]
pub fn crate_rule(name: &str) -> Option<&'static CrateRule> {
    CRATES.iter().find(|rule| rule.name == name)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn kind_label(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Development => "dev",
        DependencyKind::Build => "build",
        _ => "normal",
    }
}

fn matches_prefix(dep: &str, prefix: &str) -> bool {
    dep == prefix
        || dep
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
}

/// Evaluates the SKELETON.md dependency rules against workspace metadata.
///
/// All direct dependency kinds are checked; see the crate docs for which
/// rules apply to which kinds. Transitive edges are delegated to
/// `cargo deny check bans`.
#[must_use]
pub fn check_dependencies(metadata: &Metadata) -> Vec<Violation> {
    let members: Vec<&Package> = metadata.workspace_packages();
    let member_names: BTreeSet<&str> = members.iter().map(|pkg| pkg.name.as_str()).collect();
    let mut out = Vec::new();
    let workspace_root = metadata.workspace_root.as_std_path();
    let pin = match ZenPin::checked_in() {
        Ok(pin) => {
            out.extend(check_zen_vendor(workspace_root, &pin));
            Some(pin)
        }
        Err(err) => {
            out.push(violation("zen-pin-file", err.0));
            None
        }
    };

    for rule in CRATES {
        if !member_names.contains(rule.name) {
            out.push(violation(
                "crate-missing",
                format!(
                    "SKELETON.md crate `{}` is not a workspace member",
                    rule.name
                ),
            ));
        }
    }

    let mut seen_edges: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut seen_third: BTreeSet<(&str, &str)> = BTreeSet::new();
    for pkg in &members {
        let name = pkg.name.as_str();
        let Some(rule) = crate_rule(name) else {
            out.push(violation(
                "crate-unclassified",
                format!(
                    "workspace crate `{name}` is not in the SKELETON.md table; amend SKELETON.md and the guard policy first"
                ),
            ));
            continue;
        };
        if name == "norn-cell" {
            check_norn_cell_native_feature(pkg, &mut out);
        }
        for dep in &pkg.dependencies {
            let dep_name = dep.name.as_str();
            check_calendar_dependencies(name, dep, &mut out);
            if dep_name == name {
                // Self dev-dependency used to enable test-only features.
                continue;
            }
            // Vendored zen crates are workspace members but keep the zen
            // source/profile rules rather than the `norn-*` edge table.
            if member_names.contains(dep_name) && !is_zen_crate(dep_name) {
                check_norn_edge(rule, dep, &mut seen_edges, &mut out);
            } else {
                check_third_party(
                    rule,
                    dep,
                    pin.as_ref().map(|pin| (pin, workspace_root)),
                    &mut seen_third,
                    &mut out,
                );
            }
        }
    }

    for (from, to, _) in KNOWN_EDGE_VIOLATIONS {
        if !seen_edges.contains(&(*from, *to)) {
            out.push(violation(
                "stale-allowlist",
                format!(
                    "known violation `{from}` → `{to}` no longer exists; delete it from KNOWN_EDGE_VIOLATIONS and baseline.json"
                ),
            ));
        }
    }
    for (krate, dep, _) in KNOWN_THIRD_PARTY_VIOLATIONS {
        if !seen_third.contains(&(*krate, *dep)) {
            out.push(violation(
                "stale-allowlist",
                format!(
                    "known third-party violation `{krate}` → `{dep}` no longer exists; delete it from KNOWN_THIRD_PARTY_VIOLATIONS and baseline.json"
                ),
            ));
        }
    }
    out.sort();
    out
}

fn check_calendar_dependencies(name: &str, dep: &Dependency, out: &mut Vec<Violation>) {
    let dep_name = dep.name.as_str();
    if matches!(name, "norn-expr" | "norn-importers")
        && dep_name == "chrono"
        && dep.kind != DependencyKind::Development
        && (dep.uses_default_features || dep.features != ["std"])
    {
        out.push(violation(
            if name == "norn-expr" {
                "norn-expr-temporal-features"
            } else {
                "norn-importers-calendar-features"
            },
            format!("`{name}` must use chrono with default features off and only `std`"),
        ));
    }
    if name == "norn-expr"
        && dep_name == "chrono-tz"
        && dep.kind != DependencyKind::Development
        && (dep.req.to_string() != "=0.10.4"
            || dep.uses_default_features
            || dep.features != ["std"])
    {
        out.push(violation(
            "norn-expr-timezone-features",
            "`norn-expr` must use chrono-tz = 0.10.4 with default features off and only `std`"
                .to_owned(),
        ));
    }
}

fn check_norn_cell_native_feature(pkg: &Package, out: &mut Vec<Violation>) {
    const RULE: &str = "norn-cell-native-feature";

    let default_is_native = pkg.features.get("default").is_some_and(|features| {
        features.len() == 1 && features.first().is_some_and(|feature| feature == "native")
    });
    if !default_is_native {
        out.push(violation(
            RULE,
            "`norn-cell` must declare `default = [\"native\"]`".to_owned(),
        ));
    }

    let expected: BTreeSet<String> = NORN_CELL_NATIVE_DEPENDENCIES
        .iter()
        .map(|dependency| format!("dep:{dependency}"))
        .collect();
    let native = pkg.features.get("native");
    let actual: BTreeSet<String> = native.into_iter().flatten().cloned().collect();
    let has_duplicates = native.is_some_and(|features| features.len() != actual.len());
    if actual != expected || has_duplicates {
        out.push(violation(
            RULE,
            format!(
                "`norn-cell` feature `native` must contain exactly {}; found {:?}",
                expected.into_iter().collect::<Vec<_>>().join(", "),
                native
            ),
        ));
    }

    for dependency_name in NORN_CELL_NATIVE_DEPENDENCIES {
        let matching: Vec<_> = pkg
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.name == *dependency_name && dependency.kind == DependencyKind::Normal
            })
            .collect();
        if matching.len() != 1
            || matching
                .first()
                .is_none_or(|dependency| !dependency.optional)
        {
            out.push(violation(
                RULE,
                format!(
                    "`norn-cell` dependency `{dependency_name}` must be exactly one optional normal dependency"
                ),
            ));
        }

        // Cargo creates an implicit feature named after an optional
        // dependency unless the manifest uses `dep:<name>`. Such a feature
        // would expose the native adapter outside `native`.
        if pkg.features.contains_key(*dependency_name) {
            out.push(violation(
                RULE,
                format!(
                    "`norn-cell` dependency `{dependency_name}` has an implicit feature; expose it only through `native`"
                ),
            ));
        }

        for (feature_name, features) in &pkg.features {
            if feature_name == "native" {
                continue;
            }
            if features.iter().any(|feature| {
                let dependency = feature
                    .strip_prefix("dep:")
                    .unwrap_or(feature)
                    .split(['/', '?'])
                    .next()
                    .unwrap_or(feature);
                dependency == *dependency_name
            }) {
                out.push(violation(
                    RULE,
                    format!(
                        "`norn-cell` feature `{feature_name}` exposes `{dependency_name}` outside `native`"
                    ),
                ));
            }
        }
    }
}

// #2684: transports must never leak into the portable shared router.

fn check_norn_edge(
    rule: &CrateRule,
    dep: &Dependency,
    seen: &mut BTreeSet<(&'static str, &'static str)>,
    out: &mut Vec<Violation>,
) {
    let name = rule.name;
    let dep_name = dep.name.as_str();
    if crate_rule(dep_name).is_some_and(|target| target.ring == Ring::TestSupport) {
        if dep.kind != DependencyKind::Development {
            out.push(violation(
                "test-support-dev-only",
                format!(
                    "`{name}` → `{dep_name}` ({} dependency): test-support crates may only be dev-dependencies",
                    kind_label(dep.kind)
                ),
            ));
        }
        return;
    }
    if rule.allowed_norn.contains(&dep_name) {
        return;
    }
    if rule.dev_only_norn.contains(&dep_name) {
        if dep.kind != DependencyKind::Development {
            out.push(violation(
                "norn-dev-only-edge",
                format!(
                    "`{name}` → `{dep_name}` ({} dependency): SKELETON.md admits this edge only as a dev-dependency",
                    kind_label(dep.kind)
                ),
            ));
        }
        return;
    }
    if let Some((from, to, _)) = KNOWN_EDGE_VIOLATIONS
        .iter()
        .find(|(from, to, _)| *from == name && *to == dep_name)
    {
        seen.insert((from, to));
        return;
    }
    out.push(violation(
        "norn-edge",
        format!(
            "`{name}` → `{dep_name}` ({} dependency) is not an allowed SKELETON.md edge",
            kind_label(dep.kind)
        ),
    ));
}

fn check_third_party(
    rule: &CrateRule,
    dep: &Dependency,
    pin: Option<(&ZenPin, &Path)>,
    seen: &mut BTreeSet<(&'static str, &'static str)>,
    out: &mut Vec<Violation>,
) {
    let name = rule.name;
    let dep_name = dep.name.as_str();
    let kind = kind_label(dep.kind);

    if is_zen_crate(dep_name)
        && let Some((pin, workspace_root)) = pin
    {
        check_zen_dependency(name, dep, kind, pin, workspace_root, out);
    }
    // Inside the vendored crate set, zen crates use each other (deny.toml
    // `wrappers` mirror these edges); every other owner rule still applies.
    let intra_vendor = rule.ring == Ring::Vendored && is_zen_crate(dep_name);
    if let Some((_, owner)) = SINGLE_OWNER.iter().find(|(krate, _)| *krate == dep_name)
        && *owner != name
        && !intra_vendor
    {
        out.push(violation(
            "single-owner",
            format!("only `{owner}` may depend on `{dep_name}`; found in `{name}` ({kind})"),
        ));
    }
    if rule.ring == Ring::Vendored {
        let banned = pin.is_some_and(|(pin, _)| {
            pin.forbidden.iter().any(|krate| krate == dep_name)
                || pin.forbidden_wasm32.iter().any(|krate| krate == dep_name)
        });
        if dep.kind != DependencyKind::Development
            && (banned || RING0_FORBIDDEN_EVALUATORS.contains(&dep_name))
        {
            out.push(violation(
                "vendored-forbidden",
                format!(
                    "vendored crate `{name}` depends on banned crate `{dep_name}` ({kind}); see zen-pin.json tree_gate"
                ),
            ));
        }
        return;
    }
    if rule.ring == Ring::TestSupport {
        if dep.kind != DependencyKind::Development && !rule.third_party.contains(&dep_name) {
            out.push(violation(
                "test-support-unlisted",
                format!(
                    "test-support crate `{name}` depends on `{dep_name}` ({kind}), which SKELETON.md does not list; allowed third-party: {:?}",
                    rule.third_party
                ),
            ));
        }
        return;
    }
    if rule.ring != Ring::Zero {
        return;
    }
    if let Some(prefix) = RING0_FORBIDDEN_PREFIXES
        .iter()
        .find(|prefix| matches_prefix(dep_name, prefix))
    {
        out.push(violation(
            "ring0-host",
            format!(
                "ring-0 crate `{name}` directly depends on host/runtime crate `{dep_name}` ({kind}; banned prefix `{prefix}`)"
            ),
        ));
    }
    if RING0_FORBIDDEN_EVALUATORS.contains(&dep_name) {
        out.push(violation(
            "ring0-evaluator",
            format!(
                "ring-0 crate `{name}` depends on evaluator `{dep_name}` ({kind}); only zen-engine/zen-expression are admitted"
            ),
        ));
    }
    if dep.kind == DependencyKind::Development || rule.third_party.contains(&dep_name) {
        return;
    }
    if let Some((krate, known, _)) = KNOWN_THIRD_PARTY_VIOLATIONS
        .iter()
        .find(|(krate, known, _)| *krate == name && *known == dep_name)
    {
        seen.insert((krate, known));
        return;
    }
    out.push(violation(
        "ring0-unlisted",
        format!(
            "ring-0 crate `{name}` depends on `{dep_name}` ({kind}), which SKELETON.md does not list; allowed third-party: {:?}",
            rule.third_party
        ),
    ));
}

/// A direct `zen-*` dependency must be the vendored path crate, keep
/// upstream default features off, and request only Norn profile features
/// (e.g. never `regex-deprecated`, `js`, `http`, or `runtime-rng`).
fn check_zen_dependency(
    name: &str,
    dep: &Dependency,
    kind: &str,
    pin: &ZenPin,
    workspace_root: &Path,
    out: &mut Vec<Violation>,
) {
    let dep_name = dep.name.as_str();
    if !pin.is_vendored_dependency(workspace_root, dep) {
        out.push(violation(
            "zen-source",
            format!(
                "`{name}` → `{dep_name}` must be the vendored path crate {:?} (zen-pin.json); found source {:?}, path {:?}",
                pin.crate_dir(workspace_root, dep_name),
                dep.source.as_ref().map(|src| src.repr.as_str()),
                dep.path
            ),
        ));
    }
    let Some(profile) = pin.profile.get(dep_name) else {
        out.push(violation(
            "zen-profile",
            format!("`{name}` → `{dep_name}` ({kind}) is not a package of the Norn zen profile (zen-pin.json)"),
        ));
        return;
    };
    if dep.uses_default_features {
        out.push(violation(
            "zen-profile",
            format!("`{name}` → `{dep_name}` ({kind}) must set `default-features = false` (Norn profile)"),
        ));
    }
    let extra: Vec<&str> = dep
        .features
        .iter()
        .map(String::as_str)
        .filter(|feature| !profile.contains(*feature))
        .collect();
    if !extra.is_empty() {
        out.push(violation(
            "zen-profile",
            format!(
                "`{name}` → `{dep_name}` ({kind}) enables {extra:?} outside the Norn profile {profile:?} (zen-pin.json)"
            ),
        ));
    }
}

/// Asserts every classified crate and every non-Cargo surface carries
/// README.md and AGENTS.md. Vendored crates ([`Ring::Vendored`]) carry them
/// once, at the vendor root (`zen-pin.json` `vendor.path`).
#[must_use]
pub fn check_layout(workspace_root: &Path, metadata: &Metadata) -> Vec<Violation> {
    let mut dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let vendor_root = ZenPin::checked_in().map(|pin| workspace_root.join(pin.vendor_path));
    for pkg in metadata.workspace_packages() {
        if crate_rule(pkg.name.as_str()).is_some_and(|rule| rule.ring == Ring::Vendored) {
            match &vendor_root {
                Ok(dir) => {
                    dirs.insert(format!("vendored {}", pkg.name), dir.clone());
                }
                Err(err) => return vec![violation("zen-pin-file", err.0.clone())],
            }
            continue;
        }
        if let Some(dir) = pkg.manifest_path.parent() {
            dirs.insert(
                format!("crate {}", pkg.name),
                dir.as_std_path().to_path_buf(),
            );
        }
    }
    for surface in NON_CARGO_SURFACES {
        let dir = workspace_root.join(surface);
        dirs.insert(format!("surface {surface}/"), dir);
    }
    let mut out = Vec::new();
    for (label, dir) in dirs {
        for file in ["README.md", "boundary.json"] {
            if !dir.join(file).is_file() {
                out.push(violation(
                    "boundary-file",
                    format!("{label} is missing {file} ({})", dir.display()),
                ));
            }
        }
    }
    out
}

/// Error from scanning Rust sources.
#[derive(Debug)]
pub enum ScanError {
    /// Directory walk failed.
    Walk(walkdir::Error),
    /// File read failed.
    Io(PathBuf, std::io::Error),
    /// File did not parse as Rust.
    Parse(PathBuf, syn::Error),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Walk(err) => write!(f, "walk failed: {err}"),
            Self::Io(path, err) => write!(f, "read {} failed: {err}", path.display()),
            Self::Parse(path, err) => write!(f, "parse {} failed: {err}", path.display()),
        }
    }
}

impl std::error::Error for ScanError {}

#[derive(Default)]
struct ValidateFnVisitor {
    found: Vec<String>,
}

impl ValidateFnVisitor {
    fn record(&mut self, vis: &syn::Visibility, ident: &syn::Ident) {
        let name = ident.to_string();
        if !matches!(vis, syn::Visibility::Inherited) && name.starts_with("validate_") {
            self.found.push(name);
        }
    }
}

impl<'ast> Visit<'ast> for ValidateFnVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.record(&node.vis, &node.sig.ident);
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.record(&node.vis, &node.sig.ident);
        syn::visit::visit_impl_item_fn(self, node);
    }
}

/// Returns every non-private `fn validate_*` (free function or inherent/trait
/// impl method) under `src_dir`, as `<relative path>::<name>`.
///
/// # Errors
///
/// Returns [`ScanError`] if the directory cannot be walked or a file cannot
/// be read or parsed.
pub fn pub_validate_fns(src_dir: &Path) -> Result<BTreeSet<String>, ScanError> {
    let mut out = BTreeSet::new();
    for entry in walkdir::WalkDir::new(src_dir).sort_by_file_name() {
        let entry = entry.map_err(ScanError::Walk)?;
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source =
            std::fs::read_to_string(path).map_err(|err| ScanError::Io(path.to_path_buf(), err))?;
        let file =
            syn::parse_file(&source).map_err(|err| ScanError::Parse(path.to_path_buf(), err))?;
        let mut visitor = ValidateFnVisitor::default();
        visitor.visit_file(&file);
        let rel = path
            .strip_prefix(src_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for name in visitor.found {
            out.insert(format!("{rel}::{name}"));
        }
    }
    Ok(out)
}

/// `serde_json` functions that turn Serde data into raw JSON bytes. Bytes that
/// are hashed must be canonical, and `serde_json`'s key order depends on
/// whether `preserve_order` is feature-unified into the build (it is whenever
/// `cedar-policy` is linked; #2882). Outside `norn-canonical`, use
/// `norn_canonical::canonical_json_of` / `domain_hash_hex` (or their
/// `norn_ir::canonical` re-exports); for human-facing text use `to_string`.
pub const RAW_JSON_BYTE_FNS: &[&str] =
    &["to_vec", "to_vec_pretty", "to_writer", "to_writer_pretty"];

/// The one crate allowed to call [`RAW_JSON_BYTE_FNS`] in production code.
pub const RAW_JSON_BYTES_OWNER: &str = "norn-canonical";

/// Names `serde_json` is reachable by in one file: the crate itself plus any
/// `use serde_json as x;`, `use serde_json::{self as x}`, or
/// `extern crate serde_json as x;`.
#[derive(Default)]
struct JsonAliasVisitor {
    aliases: BTreeSet<String>,
}

impl JsonAliasVisitor {
    fn use_tree(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.use_tree(prefix, &path.tree);
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                let is_crate = (prefix.is_empty() && rename.ident == "serde_json")
                    || (prefix.last().is_some_and(|last| last == "serde_json")
                        && rename.ident == "self");
                if is_crate {
                    self.aliases.insert(rename.rename.to_string());
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.use_tree(prefix, item);
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for JsonAliasVisitor {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.use_tree(&mut Vec::new(), &node.tree);
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        if node.ident == "serde_json"
            && let Some((_, rename)) = &node.rename
        {
            self.aliases.insert(rename.to_string());
        }
    }
}

struct RawJsonBytesVisitor {
    /// `serde_json` and its aliases in this file.
    crates: BTreeSet<String>,
    found: Vec<String>,
}

impl RawJsonBytesVisitor {
    fn record(&mut self, segments: &[String]) {
        for pair in segments.windows(2) {
            if let [first, second] = pair
                && self.crates.contains(first)
                && RAW_JSON_BYTE_FNS.contains(&second.as_str())
            {
                self.found.push(format!("serde_json::{second}"));
            }
        }
    }

    fn use_tree(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.use_tree(prefix, &path.tree);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                self.record(prefix);
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                self.record(prefix);
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.use_tree(prefix, item);
                }
            }
            // `use serde_json::*;` imports `to_vec` and friends unqualified.
            syn::UseTree::Glob(_) => {
                if prefix.last().is_some_and(|last| self.crates.contains(last)) {
                    self.found.push("serde_json::*".to_owned());
                }
            }
        }
    }

    /// Macro bodies are tokens, not paths: look for `<crate>::<fn>` in the
    /// whitespace-free token text.
    fn tokens(&mut self, text: &str) {
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let crates: Vec<String> = self.crates.iter().cloned().collect();
        for krate in crates {
            for function in RAW_JSON_BYTE_FNS {
                let needle = format!("{krate}::{function}");
                for (start, _) in compact.match_indices(&needle) {
                    let before = compact
                        .get(..start)
                        .and_then(|head| head.chars().next_back());
                    let after = compact
                        .get(start.saturating_add(needle.len())..)
                        .and_then(|tail| tail.chars().next());
                    let boundary =
                        |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                    if boundary(before) && boundary(after) {
                        self.found.push(format!("serde_json::{function}"));
                    }
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for RawJsonBytesVisitor {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        let segments: Vec<String> = node
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        self.record(&segments);
        syn::visit::visit_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        let mut prefix = Vec::new();
        self.use_tree(&mut prefix, &node.tree);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(&node.tokens.to_string());
        syn::visit::visit_macro(self, node);
    }
}

/// Raw JSON byte encodings (`serde_json::to_vec` and friends) in one Rust
/// source file: by path, by `use` (including renames and `serde_json::*`),
/// through a crate alias, or inside macro arguments.
///
/// # Errors
///
/// [`syn::Error`] if `source` does not parse.
pub fn raw_json_byte_calls(source: &str) -> Result<Vec<String>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut aliases = JsonAliasVisitor::default();
    aliases.visit_file(&file);
    let mut crates = aliases.aliases;
    crates.insert("serde_json".to_owned());
    let mut visitor = RawJsonBytesVisitor {
        crates,
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every raw JSON byte encoding under `src_dir`, as `<relative path>: <fn>`.
///
/// # Errors
///
/// Returns [`ScanError`] if the directory cannot be walked or a file cannot
/// be read or parsed.
pub fn raw_json_byte_calls_in(src_dir: &Path) -> Result<BTreeSet<String>, ScanError> {
    let mut out = BTreeSet::new();
    for entry in walkdir::WalkDir::new(src_dir).sort_by_file_name() {
        let entry = entry.map_err(ScanError::Walk)?;
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source =
            std::fs::read_to_string(path).map_err(|err| ScanError::Io(path.to_path_buf(), err))?;
        let found = raw_json_byte_calls(&source)
            .map_err(|err| ScanError::Parse(path.to_path_buf(), err))?;
        for call in found {
            out.insert(format!("{}: {call}", path.display()));
        }
    }
    Ok(out)
}

/// Compares found `norn-ir` `validate_*` functions with the shrink-only
/// allowlist: new entries and stale allowlist entries both fail.
#[must_use]
pub fn check_norn_ir_validators(found: &BTreeSet<String>) -> Vec<Violation> {
    check_validators_against(found, NORN_IR_VALIDATE_ALLOWLIST)
}

/// [`check_norn_ir_validators`] against an explicit allowlist (so the stale
/// entry rule stays testable once the real allowlist is empty).
#[must_use]
pub fn check_validators_against(found: &BTreeSet<String>, allowlist: &[&str]) -> Vec<Violation> {
    let allow: BTreeSet<&str> = allowlist.iter().copied().collect();
    let mut out = Vec::new();
    for item in found {
        if !allow.contains(item.as_str()) {
            out.push(violation(
                "norn-ir-validate",
                format!("new `pub fn {item}` in norn-ir; validation belongs in norn-checker"),
            ));
        }
    }
    for item in allow {
        if !found.contains(item) {
            out.push(violation(
                "stale-allowlist",
                format!("`{item}` is gone from norn-ir; delete it from NORN_IR_VALIDATE_ALLOWLIST and baseline.json"),
            ));
        }
    }
    out
}

/// Error from reading `baseline.json`.
#[derive(Debug)]
pub struct BaselineError(String);

impl fmt::Display for BaselineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid baseline.json: {}", self.0)
    }
}

impl std::error::Error for BaselineError {}

fn baseline_set(root: &serde_json::Value, key: &str) -> Result<BTreeSet<String>, BaselineError> {
    let items = root
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BaselineError(format!("missing array `{key}`")))?;
    items
        .iter()
        .map(|item| match item {
            serde_json::Value::String(text) => Ok(text.clone()),
            serde_json::Value::Array(parts) => parts
                .iter()
                .map(|part| part.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.join(" -> "))
                .ok_or_else(|| BaselineError(format!("`{key}` entries must be strings"))),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Object(_) => {
                Err(BaselineError(format!("unexpected `{key}` entry {item}")))
            }
        })
        .collect()
}

/// Compares every known-violation allowlist with the checked-in baseline
/// (`crates/norn-skeleton-guard/baseline.json`).
///
/// An allowlist entry absent from the baseline fails (allowlists cannot
/// grow), and a baseline entry absent from the allowlist fails (the baseline
/// shrinks in lockstep). Growth of the baseline itself across PRs is rejected
/// in CI by `scripts/baseline-shrink-only.sh`.
///
/// # Errors
///
/// Returns [`BaselineError`] if the baseline is not the expected JSON shape.
pub fn check_baseline(baseline_json: &str) -> Result<Vec<Violation>, BaselineError> {
    let root: serde_json::Value =
        serde_json::from_str(baseline_json).map_err(|err| BaselineError(err.to_string()))?;
    let pairs = |list: &[(&str, &str, &str)]| -> BTreeSet<String> {
        list.iter().map(|(a, b, _)| format!("{a} -> {b}")).collect()
    };
    let lists: [(&str, BTreeSet<String>); 3] = [
        ("norn_edges", pairs(KNOWN_EDGE_VIOLATIONS)),
        ("third_party", pairs(KNOWN_THIRD_PARTY_VIOLATIONS)),
        (
            "norn_ir_validators",
            NORN_IR_VALIDATE_ALLOWLIST
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        ),
    ];
    let mut out = Vec::new();
    for (key, allow) in lists {
        let base = baseline_set(&root, key)?;
        for item in allow.difference(&base) {
            out.push(violation(
                "allowlist-grew",
                format!("`{item}` is allowlisted but not in baseline.json `{key}`; known-violation allowlists may only shrink"),
            ));
        }
        for item in base.difference(&allow) {
            out.push(violation(
                "stale-baseline",
                format!("baseline.json `{key}` still lists `{item}`; delete it"),
            ));
        }
    }
    Ok(out)
}
