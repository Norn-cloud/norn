//! Automatic base-pack loading for `norn check` (`--base` and `norn.json` `basePacks`).
//!
//! Each base pack entry may be a `.json` artifact file (single envelope or JSON
//! array of envelopes), a directory of `*.json` artifacts (recursive, bounded,
//! symlink-free), or a `.ts`/`.py` authoring file or pack directory compiled
//! in-process. All resolved base artifacts are merged into `context.base`
//! before calling the checker so cross-pack `core_preservation`, `core.laws.*`,
//! and canon->`core` link completeness run automatically.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use serde_json::value::RawValue;

use super::fs_io::{
    InvocationBudget, SourceLang, classify_source_file, is_declaration_file, is_json_file,
    read_opened_file_budgeted, validate_safe_relative,
};
use super::trusted_dir::{
    PinnedSourceTarget, TrustedDir, open_source_target_pinned, read_files_in_pinned_subdirs,
};
use super::{
    AuthoringFailure, CompileDirOptions, MAX_CHECK_CONTEXT_BYTES, compile_directory_pinned,
    compile_single_file_pinned, read_norn_manifest_from_dir,
};

/// Scope and tool options used when compiling `.ts`/`.py` base packs.
pub(crate) struct BasePackScope<'a> {
    pub(crate) org: Option<&'a str>,
    pub(crate) env: Option<&'a str>,
    pub(crate) forbid_unsafe_zen: bool,
    pub(crate) tool_root: Option<&'a Path>,
}

/// Parses the optional `"basePacks"` array from a `norn.json` object, validating
/// every entry as a safe project-relative path (no absolute paths or `..`).
pub(super) fn parse_manifest_base_packs(
    parsed: &Value,
    manifest_display: &Path,
) -> Result<Vec<PathBuf>, String> {
    let Some(raw) = parsed.get("basePacks") else {
        return Ok(Vec::new());
    };
    if raw.is_null() {
        return Ok(Vec::new());
    }
    let items = raw.as_array().ok_or_else(|| {
        format!(
            "{}: `basePacks` must be an array of relative path strings",
            manifest_display.display()
        )
    })?;
    let mut base_packs = Vec::with_capacity(items.len());
    for item in items {
        let text = item.as_str().ok_or_else(|| {
            format!(
                "{}: `basePacks` entries must be relative path strings",
                manifest_display.display()
            )
        })?;
        let rel = validate_safe_relative(text)?;
        base_packs.push(rel);
    }
    Ok(base_packs)
}

/// Reads only the optional `"basePacks"` entries from `norn.json` inside `app_root`.
pub(super) fn read_manifest_base_packs_from_dir(
    app_root: &TrustedDir,
    budget: &mut InvocationBudget,
) -> Result<Vec<PathBuf>, String> {
    let Some(raw) = app_root.try_read_regular_file_rel(Path::new("norn.json"), budget)? else {
        return Ok(Vec::new());
    };
    let manifest_display = app_root.display_path().join("norn.json");
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("{}: invalid JSON: {err}", manifest_display.display()))?;
    parse_manifest_base_packs(&parsed, &manifest_display)
}

/// Resolves `norn.json` `basePacks` (strictly inside `app_root`) plus explicit
/// CLI `--base` paths and merges all base artifacts into `context_json`.
pub(crate) fn merge_base_packs_into_context(
    context_json: &str,
    app_root: Option<&TrustedDir>,
    manifest_base_packs: &[PathBuf],
    cli_base_paths: &[PathBuf],
    scope: &BasePackScope<'_>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    if manifest_base_packs.is_empty() && cli_base_paths.is_empty() {
        return Ok(context_json.to_owned());
    }
    let mut loaded: Vec<Box<RawValue>> = Vec::new();
    if let Some(root) = app_root {
        for rel in manifest_base_packs {
            let pinned = open_rel_source_target(root, rel).map_err(AuthoringFailure::Io)?;
            load_pinned_base_pack(pinned, scope, budget, &mut loaded)?;
        }
    }
    for path in cli_base_paths {
        let pinned = open_source_target_pinned(path).map_err(AuthoringFailure::Io)?;
        load_pinned_base_pack(pinned, scope, budget, &mut loaded)?;
    }
    merge_raw_base_artifacts(context_json, loaded).map_err(AuthoringFailure::Io)
}

