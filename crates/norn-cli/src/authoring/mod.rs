//! Native Rust authoring subcommands (`compile`, `fmt`, `profile`, `question`, `dashboard`)
//! and in-process `check --source` wiring both `norn-authoring` (`TypeScript`) and
//! `norn-authoring-py` (Python) with zero Node/Bun/`tsc`/`CPython`.

mod atomic_io;
pub(crate) mod base_packs;
mod fmt;
mod fs_io;
mod pack_json;
mod pack_manifest;
mod py;
mod retained;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod tests;
mod trusted_dir;
mod ts;
mod zero_ci;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::{Value, json};

use self::atomic_io::write_file_atomic;
pub(crate) use self::fs_io::InvocationBudget;
use self::fs_io::{
    BoundedJsonArray, LoadedFile, SourceLang, classify_source_file, display_posix_path,
    is_declaration_file, is_json_file, read_opened_file_budgeted, validate_safe_relative,
};
use self::retained::validate_retained_definition;
pub(crate) use self::trusted_dir::{PinnedSourceTarget, TrustedDir, open_source_target_pinned};
use self::ts::{TsCompileOptions, TsCompileTarget};
pub(crate) use self::zero_ci::find_norn_app_root_from_dir;
use self::zero_ci::{AppRootMatch, ZeroCiOutcome, check_zero_ci_app_root};

pub(super) const MAX_CHECK_CONTEXT_BYTES: u64 = 16 * 1024 * 1024;

/// Structured CLI execution result supporting exit codes `0` (ok), `1` (refusal), and `2` (usage/IO).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: u8,
}

impl CommandOutput {
    pub(crate) fn ok(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit_code: 0,
        }
    }

    pub(crate) fn refused(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit_code: 1,
        }
    }

    pub(crate) fn usage_or_io_error(message: &str) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("norn: {message}"),
            exit_code: 2,
        }
    }
}

/// Internal authoring failure distinguishing compiler diagnostics (exit `1`) from usage/IO (exit `2`).
pub(crate) enum AuthoringFailure {
    Diagnostics(String),
    Io(String),
}

impl AuthoringFailure {
    pub(crate) fn into_output(self) -> CommandOutput {
        match self {
            Self::Diagnostics(doc) => CommandOutput::refused(doc),
            Self::Io(message) => CommandOutput::usage_or_io_error(&message),
        }
    }
}

/// Arguments for `norn compile`.
#[derive(Debug, Args)]
pub(crate) struct CompileArgs {
    /// `TypeScript` (`.ts`) or Python (`.py`) authoring file or directory.
    pub(crate) path: PathBuf,
    /// Explicit organization scope (`scope.orgId`).
    #[arg(
        long,
        requires = "env",
        conflicts_with = "registry",
        required_unless_present = "pack_json"
    )]
    pub(crate) org: Option<String>,
    /// Explicit environment scope (`scope.environmentId`).
    #[arg(
        long,
        requires = "org",
        conflicts_with = "registry",
        required_unless_present = "pack_json"
    )]
    pub(crate) env: Option<String>,
    /// Promote `NORN-AUTHORING-UNSAFE-ZEN` warnings to hard errors.
    #[arg(long)]
    pub(crate) forbid_unsafe_zen: bool,
    /// Write compiled canonical JSON artifacts to this file instead of stdout.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect`.
    #[arg(long)]
    pub(crate) tool_root: Option<PathBuf>,
    /// Emit full pack compilation metadata (`artifacts`, `sources`, `sourceMap`, `warnings`, `examples`, `retained`, `flows`, `flowSourceMap`, `modules`) as JSON.
    #[arg(long)]
    pub(crate) pack_json: bool,
    /// Repo-relative registry package path (enables active/system envelopes with empty scope).
    #[arg(long, requires = "pack_json")]
    pub(crate) registry: Option<String>,
    /// Provenance root directory for computing relative `sourcePath` and source-map paths.
    #[arg(long, requires = "pack_json")]
    pub(crate) root: Option<PathBuf>,
    /// Explicit provenance `sourcePath` override for single-file compilation.
    #[arg(long, requires = "pack_json")]
    pub(crate) source_path: Option<String>,
    /// Path to a JSON manifest listing explicit `TypeScript` modules to compile together.
    #[arg(long, requires = "pack_json")]
    pub(crate) modules: Option<PathBuf>,
    /// Allow modules that declare no definitions or flows.
    #[arg(long, requires = "pack_json")]
    pub(crate) allow_empty: bool,
    /// Optional per-module expansion work limit override (clamped to compiler ceilings).
    #[arg(long, requires = "pack_json")]
    pub(crate) max_expansion_work: Option<usize>,
}

