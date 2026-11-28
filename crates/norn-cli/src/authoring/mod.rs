//! Native Rust authoring subcommands (`compile`, `fmt`, `profile`, `question`, `dashboard`)
//! and in-process `check --source` wiring both `norn-authoring` (`TypeScript`) and
//! `norn-authoring-py` (Python) with zero Node/Bun/`tsc`/`CPython`.

mod atomic_io;
mod fmt;
mod fs_io;
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
use self::fs_io::{
    BoundedJsonArray, InvocationBudget, LoadedFile, SourceLang, classify_source_file,
    display_posix_path, is_declaration_file, is_json_file, read_opened_file_budgeted,
    validate_safe_relative,
};
use self::retained::validate_retained_definition;
use self::trusted_dir::{PinnedSourceTarget, TrustedDir, open_source_target_pinned};
use self::ts::{TsCompileOptions, TsCompileTarget};
use self::zero_ci::{
    AppRootMatch, ZeroCiOutcome, check_zero_ci_app_root, find_norn_app_root_from_dir,
};

const MAX_CHECK_CONTEXT_BYTES: u64 = 16 * 1024 * 1024;

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
    fn into_output(self) -> CommandOutput {
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
    #[arg(long)]
    pub(crate) org: String,
    /// Explicit environment scope (`scope.environmentId`).
    #[arg(long)]
    pub(crate) env: String,
    /// Promote `NORN-AUTHORING-UNSAFE-ZEN` warnings to hard errors.
    #[arg(long)]
    pub(crate) forbid_unsafe_zen: bool,
    /// Write compiled canonical JSON artifacts to this file instead of stdout.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect` and `node_modules/typescript`.
    #[arg(long)]
    pub(crate) tool_root: Option<PathBuf>,
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
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect` and `node_modules/typescript`.
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
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect` and `node_modules/typescript`.
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
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect` and `node_modules/typescript`.
    #[arg(long)]
    pub(crate) tool_root: Option<PathBuf>,
}

/// Inputs for `norn check --source`.
pub(crate) struct CheckSourceRequest<'a> {
    pub(crate) source: &'a Path,
    pub(crate) context: Option<&'a Path>,
    pub(crate) org: Option<&'a str>,
    pub(crate) env: Option<&'a str>,
    pub(crate) forbid_unsafe_zen: bool,
    pub(crate) tool_root: Option<&'a Path>,
}

struct NornAppManifest {
    sources: Vec<PathBuf>,
    org_id: Option<String>,
    environment_id: Option<String>,
}

enum JsonSourceEntry {
    Artifact(Value),
    RetainedDefinition,
}

struct CompileDirOptions<'a> {
    org: &'a str,
    env: &'a str,
    forbid_unsafe_zen: bool,
    tool_root: Option<&'a Path>,
    allow_empty: bool,
}

fn read_norn_manifest_from_dir(
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

fn compile_single_file_pinned(
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

fn compile_directory_pinned(
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
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    validate_non_empty_scope("--org", org)?;
    validate_non_empty_scope("--env", env)?;
    let opts = CompileDirOptions {
        org,
        env,
        forbid_unsafe_zen,
        tool_root,
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

/// Executes `norn compile`.
pub(crate) fn execute_compile(args: &CompileArgs) -> CommandOutput {
    let mut budget = InvocationBudget::new();
    match compile_path_to_json(
        &args.path,
        &args.org,
        &args.env,
        args.forbid_unsafe_zen,
        args.tool_root.as_deref(),
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

fn resolve_check_scope(
    req: &CheckSourceRequest<'_>,
    manifest: Option<&NornAppManifest>,
) -> Result<(String, String), String> {
    match (req.org, req.env) {
        (Some(org), Some(env)) => Ok((org.to_owned(), env.to_owned())),
        (Some(_), None) | (None, Some(_)) => Err(
            "--org and --env must be supplied together with --source (or omit both when checking a norn.json project)"
                .to_owned(),
        ),
        (None, None) => {
            if let Some(app) = manifest
                && let (Some(org_id), Some(env_id)) = (&app.org_id, &app.environment_id)
            {
                return Ok((org_id.clone(), env_id.clone()));
            }
            Err(
                "--org and --env are required with --source (or omit both when checking a norn.json project with scope)"
                    .to_owned(),
            )
        }
    }
}

fn compile_check_source_pinned(
    target: PinnedSourceTarget,
    app_root: Option<&AppRootMatch>,
    req: &CheckSourceRequest<'_>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    match target {
        PinnedSourceTarget::Directory(root_dir) => {
            let manifest = if matches!(app_root, Some(AppRootMatch::SameAsStart)) {
                read_norn_manifest_from_dir(&root_dir, budget).map_err(AuthoringFailure::Io)?
            } else {
                None
            };
            let (resolved_org, resolved_env) =
                resolve_check_scope(req, manifest.as_ref()).map_err(AuthoringFailure::Io)?;
            validate_non_empty_scope("--org", &resolved_org)?;
            validate_non_empty_scope("--env", &resolved_env)?;
            let opts = CompileDirOptions {
                org: &resolved_org,
                env: &resolved_env,
                forbid_unsafe_zen: req.forbid_unsafe_zen,
                tool_root: req.tool_root,
                allow_empty: false,
            };
            compile_directory_pinned(&root_dir, manifest.as_ref(), &opts, budget)
        }
        PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        } => {
            let (resolved_org, resolved_env) =
                resolve_check_scope(req, None).map_err(AuthoringFailure::Io)?;
            validate_non_empty_scope("--org", &resolved_org)?;
            validate_non_empty_scope("--env", &resolved_env)?;
            let opts = CompileDirOptions {
                org: &resolved_org,
                env: &resolved_env,
                forbid_unsafe_zen: req.forbid_unsafe_zen,
                tool_root: req.tool_root,
                allow_empty: false,
            };
            compile_single_file_pinned(
                &parent_dir,
                &file_name,
                &display_path,
                opened,
                &opts,
                budget,
            )
        }
    }
}

/// Executes `norn check --source <PATH> [--org <ID> --env <ID>] [--context <FILE>]`.
pub(crate) fn execute_check_source(req: &CheckSourceRequest<'_>) -> CommandOutput {
    let target = match open_source_target_pinned(req.source) {
        Ok(pinned) => pinned,
        Err(err) => return CommandOutput::usage_or_io_error(&err),
    };
    let start_dir = match &target {
        PinnedSourceTarget::Directory(dir) => dir,
        PinnedSourceTarget::File { parent_dir, .. } => parent_dir,
    };
    let app_root = match find_norn_app_root_from_dir(start_dir) {
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
    let artifacts_json =
        match compile_check_source_pinned(target, app_root.as_ref(), req, &mut budget) {
            Ok(json_text) => json_text,
            Err(failure) => return failure.into_output(),
        };

    let context_json = match req.context {
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

    let report = match norn_checker_wasm::check_artifact_set_json(&artifacts_json, &context_json) {
        Ok(text) => text,
        Err(err) => return CommandOutput::usage_or_io_error(&err.to_string()),
    };
    let parsed: Value = match serde_json::from_str(&report) {
        Ok(val) => val,
        Err(err) => return CommandOutput::usage_or_io_error(&err.to_string()),
    };
    if parsed.get("admitted").and_then(Value::as_bool) == Some(true) {
        CommandOutput::ok(report)
    } else {
        CommandOutput::refused(report)
    }
}