fn load_pinned_base_pack(
    target: PinnedSourceTarget,
    scope: &BasePackScope<'_>,
    budget: &mut InvocationBudget,
    out: &mut Vec<Box<RawValue>>,
) -> Result<(), AuthoringFailure> {
    match target {
        PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        } => {
            if is_json_file(&display_path) {
                budget
                    .charge_accepted_file(&display_path)
                    .map_err(AuthoringFailure::Io)?;
                let content = read_opened_file_budgeted(opened, &display_path, budget)
                    .map_err(AuthoringFailure::Io)?;
                push_json_artifacts(&display_path, &content, out).map_err(AuthoringFailure::Io)
            } else if classify_source_file(&display_path).is_some() {
                let (org, env) = require_base_compile_scope(&display_path, None, scope)?;
                let opts = CompileDirOptions {
                    org,
                    env,
                    forbid_unsafe_zen: scope.forbid_unsafe_zen,
                    tool_root: scope.tool_root,
                    allow_empty: false,
                };
                let compiled = compile_single_file_pinned(
                    &parent_dir,
                    &file_name,
                    &display_path,
                    opened,
                    &opts,
                    budget,
                )?;
                push_json_artifacts(&display_path, &compiled, out).map_err(AuthoringFailure::Io)
            } else {
                Err(AuthoringFailure::Io(format!(
                    "{}: unsupported base pack file (expected .json, .ts, .tsx, or .py)",
                    display_path.display()
                )))
            }
        }
        PinnedSourceTarget::Directory(root_dir) => {
            load_base_directory(root_dir, scope, budget, out)
        }
    }
}

fn load_base_directory(
    root_dir: TrustedDir,
    scope: &BasePackScope<'_>,
    budget: &mut InvocationBudget,
    out: &mut Vec<Box<RawValue>>,
) -> Result<(), AuthoringFailure> {
    let manifest = read_norn_manifest_from_dir(&root_dir, budget).map_err(AuthoringFailure::Io)?;
    if let Some(app_manifest) = manifest {
        let (org, env) =
            require_base_compile_scope(root_dir.display_path(), Some(&app_manifest), scope)?;
        let opts = CompileDirOptions {
            org,
            env,
            forbid_unsafe_zen: scope.forbid_unsafe_zen,
            tool_root: scope.tool_root,
            allow_empty: false,
        };
        let compiled = compile_directory_pinned(&root_dir, Some(&app_manifest), &opts, budget)?;
        return push_json_artifacts(root_dir.display_path(), &compiled, out)
            .map_err(AuthoringFailure::Io);
    }

    let top_scan = root_dir
        .scan_top_level(budget)
        .map_err(AuthoringFailure::Io)?;
    let has_authoring = top_scan.files.iter().any(|file| {
        let name = file
            .rel_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");
        !is_declaration_file(name)
            && matches!(
                classify_source_file(&file.display_path),
                Some(SourceLang::TypeScript | SourceLang::Python)
            )
    });
    if has_authoring {
        let (org, env) = require_base_compile_scope(root_dir.display_path(), None, scope)?;
        let opts = CompileDirOptions {
            org,
            env,
            forbid_unsafe_zen: scope.forbid_unsafe_zen,
            tool_root: scope.tool_root,
            allow_empty: false,
        };
        let compiled = compile_directory_pinned(&root_dir, None, &opts, budget)?;
        return push_json_artifacts(root_dir.display_path(), &compiled, out)
            .map_err(AuthoringFailure::Io);
    }

    let before = out.len();
    for file in top_scan
        .files
        .iter()
        .filter(|file| is_json_file(&file.display_path))
    {
        push_json_artifacts(&file.display_path, &file.content, out)
            .map_err(AuthoringFailure::Io)?;
    }
    let nested_json =
        read_files_in_pinned_subdirs(&top_scan.subdirs, |_name, path| is_json_file(path), budget)
            .map_err(AuthoringFailure::Io)?;
    for file in nested_json {
        push_json_artifacts(&file.display_path, &file.content, out)
            .map_err(AuthoringFailure::Io)?;
    }
    if out.len() == before {
        return Err(AuthoringFailure::Io(format!(
            "{}: base pack directory contains no .json, .ts, or .py artifacts",
            root_dir.display_path().display()
        )));
    }
    Ok(())
}

