//! Native Rust authoring subcommands (`compile`, `fmt`, `profile`, `question`, `dashboard`)
//! and in-process `check --source` wiring both `norn-authoring` (`TypeScript`) and
//! `norn-authoring-py` (Python) with zero Node/Bun/`tsc`/`CPython`.

mod fmt;
mod fs_io;
mod py;
#[cfg(test)]
mod tests;
mod ts;

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::{Value, json};

use self::fs_io::{
    EntryKind, SourceLang, classify_source_file, collect_top_level_files, inspect_path_no_follow,
    read_source_file_bounded, validate_safe_relative, write_file_atomic,
};

const MAX_CHECK_CONTEXT_BYTES: u64 = 16 * 1024 * 1024;
const ZERO_CI_DIAGNOSTIC_CODE: &str = "NORN-APP-ZERO-CI";

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

    pub(crate) fn usage_or_io_error(message: String) -> Self {
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
            Self::Io(message) => CommandOutput::usage_or_io_error(message),
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

fn read_norn_manifest(app_dir: &Path) -> Result<NornAppManifest, String> {
    let manifest_path = app_dir.join("norn.json");
    let raw = read_source_file_bounded(&manifest_path)?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("{}: invalid JSON: {err}", manifest_path.display()))?;
    let mut sources = Vec::new();
    if let Some(items) = parsed.get("sources").and_then(Value::as_array) {
        for item in items {
            let text = item.as_str().ok_or_else(|| {
                format!(
                    "{}: `sources` entries must be relative strings",
                    manifest_path.display()
                )
            })?;
            let rel = validate_safe_relative(text)?;
            sources.push(app_dir.join(rel));
        }
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
    Ok(NornAppManifest {
        sources,
        org_id,
        environment_id,
    })
}

fn find_norn_app_root(path: &Path) -> Option<PathBuf> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let start = if resolved.is_dir() {
        resolved.as_path()
    } else {
        resolved.parent().unwrap_or(resolved.as_path())
    };
    for ancestor in start.ancestors() {
        if ancestor.join("crates/norn-checker").is_dir() {
            return None;
        }
        if ancestor.join("norn.json").is_file() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn check_zero_ci_app_dir(path: &Path) -> Option<String> {
    let app_root = find_norn_app_root(path)?;
    let workflows_dir = app_root.join(".github/workflows");
    if std::fs::symlink_metadata(&workflows_dir).is_err() {
        return None;
    }
    let report = json!({
        "format": "norn-checker-abi/check@1",
        "admitted": false,
        "diagnostics": [{
            "code": ZERO_CI_DIAGNOSTIC_CODE,
            "severity": "error",
            "message": "Downstream Norn apps must not define `.github/workflows/` CI pipelines; Norn apps compile, format, and verify locally and in-browser via native Rust `norn` with zero CI.",
            "path": ".github/workflows",
        }],
        "receipt": null,
        "receiptSha256": null,
    })
    .to_string();
    Some(report)
}

fn validate_non_empty_scope(label: &str, value: &str) -> Result<(), AuthoringFailure> {
    if value.trim().is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{label} must be a non-empty scope identifier"
        )));
    }
    Ok(())
}

fn merge_artifact_arrays(batches: &[String]) -> Result<String, AuthoringFailure> {
    let mut merged = Vec::new();
    for batch in batches {
        let parsed: Value =
            serde_json::from_str(batch).map_err(|err| AuthoringFailure::Io(err.to_string()))?;
        let Some(items) = parsed.as_array() else {
            return Err(AuthoringFailure::Io(
                "compiled batch did not produce a JSON array".to_owned(),
            ));
        };
        merged.extend(items.iter().cloned());
    }
    norn_authoring::canonical_json(&Value::Array(merged))
        .map_err(|err| AuthoringFailure::Io(err.to_string()))
}

fn compile_directory_targets(
    dir: &Path,
    org: &str,
    env: &str,
    forbid_unsafe_zen: bool,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    let top_files = collect_top_level_files(dir).map_err(AuthoringFailure::Io)?;
    let mut typescript_files = Vec::new();
    let mut python_files = Vec::new();
    for file in top_files {
        match classify_source_file(&file) {
            Some(SourceLang::TypeScript) => typescript_files.push(file),
            Some(SourceLang::Python) => python_files.push(file),
            None => {}
        }
    }
    if typescript_files.is_empty() && python_files.is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{}: no .ts or .py authoring modules found",
            dir.display()
        )));
    }
    let mut batches = Vec::new();
    if !typescript_files.is_empty() {
        batches.push(ts::compile_ts_targets(
            Some(dir),
            &typescript_files,
            org,
            env,
            forbid_unsafe_zen,
            tool_root,
        )?);
    }
    if !python_files.is_empty() {
        batches.push(py::compile_python_files(&python_files, org, env)?);
    }
    if batches.len() == 1
        && let Some(only) = batches.pop()
    {
        return Ok(only);
    }
    merge_artifact_arrays(&batches)
}