/// Arguments for `norn fmt`.
#[derive(Debug, Args)]
pub(crate) struct FmtArgs {
    /// Check formatting without modifying files (exit 1 if unformatted).
    #[arg(long)]
    pub(crate) check: bool,
    /// `TypeScript` (`.ts`, `.tsx`) or Python (`.py`) file or directory.
    pub(crate) path: PathBuf,
}

/// Arguments for `norn profile`.
#[derive(Debug, Args)]
pub(crate) struct ProfileArgs {
    /// `TypeScript` authoring file (`.ts`) to check against the `norn-effect` profile.
    pub(crate) path: PathBuf,
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect`.
    #[arg(long)]
    pub(crate) tool_root: Option<PathBuf>,
}

/// Arguments for `norn question`.
#[derive(Debug, Args)]
pub(crate) struct QuestionArgs {
    /// `TypeScript` question module (`.ts`).
    pub(crate) path: PathBuf,
    /// Write compiled JSON questions to this file instead of stdout.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect`.
    #[arg(long)]
    pub(crate) tool_root: Option<PathBuf>,
}

/// Arguments for `norn dashboard`.
#[derive(Debug, Args)]
pub(crate) struct DashboardArgs {
    /// `TypeScript` dashboard module (`.ts`).
    pub(crate) path: PathBuf,
    /// Write compiled JSON dashboards to this file instead of stdout.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect`.
    #[arg(long)]
    pub(crate) tool_root: Option<PathBuf>,
}

/// Inputs for `norn check --source`.
pub(crate) struct CheckSourceRequest<'a> {
    pub(crate) source: &'a Path,
    pub(crate) context: Option<&'a Path>,
    pub(crate) base: &'a [PathBuf],
    pub(crate) org: Option<&'a str>,
    pub(crate) env: Option<&'a str>,
    pub(crate) forbid_unsafe_zen: bool,
    pub(crate) tool_root: Option<&'a Path>,
    pub(crate) boundary_root: Option<&'a Path>,
}

pub(super) struct NornAppManifest {
    pub(super) sources: Vec<PathBuf>,
    pub(super) org_id: Option<String>,
    pub(super) environment_id: Option<String>,
}

enum JsonSourceEntry {
    Artifact(Value),
    RetainedDefinition,
}

pub(super) struct CompileDirOptions<'a> {
    pub(super) org: &'a str,
    pub(super) env: &'a str,
    pub(super) forbid_unsafe_zen: bool,
    pub(super) tool_root: Option<&'a Path>,
    pub(super) boundary_root: Option<&'a Path>,
    pub(super) allow_empty: bool,
}

pub(super) fn read_norn_manifest_from_dir(
    app_root: &TrustedDir,
    budget: &mut InvocationBudget,
) -> Result<Option<NornAppManifest>, String> {
    let Some(raw) = app_root.try_read_regular_file_rel(Path::new("norn.json"), budget)? else {
        return Ok(None);
    };
    let manifest_display = app_root.display_path().join("norn.json");
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("{}: invalid JSON: {err}", manifest_display.display()))?;
    let items = parsed
        .get("sources")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "{}: `sources` must be an array of relative directory paths",
                manifest_display.display()
            )
        })?;
    let mut sources = Vec::with_capacity(items.len());
    for item in items {
        let text = item.as_str().ok_or_else(|| {
            format!(
                "{}: `sources` entries must be relative strings",
                manifest_display.display()
            )
        })?;
        let rel = validate_safe_relative(text)?;
        sources.push(rel);
    }
    let _validated_base_packs = base_packs::parse_manifest_base_packs(&parsed, &manifest_display)?;
    let org_id = parsed
        .pointer("/scope/orgId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let environment_id = parsed
        .pointer("/scope/environmentId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(Some(NornAppManifest {
        sources,
        org_id,
        environment_id,
    }))
}

fn validate_non_empty_scope(label: &str, value: &str) -> Result<(), AuthoringFailure> {
    if value.trim().is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{label} must be a non-empty scope identifier"
        )));
    }
    Ok(())
}