fn require_base_compile_scope<'a>(
    display_path: &Path,
    manifest: Option<&'a super::NornAppManifest>,
    scope: &'a BasePackScope<'a>,
) -> Result<(&'a str, &'a str), AuthoringFailure> {
    if let (Some(org), Some(env)) = (scope.org, scope.env) {
        return Ok((org, env));
    }
    if let Some(app) = manifest
        && let (Some(org), Some(env)) = (app.org_id.as_deref(), app.environment_id.as_deref())
    {
        return Ok((org, env));
    }
    Err(AuthoringFailure::Io(format!(
        "{}: compiling authoring --base pack requires explicit scope (use --source with --org/--env or a base pack norn.json scope)",
        display_path.display()
    )))
}

fn push_json_artifacts(
    display_path: &Path,
    text: &str,
    out: &mut Vec<Box<RawValue>>,
) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.starts_with('[') {
        let items: Vec<Box<RawValue>> = serde_json::from_str(trimmed).map_err(|err| {
            format!(
                "{}: invalid JSON artifact array: {err}",
                display_path.display()
            )
        })?;
        for item in items {
            validate_base_artifact_envelope(display_path, item.get())?;
            out.push(item);
        }
        return Ok(());
    }
    if trimmed.starts_with('{') {
        let item: Box<RawValue> = serde_json::from_str(trimmed)
            .map_err(|err| format!("{}: invalid JSON artifact: {err}", display_path.display()))?;
        validate_base_artifact_envelope(display_path, item.get())?;
        out.push(item);
        return Ok(());
    }
    Err(format!(
        "{}: expected a JSON artifact envelope object or array",
        display_path.display()
    ))
}

fn validate_base_artifact_envelope(display_path: &Path, raw: &str) -> Result<(), String> {
    let _envelope: norn_ir::ArtifactEnvelope = serde_json::from_str(raw).map_err(|err| {
        format!(
            "{}: invalid base artifact envelope: {err}",
            display_path.display()
        )
    })?;
    Ok(())
}

fn merge_raw_base_artifacts(
    context_json: &str,
    extra_base: Vec<Box<RawValue>>,
) -> Result<String, String> {
    let mut fields: BTreeMap<String, Box<RawValue>> = serde_json::from_str(context_json)
        .map_err(|err| format!("invalid context JSON object: {err}"))?;

    let mut merged: Vec<Box<RawValue>> = match fields.remove("base") {
        Some(existing) => serde_json::from_str(existing.get())
            .map_err(|err| format!("invalid context.base array: {err}"))?,
        None => Vec::new(),
    };

    let mut seen_exact: BTreeSet<(String, String, String)> = BTreeSet::new();
    for item in &merged {
        if let Some(key) = exact_artifact_key(item.get()) {
            seen_exact.insert(key);
        }
    }
    for item in extra_base {
        if let Some(key) = exact_artifact_key(item.get())
            && !seen_exact.insert(key)
        {
            continue;
        }
        merged.push(item);
    }

    let base_array_json =
        serde_json::to_string(&merged).map_err(|err| format!("serialize context.base: {err}"))?;
    let base_raw = RawValue::from_string(base_array_json)
        .map_err(|err| format!("wrap context.base: {err}"))?;
    fields.insert("base".to_owned(), base_raw);

    let out =
        serde_json::to_string(&fields).map_err(|err| format!("serialize merged context: {err}"))?;
    let used = u64::try_from(out.len()).map_err(|err| err.to_string())?;
    if used > MAX_CHECK_CONTEXT_BYTES {
        return Err("merged context with base packs exceeds 16 MiB check input limit".to_owned());
    }
    Ok(out)
}

fn exact_artifact_key(raw: &str) -> Option<(String, String, String)> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let id = value.get("artifactId").and_then(Value::as_str)?.to_owned();
    let version = value.get("version").and_then(Value::as_str)?.to_owned();
    let canonical = norn_authoring::compact_canonical_json(&value).ok()?;
    Some((id, version, canonical))
}