fn compile_path_to_json(
    path: &Path,
    org: &str,
    env: &str,
    forbid_unsafe_zen: bool,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    validate_non_empty_scope("--org", org)?;
    validate_non_empty_scope("--env", env)?;
    match inspect_path_no_follow(path).map_err(AuthoringFailure::Io)? {
        EntryKind::File => match classify_source_file(path) {
            Some(SourceLang::Python) => py::compile_python_files(&[path.to_path_buf()], org, env),
            Some(SourceLang::TypeScript) => ts::compile_ts_targets(
                None,
                &[path.to_path_buf()],
                org,
                env,
                forbid_unsafe_zen,
                tool_root,
            ),
            None => Err(AuthoringFailure::Io(format!(
                "{}: expected a .ts or .py authoring source file",
                path.display()
            ))),
        },
        EntryKind::Directory => {
            if path.join("norn.json").is_file() {
                let manifest = read_norn_manifest(path).map_err(AuthoringFailure::Io)?;
                let mut batches = Vec::new();
                for source_dir in &manifest.sources {
                    if inspect_path_no_follow(source_dir).map_err(AuthoringFailure::Io)?
                        == EntryKind::Directory
                    {
                        batches.push(compile_directory_targets(
                            source_dir,
                            org,
                            env,
                            forbid_unsafe_zen,
                            tool_root,
                        )?);
                    }
                }
                if batches.is_empty() {
                    return Err(AuthoringFailure::Io(format!(
                        "{}: norn.json declares no authoring source directories",
                        path.display()
                    )));
                }
                if batches.len() == 1
                    && let Some(only) = batches.pop()
                {
                    return Ok(only);
                }
                return merge_artifact_arrays(&batches);
            }
            compile_directory_targets(path, org, env, forbid_unsafe_zen, tool_root)
        }
    }
}

/// Executes `norn compile`.
pub(crate) fn execute_compile(args: &CompileArgs) -> CommandOutput {
    match compile_path_to_json(
        &args.path,
        &args.org,
        &args.env,
        args.forbid_unsafe_zen,
        args.tool_root.as_deref(),
    ) {
        Ok(canonical) => {
            if let Some(out_path) = &args.out {
                match write_file_atomic(out_path, &canonical) {
                    Ok(()) => CommandOutput::ok(String::new()),
                    Err(err) => CommandOutput::usage_or_io_error(err),
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

fn resolve_check_scope(req: &CheckSourceRequest<'_>) -> Result<(String, String), String> {
    if let (Some(org), Some(env)) = (req.org, req.env) {
        return Ok((org.to_owned(), env.to_owned()));
    }
    if req.source.is_dir() && req.source.join("norn.json").is_file() {
        let manifest = read_norn_manifest(req.source)?;
        let org = req.org.map(str::to_owned).or(manifest.org_id);
        let env = req.env.map(str::to_owned).or(manifest.environment_id);
        if let (Some(org_id), Some(env_id)) = (org, env) {
            return Ok((org_id, env_id));
        }
    }
    Err("--org and --env are required with --source".to_owned())
}

/// Executes `norn check --source <PATH> --org <ID> --env <ID> [--context <FILE>]`.
pub(crate) fn execute_check_source(req: &CheckSourceRequest<'_>) -> CommandOutput {
    if let Some(refused_report) = check_zero_ci_app_dir(req.source) {
        return CommandOutput::refused(refused_report);
    }

    let (resolved_org, resolved_env) = match resolve_check_scope(req) {
        Ok(pair) => pair,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };

    let artifacts_json = match compile_path_to_json(
        req.source,
        &resolved_org,
        &resolved_env,
        req.forbid_unsafe_zen,
        req.tool_root,
    ) {
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
            Err(err) => return CommandOutput::usage_or_io_error(err),
        },
        None => "{}".to_owned(),
    };

    let report = match norn_checker_wasm::check_artifact_set_json(&artifacts_json, &context_json) {
        Ok(text) => text,
        Err(err) => return CommandOutput::usage_or_io_error(err.to_string()),
    };
    let parsed: Value = match serde_json::from_str(&report) {
        Ok(val) => val,
        Err(err) => return CommandOutput::usage_or_io_error(err.to_string()),
    };
    if parsed.get("admitted").and_then(Value::as_bool) == Some(true) {
        CommandOutput::ok(report)
    } else {
        CommandOutput::refused(report)
    }
}