fn stream_unique_json_artifact(
    artifact: &Value,
    source_label: &str,
    seen_ids: &mut BTreeSet<String>,
    out: &mut BoundedJsonArray,
    budget: &mut InvocationBudget,
) -> Result<(), AuthoringFailure> {
    if let Some(id) = artifact.get("artifactId").and_then(Value::as_str)
        && !seen_ids.insert(id.to_owned())
    {
        let refused = json!({
            "ok": false,
            "diagnostics": [{
                "code": "NORN-FLOW-LITERAL",
                "message": format!("duplicate artifactId `{id}` declared in `{source_label}`"),
                "file": source_label,
            }],
        })
        .to_string();
        return Err(AuthoringFailure::Diagnostics(refused));
    }
    out.push_value("compiled json artifact", artifact, budget)
        .map_err(AuthoringFailure::Io)
}

fn classify_json_source(
    file: &LoadedFile,
    seen_retained_ids: &mut BTreeSet<String>,
) -> Result<JsonSourceEntry, AuthoringFailure> {
    let shown = display_posix_path(&file.display_path);
    let parsed: Value = serde_json::from_str(&file.content)
        .map_err(|err| AuthoringFailure::Io(format!("{shown}: invalid JSON: {err}")))?;
    let Some(obj) = parsed.as_object() else {
        return Err(AuthoringFailure::Io(format!(
            "{shown}: unsupported JSON source (expected a precompiled artifact envelope or retained definition object)"
        )));
    };
    if obj.get("artifactId").and_then(Value::as_str).is_some()
        && obj.get("irVersion").and_then(Value::as_str).is_some()
    {
        let _envelope: norn_ir::ArtifactEnvelope =
            serde_json::from_str(&file.content).map_err(|err| {
                AuthoringFailure::Io(format!(
                    "{shown}: invalid precompiled artifact envelope: {err}"
                ))
            })?;
        return Ok(JsonSourceEntry::Artifact(parsed));
    }
    if let Some(kind) = obj.get("kind").and_then(Value::as_str)
        && matches!(kind, "approval" | "question" | "dashboard")
    {
        validate_retained_definition(&shown, obj, kind, seen_retained_ids)?;
        return Ok(JsonSourceEntry::RetainedDefinition);
    }
    Err(AuthoringFailure::Io(format!(
        "{shown}: unsupported JSON source (expected an artifact envelope with `artifactId`/`irVersion` or a retained definition with `kind`/`id`/`payload`)"
    )))
}

struct DirCompileState<'a> {
    seen_ids: &'a mut BTreeSet<String>,
    seen_retained_ids: &'a mut BTreeSet<String>,
    out: &'a mut BoundedJsonArray,
    budget: &'a mut InvocationBudget,
}

fn compile_source_dir_entries(
    source_dir: &TrustedDir,
    opts: &CompileDirOptions<'_>,
    state: &mut DirCompileState<'_>,
) -> Result<(), AuthoringFailure> {
    let top_scan = source_dir
        .scan_top_level(state.budget)
        .map_err(AuthoringFailure::Io)?;
    let mut typescript_files = Vec::new();
    let mut python_files = Vec::new();
    let mut json_files = Vec::new();
    for file in top_scan.files {
        let name = file
            .rel_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");
        if is_declaration_file(name) {
            continue;
        }
        if is_json_file(&file.display_path) {
            json_files.push(file);
            continue;
        }
        match classify_source_file(&file.display_path) {
            Some(SourceLang::TypeScript) => typescript_files.push(file),
            Some(SourceLang::Python) => python_files.push(file),
            None => {
                return Err(AuthoringFailure::Io(format!(
                    "{}: unsupported file in authoring source directory (expected .ts, .tsx, .py, or .json)",
                    file.display_path.display()
                )));
            }
        }
    }

    let before_count = state.out.len();
    let mut retained_count = 0_usize;
    if !typescript_files.is_empty() {
        let ts_opts = TsCompileOptions {
            org: opts.org,
            env: opts.env,
            forbid_unsafe_zen: opts.forbid_unsafe_zen,
            tool_root: opts.tool_root,
            boundary_root: opts.boundary_root,
        };
        ts::compile_ts_targets_into(
            TsCompileTarget::Directory {
                root_dir: source_dir,
                root_files: typescript_files,
                support_files: &json_files,
                pinned_subdirs: &top_scan.subdirs,
            },
            &ts_opts,
            state.seen_ids,
            state.out,
            state.budget,
        )?;
    }
    if !python_files.is_empty() {
        py::compile_python_files_into(
            &python_files,
            opts.org,
            opts.env,
            state.seen_ids,
            state.out,
            state.budget,
        )?;
    }
    for json_file in &json_files {
        match classify_json_source(json_file, state.seen_retained_ids)? {
            JsonSourceEntry::Artifact(val) => {
                let label = display_posix_path(&json_file.display_path);
                stream_unique_json_artifact(&val, &label, state.seen_ids, state.out, state.budget)?;
            }
            JsonSourceEntry::RetainedDefinition => {
                retained_count = retained_count.saturating_add(1);
            }
        }
    }
    if state.out.len() == before_count && retained_count == 0 && !opts.allow_empty {
        return Err(AuthoringFailure::Io(format!(
            "{}: no .ts, .py, or .json authoring artifacts found",
            source_dir.display_path().display()
        )));
    }
    Ok(())
}