fn open_rel_source_target(app_root: &TrustedDir, rel: &Path) -> Result<PinnedSourceTarget, String> {
    let mut segments: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(seg) => segments.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "{}: base pack path `{}` must stay inside project root",
                    app_root.display_path().display(),
                    rel.display()
                ));
            }
        }
    }
    let Some(final_seg) = segments.pop() else {
        return Err(format!(
            "{}: base pack path must not be empty",
            app_root.display_path().display()
        ));
    };
    let parent_rel: PathBuf = segments.iter().collect();
    let display_path = app_root.display_path().join(rel);
    open_rel_source_target_impl(app_root, &parent_rel, final_seg, display_path)
}

#[cfg(unix)]
fn open_rel_source_target_impl(
    app_root: &TrustedDir,
    parent_rel: &Path,
    final_seg: &std::ffi::OsStr,
    display_path: PathBuf,
) -> Result<PinnedSourceTarget, String> {
    use super::fs_io::{final_dir_flags, open_entry_no_follow};
    use rustix::fs::{Mode, openat};
    use std::os::fd::OwnedFd;

    let parent_dir = if parent_rel.as_os_str().is_empty() {
        let dup = openat(app_root.as_fd(), ".", final_dir_flags(), Mode::empty())
            .map_err(|err| format!("{}: {err}", app_root.display_path().display()))?;
        TrustedDir::from_fd(dup, app_root.display_path().to_path_buf())
    } else {
        app_root.open_subdir_rel(parent_rel)?
    };
    let opened = open_entry_no_follow(parent_dir.as_fd(), final_seg, &display_path)?;
    let kind = opened
        .metadata()
        .map_err(|err| format!("{}: {err}", display_path.display()))?
        .file_type();
    if kind.is_dir() {
        return Ok(PinnedSourceTarget::Directory(TrustedDir::from_fd(
            OwnedFd::from(opened),
            display_path,
        )));
    }
    if kind.is_file() {
        let file_name = final_seg.to_str().ok_or_else(|| {
            format!(
                "{}: path must end with a UTF-8 file name",
                display_path.display()
            )
        })?;
        return Ok(PinnedSourceTarget::File {
            parent_dir,
            file_name: file_name.to_owned(),
            display_path,
            opened,
        });
    }
    Err(format!(
        "{}: not a regular file or directory",
        display_path.display()
    ))
}