fn compile_manifest_project(
    app_root: &TrustedDir,
    manifest: &NornAppManifest,
    opts: &CompileDirOptions<'_>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    if manifest.sources.is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{}: norn.json declares no authoring source directories",
            app_root.display_path().display()
        )));
    }
    let dir_opts = CompileDirOptions {
        org: opts.org,
        env: opts.env,
        forbid_unsafe_zen: opts.forbid_unsafe_zen,
        tool_root: opts.tool_root,
        boundary_root: opts.boundary_root,
        allow_empty: true,
    };
    let mut out = BoundedJsonArray::new();
    let mut seen_ids = BTreeSet::new();
    let mut seen_retained_ids = BTreeSet::new();
    let mut state = DirCompileState {
        seen_ids: &mut seen_ids,
        seen_retained_ids: &mut seen_retained_ids,
        out: &mut out,
        budget,
    };
    for rel_source in &manifest.sources {
        let source_dir = app_root
            .open_subdir_rel(rel_source)
            .map_err(AuthoringFailure::Io)?;
        compile_source_dir_entries(&source_dir, &dir_opts, &mut state)?;
    }
    if out.is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{}: norn.json sources contain no authoring artifacts",
            app_root.display_path().display()
        )));
    }
    out.finish("merged project artifacts", budget)
        .map_err(AuthoringFailure::Io)
}

pub(super) fn compile_single_file_pinned(
    parent_dir: &TrustedDir,
    file_name: &str,
    display_path: &Path,
    opened: std::fs::File,
    opts: &CompileDirOptions<'_>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    match classify_source_file(display_path) {
        Some(SourceLang::Python) => {
            budget
                .charge_accepted_file(display_path)
                .map_err(AuthoringFailure::Io)?;
            let content = read_opened_file_budgeted(opened, display_path, budget)
                .map_err(AuthoringFailure::Io)?;
            let loaded = LoadedFile {
                display_path: display_path.to_path_buf(),
                rel_path: PathBuf::from(file_name),
                content,
            };
            py::compile_python_files(&[loaded], opts.org, opts.env, budget)
        }
        Some(SourceLang::TypeScript) => {
            budget
                .charge_accepted_file(display_path)
                .map_err(AuthoringFailure::Io)?;
            let content = read_opened_file_budgeted(opened, display_path, budget)
                .map_err(AuthoringFailure::Io)?;
            let ts_opts = TsCompileOptions {
                org: opts.org,
                env: opts.env,
                forbid_unsafe_zen: opts.forbid_unsafe_zen,
                tool_root: opts.tool_root,
                boundary_root: opts.boundary_root,
            };
            ts::compile_ts_targets(
                TsCompileTarget::SingleFile {
                    parent_dir,
                    file_name,
                    display_path,
                    content,
                },
                &ts_opts,
                budget,
            )
        }
        None => Err(AuthoringFailure::Io(format!(
            "{}: expected a .ts or .py authoring source file",
            display_path.display()
        ))),
    }
}

pub(super) fn compile_directory_pinned(
    root_dir: &TrustedDir,
    manifest: Option<&NornAppManifest>,
    opts: &CompileDirOptions<'_>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    if let Some(app_manifest) = manifest {
        return compile_manifest_project(root_dir, app_manifest, opts, budget);
    }
    let mut out = BoundedJsonArray::new();
    let mut seen_ids = BTreeSet::new();
    let mut seen_retained_ids = BTreeSet::new();
    let mut state = DirCompileState {
        seen_ids: &mut seen_ids,
        seen_retained_ids: &mut seen_retained_ids,
        out: &mut out,
        budget,
    };
    compile_source_dir_entries(root_dir, opts, &mut state)?;
    out.finish("compiled directory artifacts", budget)
        .map_err(AuthoringFailure::Io)
}

fn compile_path_to_json(
    path: &Path,
    org: &str,
    env: &str,
    forbid_unsafe_zen: bool,
    tool_root: Option<&Path>,
    boundary_root: Option<&Path>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    validate_non_empty_scope("--org", org)?;
    validate_non_empty_scope("--env", env)?;
    let opts = CompileDirOptions {
        org,
        env,
        forbid_unsafe_zen,
        tool_root,
        boundary_root,
        allow_empty: false,
    };
    match open_source_target_pinned(path).map_err(AuthoringFailure::Io)? {
        PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        } => compile_single_file_pinned(
            &parent_dir,
            &file_name,
            &display_path,
            opened,
            &opts,
            budget,
        ),
        PinnedSourceTarget::Directory(root_dir) => {
            let manifest =
                read_norn_manifest_from_dir(&root_dir, budget).map_err(AuthoringFailure::Io)?;
            compile_directory_pinned(&root_dir, manifest.as_ref(), &opts, budget)
        }
    }
}

/// Compiles a `.ts`/`.py` authoring file or directory to a canonical JSON artifact array.
pub(crate) fn compile_source_to_json(
    path: &Path,
    org: &str,
    env: &str,
    forbid_unsafe_zen: bool,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    let mut budget = InvocationBudget::new();
    compile_path_to_json(
        path,
        org,
        env,
        forbid_unsafe_zen,
        tool_root,
        None,
        &mut budget,
    )
}

/// Executes `norn compile`.
pub(crate) fn execute_compile(args: &CompileArgs) -> CommandOutput {
    execute_compile_with_boundary(args, None)
}

/// Executes `norn compile` confined strictly within `boundary_root`.
pub(crate) fn execute_compile_confined(args: &CompileArgs, boundary_root: &Path) -> CommandOutput {
    execute_compile_with_boundary(args, Some(boundary_root))
}

fn execute_compile_with_boundary(
    args: &CompileArgs,
    boundary_root: Option<&Path>,
) -> CommandOutput {
    if args.pack_json {
        return pack_json::execute_compile_pack_json(args, boundary_root);
    }
    let (Some(org), Some(env)) = (args.org.as_deref(), args.env.as_deref()) else {
        return CommandOutput::usage_or_io_error("--org and --env are required");
    };
    let mut budget = InvocationBudget::new();
    match compile_path_to_json(
        &args.path,
        org,
        env,
        args.forbid_unsafe_zen,
        args.tool_root.as_deref(),
        boundary_root,
        &mut budget,
    ) {
        Ok(canonical) => {
            if let Some(out_path) = &args.out {
                match write_file_atomic(out_path, &canonical) {
                    Ok(()) => CommandOutput::ok(String::new()),
                    Err(err) => CommandOutput::usage_or_io_error(&err),
                }
            } else {
                CommandOutput::ok(canonical)
            }
        }
        Err(failure) => failure.into_output(),
    }
}

/// Executes `norn fmt`.
pub(crate) fn execute_fmt(args: &FmtArgs) -> CommandOutput {
    fmt::execute_fmt(args)
}

/// Executes `norn profile`.
pub(crate) fn execute_profile(args: &ProfileArgs) -> CommandOutput {
    match ts::run_profile(&args.path, args.tool_root.as_deref()) {
        Ok((json_out, true)) => CommandOutput::ok(json_out),
        Ok((json_out, false)) => CommandOutput::refused(json_out),
        Err(failure) => failure.into_output(),
    }
}

/// Executes `norn question`.
pub(crate) fn execute_question(args: &QuestionArgs) -> CommandOutput {
    match ts::run_question(&args.path, args.out.as_deref(), args.tool_root.as_deref()) {
        Ok(output) => CommandOutput::ok(output),
        Err(failure) => failure.into_output(),
    }
}