#[cfg(not(unix))]
fn open_rel_source_target_impl(
    app_root: &TrustedDir,
    parent_rel: &Path,
    final_seg: &std::ffi::OsStr,
    display_path: PathBuf,
) -> Result<PinnedSourceTarget, String> {
    let parent_dir = if parent_rel.as_os_str().is_empty() {
        TrustedDir::open(app_root.display_path())?
    } else {
        app_root.open_subdir_rel(parent_rel)?
    };
    let meta = std::fs::symlink_metadata(&display_path)
        .map_err(|err| format!("{}: {err}", display_path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{}: symlinks are refused", display_path.display()));
    }
    if meta.is_dir() {
        return Ok(PinnedSourceTarget::Directory(TrustedDir::open(
            &display_path,
        )?));
    }
    if meta.is_file() {
        let file_name = final_seg.to_str().ok_or_else(|| {
            format!(
                "{}: path must end with a UTF-8 file name",
                display_path.display()
            )
        })?;
        let opened = std::fs::File::open(&display_path)
            .map_err(|err| format!("{}: {err}", display_path.display()))?;
        return Ok(PinnedSourceTarget::File {
            parent_dir,
            file_name: file_name.to_owned(),
            display_path,
            opened,
        });
    }
    Err(format!(
        "{}: not a regular file or directory",
        display_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};

    use crate::authoring::{CheckSourceRequest, execute_check_source};
    use crate::{CheckArgs, run_check_args};

    type TestResult = Result<(), Box<dyn Error>>;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Result<Self, Box<dyn Error>> {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "norn-cli-base-packs-{label}-{}-{seq}",
                std::process::id()
            ));
            if root.exists() {
                std::fs::remove_dir_all(&root)?;
            }
            std::fs::create_dir_all(&root)?;
            Ok(Self(std::fs::canonicalize(&root)?))
        }

        fn write(&self, rel: &str, text: &str) -> Result<PathBuf, Box<dyn Error>> {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, text)?;
            Ok(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Err(_cleanup) = std::fs::remove_dir_all(&self.0) {}
        }
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn tools_norn_ts_root() -> PathBuf {
        repo_root().join("tools/norn-ts")
    }

    const BAD_ROLE_TS: &str = r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const RoleInput = Schema.Struct({
  id: Schema.String,
  role: Norn.ref("core.code"),
  valid_from: Schema.NullOr(Norn.ref("iso8601.date")),
  valid_to: Schema.NullOr(Norn.ref("iso8601.date")),
})

export const CustomerRole = Norn.type({
  name: "acme.CustomerRole",
  title: "Customer Role",
  version: "0.1.0",
  implements: ["core.AgentRole"],
  properties: RoleInput,
  links: [
    {
      id: "agent",
      target: "core.Agent",
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "counterparty",
      target: "core.Agent",
      cardinality: { to: "one", from: "many" },
    },
  ],
  identity: [{ name: "primary", properties: ["id"] }],
})

export const CreateCustomerRole = Norn.action({
  id: "acme.customer_role.create",
  title: "Create customer role",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: RoleInput,
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CustomerRole, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: CustomerRole,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})
"#;

    const GUARDED_BASE_TS: &str = r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Guarded = Norn.interfaceType({
  name: "core.Guarded",
  role: "thing",
  title: "Guarded",
  version: "0.1.0",
  properties: Schema.Struct({
    amount: Schema.Int,
  }),
  invariants: [
    {
      id: "positive_amount",
      message: "amount must stay strictly positive",
      expression: Norn.expression(Schema.Struct({ amount: Schema.Int }), (self) => self.amount > 0),
    },
  ],
})
"#;

    const BREAK_BUCKET_TS: &str = r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const BucketInput = Schema.Struct({
  id: Schema.String,
  amount: Schema.Int,
})

export const Bucket = Norn.type({
  name: "acme.Bucket",
  title: "Bucket",
  version: "0.1.0",
  implements: ["core.Guarded"],
  properties: BucketInput,
  identity: [{ name: "primary", properties: ["id"] }],
})

export const BreakBucket = Norn.action({
  id: "acme.bucket.break",
  title: "Break bucket",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: BucketInput,
  output: Schema.Struct({ id: Schema.String }),
  derive: [
    {
      field: "amount",
      inputRequired: false,
      expression: Norn.expression(BucketInput, () => 0),
    },
  ],
  writes: [{ type: Bucket, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Bucket,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})
"#;

    const SHARED_TAG_PY: &str = r#"from __future__ import annotations

from norn_py import Concept, Field, KeySpec


class SharedTag(
    Concept,
    name="shared.Tag",
    title="Shared Tag",
    version="0.1.0",
    identity=(KeySpec("primary", ("id",)),),
):
    id: Field[str]
"#;

    const APP_ITEM_PY: &str = r#"from __future__ import annotations

from norn_py import Concept, Field, KeySpec


class Item(
    Concept,
    name="app.Item",
    title="Item",
    version="0.1.0",
    identity=(KeySpec("primary", ("id",)),),
):
    id: Field[str]
"#;

    #[test]
    fn cli_base_json_dir_enforces_core_link_binding_on_source_pack() -> TestResult {
        let scratch = Scratch::new("core-dir")?;
        let ts_root = tools_norn_ts_root();
        let core_dir = repo_root().join("registry/core/artifacts");
        let bad_source = scratch.write("bad_role.ts", BAD_ROLE_TS)?;

        let refused = execute_check_source(&CheckSourceRequest {
            source: &bad_source,
            context: None,
            base: &[core_dir],
            org: Some("org_demo"),
            env: Some("dev"),
            forbid_unsafe_zen: false,
            tool_root: Some(&ts_root),
        });
        assert_eq!(
            refused.exit_code, 1,
            "missing required core.AgentRole link must refuse: stdout={} stderr={}",
            refused.stdout, refused.stderr
        );
        let report: Value = serde_json::from_str(&refused.stdout)?;
        assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(false));
        let diagnostics = report
            .pointer("/diagnostics/diagnostics")
            .and_then(Value::as_array)
            .ok_or("missing diagnostics array")?;
        assert!(
            diagnostics.iter().any(|diag| {
                diag.get("code").and_then(Value::as_str)
                    == Some("core.laws.missing_required_link_binding")
            }),
            "expected core.laws.missing_required_link_binding, got: {report}"
        );
        Ok(())
    }

    #[test]
    fn cli_base_ts_pack_catches_core_law_violation_in_specializing_source() -> TestResult {
        let scratch = Scratch::new("ts-base")?;
        let ts_root = tools_norn_ts_root();
        let base_ts = scratch.write("core_base.ts", GUARDED_BASE_TS)?;
        let app_ts = scratch.write("app.ts", BREAK_BUCKET_TS)?;

        let refused = execute_check_source(&CheckSourceRequest {
            source: &app_ts,
            context: None,
            base: &[base_ts],
            org: Some("org_demo"),
            env: Some("dev"),
            forbid_unsafe_zen: false,
            tool_root: Some(&ts_root),
        });
        assert_eq!(
            refused.exit_code, 1,
            "deriving amount = 0 must violate core.Guarded invariant: stdout={} stderr={}",
            refused.stdout, refused.stderr
        );
        let report: Value = serde_json::from_str(&refused.stdout)?;
        let diagnostics = report
            .pointer("/diagnostics/diagnostics")
            .and_then(Value::as_array)
            .ok_or("missing diagnostics array")?;
        assert!(
            diagnostics.iter().any(|diag| {
                diag.get("code").and_then(Value::as_str) == Some("core.laws.preservation_violated")
            }),
            "expected core.laws.preservation_violated, got: {report}"
        );
        Ok(())
    }

    #[test]
    fn manifest_base_packs_loads_python_base_and_refuses_traversal() -> TestResult {
        let scratch = Scratch::new("manifest-base")?;
        let core_units =
            std::fs::read_to_string(repo_root().join("registry/core/artifacts/core.units.json"))?;
        scratch.write("vendor/core/core.units.json", &core_units)?;
        scratch.write("base/common.py", SHARED_TAG_PY)?;
        scratch.write("src/item.py", APP_ITEM_PY)?;
        scratch.write(
            "norn.json",
            &json!({
                "format": "norn-project/1",
                "name": "base-pack-app",
                "sources": ["src"],
                "basePacks": ["vendor/core/core.units.json", "base/common.py"],
                "scope": { "orgId": "local", "environmentId": "dev" },
            })
            .to_string(),
        )?;

        let admitted = execute_check_source(&CheckSourceRequest {
            source: &scratch.0,
            context: None,
            base: &[],
            org: None,
            env: None,
            forbid_unsafe_zen: false,
            tool_root: None,
        });
        assert_eq!(
            admitted.exit_code, 0,
            "manifest basePacks check failed: stdout={} stderr={}",
            admitted.stdout, admitted.stderr
        );

        scratch.write(
            "norn.json",
            &json!({
                "format": "norn-project/1",
                "name": "bad-base-pack-app",
                "sources": ["src"],
                "basePacks": ["../escape"],
                "scope": { "orgId": "local", "environmentId": "dev" },
            })
            .to_string(),
        )?;
        let refused = execute_check_source(&CheckSourceRequest {
            source: &scratch.0,
            context: None,
            base: &[],
            org: None,
            env: None,
            forbid_unsafe_zen: false,
            tool_root: None,
        });
        assert_eq!(refused.exit_code, 2);
        assert!(
            refused.stderr.contains("stay inside"),
            "expected traversal refusal, got: {}",
            refused.stderr
        );
        Ok(())
    }

    #[test]
    fn check_artifacts_merges_cli_base_pack_with_context() -> TestResult {
        let scratch = Scratch::new("artifacts-base")?;
        let crm_ir = tools_norn_ts_root().join("fixtures/definitions/crm.ir.json");
        let core_units = repo_root().join("registry/core/artifacts/core.units.json");
        let context_path = scratch.write("context.json", "{}")?;

        let out = run_check_args(&CheckArgs {
            artifacts: Some(crm_ir),
            context: Some(context_path),
            base: vec![core_units],
            source: None,
            org: None,
            env: None,
            forbid_unsafe_zen: false,
            tool_root: None,
        });
        assert_eq!(
            out.exit_code, 0,
            "check --artifacts --base failed: stdout={} stderr={}",
            out.stdout, out.stderr
        );
        let report: Value = serde_json::from_str(&out.stdout)?;
        assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));
        Ok(())
    }
}