/// Executes `norn dashboard`.
pub(crate) fn execute_dashboard(args: &DashboardArgs) -> CommandOutput {
    match ts::run_dashboard(&args.path, args.out.as_deref(), args.tool_root.as_deref()) {
        Ok(output) => CommandOutput::ok(output),
        Err(failure) => failure.into_output(),
    }
}

fn read_norn_scope_from_dir(
    app_root: &TrustedDir,
    budget: &mut InvocationBudget,
) -> Result<Option<(String, String)>, String> {
    let Some(raw) = app_root.try_read_regular_file_rel(Path::new("norn.json"), budget)? else {
        return Ok(None);
    };
    let manifest_display = app_root.display_path().join("norn.json");
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("{}: invalid JSON: {err}", manifest_display.display()))?;
    let org_id = parsed
        .pointer("/scope/orgId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let environment_id = parsed
        .pointer("/scope/environmentId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(org_id.zip(environment_id))
}

fn resolve_check_scope(
    req: &CheckSourceRequest<'_>,
    default_scope: Option<(&str, &str)>,
) -> Result<(String, String), String> {
    match (req.org, req.env) {
        (Some(org), Some(env)) => Ok((org.to_owned(), env.to_owned())),
        (Some(_), None) | (None, Some(_)) => Err(
            "--org and --env must be supplied together with --source (or omit both when checking a norn.json project)"
                .to_owned(),
        ),
        (None, None) => {
            if let Some((org_id, env_id)) = default_scope {
                return Ok((org_id.to_owned(), env_id.to_owned()));
            }
            Err(
                "--org and --env are required with --source (or omit both when checking a norn.json project with scope)"
                    .to_owned(),
            )
        }
    }
}

struct CompiledCheckSource {
    artifacts_json: String,
    org: String,
    env: String,
    app_root: Option<TrustedDir>,
}

fn compile_check_source_pinned(
    target: PinnedSourceTarget,
    app_root: Option<AppRootMatch>,
    req: &CheckSourceRequest<'_>,
    budget: &mut InvocationBudget,
) -> Result<CompiledCheckSource, AuthoringFailure> {
    match target {
        PinnedSourceTarget::Directory(root_dir) => {
            let manifest = if matches!(app_root, Some(AppRootMatch::SameAsStart)) {
                read_norn_manifest_from_dir(&root_dir, budget).map_err(AuthoringFailure::Io)?
            } else {
                None
            };
            let ancestor_scope = if req.org.is_none() && req.env.is_none() {
                match app_root.as_ref() {
                    Some(AppRootMatch::Ancestor(dir)) => {
                        read_norn_scope_from_dir(dir, budget).map_err(AuthoringFailure::Io)?
                    }
                    Some(AppRootMatch::SameAsStart) | None => None,
                }
            } else {
                None
            };
            let default_scope = manifest
                .as_ref()
                .and_then(|app| app.org_id.as_deref().zip(app.environment_id.as_deref()))
                .or_else(|| {
                    ancestor_scope
                        .as_ref()
                        .map(|(org, env)| (org.as_str(), env.as_str()))
                });
            let (resolved_org, resolved_env) =
                resolve_check_scope(req, default_scope).map_err(AuthoringFailure::Io)?;
            validate_non_empty_scope("--org", &resolved_org)?;
            validate_non_empty_scope("--env", &resolved_env)?;
            let opts = CompileDirOptions {
                org: &resolved_org,
                env: &resolved_env,
                forbid_unsafe_zen: req.forbid_unsafe_zen,
                tool_root: req.tool_root,
                boundary_root: req.boundary_root,
                allow_empty: false,
            };
            let artifacts_json =
                compile_directory_pinned(&root_dir, manifest.as_ref(), &opts, budget)?;
            let app_root_dir = match app_root {
                Some(AppRootMatch::SameAsStart) => Some(root_dir),
                Some(AppRootMatch::Ancestor(dir)) => Some(dir),
                None => None,
            };
            Ok(CompiledCheckSource {
                artifacts_json,
                org: resolved_org,
                env: resolved_env,
                app_root: app_root_dir,
            })
        }
        PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        } => {
            let inherited_scope = if req.org.is_none() && req.env.is_none() {
                match app_root.as_ref() {
                    Some(matched) => read_norn_scope_from_dir(matched.as_dir(&parent_dir), budget)
                        .map_err(AuthoringFailure::Io)?,
                    None => None,
                }
            } else {
                None
            };
            let (resolved_org, resolved_env) = resolve_check_scope(
                req,
                inherited_scope
                    .as_ref()
                    .map(|(org, env)| (org.as_str(), env.as_str())),
            )
            .map_err(AuthoringFailure::Io)?;
            validate_non_empty_scope("--org", &resolved_org)?;
            validate_non_empty_scope("--env", &resolved_env)?;
            let opts = CompileDirOptions {
                org: &resolved_org,
                env: &resolved_env,
                forbid_unsafe_zen: req.forbid_unsafe_zen,
                tool_root: req.tool_root,
                boundary_root: req.boundary_root,
                allow_empty: false,
            };
            let artifacts_json = compile_single_file_pinned(
                &parent_dir,
                &file_name,
                &display_path,
                opened,
                &opts,
                budget,
            )?;
            let app_root_dir = match app_root {
                Some(AppRootMatch::SameAsStart) => Some(parent_dir),
                Some(AppRootMatch::Ancestor(dir)) => Some(dir),
                None => None,
            };
            Ok(CompiledCheckSource {
                artifacts_json,
                org: resolved_org,
                env: resolved_env,
                app_root: app_root_dir,
            })
        }
    }
}

/// Executes `norn check --source <PATH> [--org <ID> --env <ID>] [--context <FILE>] [--base <PATH>...]`.
pub(crate) fn execute_check_source(req: &CheckSourceRequest<'_>) -> CommandOutput {
    let target = match open_source_target_pinned(req.source) {
        Ok(pinned) => pinned,
        Err(err) => return CommandOutput::usage_or_io_error(&err),
    };
    let start_dir = match &target {
        PinnedSourceTarget::Directory(dir) => dir,
        PinnedSourceTarget::File { parent_dir, .. } => parent_dir,
    };
    let app_root = match find_norn_app_root_from_dir(start_dir, req.boundary_root) {
        Ok(matched) => matched,
        Err(err) => return CommandOutput::usage_or_io_error(&err),
    };
    if let Some(matched) = app_root.as_ref() {
        match check_zero_ci_app_root(matched.as_dir(start_dir)) {
            ZeroCiOutcome::Clean => {}
            ZeroCiOutcome::Refused(report) => return CommandOutput::refused(report),
            ZeroCiOutcome::IoError(err) => return CommandOutput::usage_or_io_error(&err),
        }
    }

    let mut budget = InvocationBudget::new();
    let manifest_base_packs = match app_root.as_ref() {
        Some(matched) => match base_packs::read_manifest_base_packs_from_dir(
            matched.as_dir(start_dir),
            &mut budget,
        ) {
            Ok(paths) => paths,
            Err(err) => return CommandOutput::usage_or_io_error(&err),
        },
        None => Vec::new(),
    };
    let compiled = match compile_check_source_pinned(target, app_root, req, &mut budget) {
        Ok(result) => result,
        Err(failure) => return failure.into_output(),
    };

    let raw_context = match req.context {
        Some(ctx_path) => match crate::cell::read_regular_file_bounded(
            ctx_path,
            MAX_CHECK_CONTEXT_BYTES,
            "16 MiB check input limit",
        ) {
            Ok(text) => text,
            Err(err) => return CommandOutput::usage_or_io_error(&err),
        },
        None => "{}".to_owned(),
    };

    let base_scope = base_packs::BasePackScope {
        org: Some(&compiled.org),
        env: Some(&compiled.env),
        forbid_unsafe_zen: req.forbid_unsafe_zen,
        tool_root: req.tool_root,
        boundary_root: req.boundary_root,
    };
    let context_json = match base_packs::merge_base_packs_into_context(
        &raw_context,
        compiled.app_root.as_ref(),
        &manifest_base_packs,
        req.base,
        &base_scope,
        &mut budget,
    ) {
        Ok(merged) => merged,
        Err(failure) => return failure.into_output(),
    };

    match crate::red_team::check_with_red_team(&compiled.artifacts_json, &context_json) {
        Ok((report, true)) => CommandOutput::ok(report),
        Ok((report, false)) => CommandOutput::refused(report),
        Err(err) => CommandOutput::usage_or_io_error(&err),
    }
}
