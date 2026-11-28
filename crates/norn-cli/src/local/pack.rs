//! Pack loading for local cell runtimes (`norn test` and `norn dev`).
//!
//! Loads compiled JSON artifact files/directories (bounded, no symlinks) or
//! compiles `.ts`/`.py` authoring sources in-process via `norn-authoring` /
//! `norn-authoring-py`, keeping `--base` layers strictly separate from tenant
//! app artifacts and enforcing one aggregate byte budget across all packs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_ir::RuntimeValue;
use serde_json::value::RawValue;

use crate::authoring::{AuthoringFailure, compile_source_artifacts};
use crate::cell::{read_file_or_json_dir_bounded, read_regular_file_bounded};

pub(super) const MAX_PACK_BYTES: u64 = 16 * 1024 * 1024;
const AGGREGATE_BUDGET_LABEL: &str = "16 MiB aggregate pack input limit";

/// Failure classifying pack loading errors between compiler diagnostics (exit `1`)
/// and usage/IO failures (exit `2`).
pub(crate) enum PackLoadError {
    Diagnostics(String),
    Io(String),
}

impl From<AuthoringFailure> for PackLoadError {
    fn from(failure: AuthoringFailure) -> Self {
        match failure {
            AuthoringFailure::Diagnostics(doc) => Self::Diagnostics(doc),
            AuthoringFailure::Io(msg) => Self::Io(msg),
        }
    }
}

/// Selection of an app pack input: compiled authoring sources or JSON artifacts.
#[derive(Debug, Clone)]
pub(crate) enum PackInput {
    Source(PathBuf),
    Artifacts(PathBuf),
}

/// Separate loaded `--base` and tenant app artifact layers.
///
/// Keeping both layers distinct ensures tenant app packs always flow through
/// `apply_definitions` and can never enter trusted base provisioning or spoof
/// reserved `core.*`/`std.*` namespaces.
#[derive(Debug, Default)]
pub(crate) struct LoadedPacks {
    pub(crate) base_artifacts: Vec<Box<RawValue>>,
    pub(crate) app_artifacts: Vec<Box<RawValue>>,
}

fn charge_budget(budget: &mut u64, bytes: usize, origin: &Path) -> Result<(), PackLoadError> {
    let used = u64::try_from(bytes)
        .map_err(|err| PackLoadError::Io(format!("{}: {err}", origin.display())))?;
    *budget = budget.checked_sub(used).ok_or_else(|| {
        PackLoadError::Io(format!(
            "{}: combined packs exceed {AGGREGATE_BUDGET_LABEL}",
            origin.display()
        ))
    })?;
    Ok(())
}

fn load_json_artifacts_budgeted(
    path: &Path,
    budget: &mut u64,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    let raw = read_file_or_json_dir_bounded(path, *budget, AGGREGATE_BUDGET_LABEL)
        .map_err(PackLoadError::Io)?;
    charge_budget(budget, raw.len(), path)?;
    parse_raw_artifact_list(&raw, path)
}

fn load_source_artifacts_budgeted(
    path: &Path,
    org: &str,
    env: &str,
    budget: &mut u64,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    if *budget == 0 {
        return Err(PackLoadError::Io(format!(
            "{}: combined packs exceed {AGGREGATE_BUDGET_LABEL}",
            path.display()
        )));
    }
    let compiled = compile_source_artifacts(path, org, env)?;
    charge_budget(budget, compiled.len(), path)?;
    parse_raw_artifact_list(&compiled, path)
}

fn load_auto_pack_budgeted(
    path: &Path,
    org: &str,
    env: &str,
    budget: &mut u64,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    if is_authoring_target(path)? {
        load_source_artifacts_budgeted(path, org, env, budget)
    } else {
        load_json_artifacts_budgeted(path, budget)
    }
}

/// Load all repeatable `--base` packs and the optional app pack under one shared
/// 16 MiB aggregate byte budget, keeping base and app layers strictly separate
/// and refusing duplicate `artifactId`s in `--base` packs fail-closed.
pub(crate) fn load_packs(
    base_paths: &[PathBuf],
    app_pack: Option<&PackInput>,
    org: &str,
    env: &str,
) -> Result<LoadedPacks, PackLoadError> {
    load_packs_with_budget(base_paths, app_pack, org, env, MAX_PACK_BYTES)
}

/// Load repeatable `--base` packs and the optional app pack under an explicit
/// aggregate byte budget `total_budget`.
pub(super) fn load_packs_with_budget(
    base_paths: &[PathBuf],
    app_pack: Option<&PackInput>,
    org: &str,
    env: &str,
    total_budget: u64,
) -> Result<LoadedPacks, PackLoadError> {
    let mut budget = total_budget;
    let mut base_artifacts = Vec::new();
    let mut seen_base_ids = BTreeSet::new();
    for base_path in base_paths {
        let loaded = load_auto_pack_budgeted(base_path, org, env, &mut budget)?;
        for raw in loaded {
            let Some(id) = artifact_id_of(&raw) else {
                return Err(PackLoadError::Io(format!(
                    "{}: base artifact envelope must declare string artifactId",
                    base_path.display()
                )));
            };
            if !seen_base_ids.insert(id.clone()) {
                return Err(PackLoadError::Io(format!(
                    "{}: duplicate base artifactId `{id}`",
                    base_path.display()
                )));
            }
            base_artifacts.push(raw);
        }
    }
    let app_artifacts = match app_pack {
        None => Vec::new(),
        Some(PackInput::Source(path)) => {
            load_source_artifacts_budgeted(path, org, env, &mut budget)?
        }
        Some(PackInput::Artifacts(path)) => load_json_artifacts_budgeted(path, &mut budget)?,
    };
    Ok(LoadedPacks {
        base_artifacts,
        app_artifacts,
    })
}

/// Load one step/request pack from either a `source` or `artifacts` path.
pub(crate) fn load_pack_input(
    input: &PackInput,
    org: &str,
    env: &str,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    let mut budget = MAX_PACK_BYTES;
    match input {
        PackInput::Source(path) => load_source_artifacts_budgeted(path, org, env, &mut budget),
        PackInput::Artifacts(path) => load_json_artifacts_budgeted(path, &mut budget),
    }
}

/// Build the exact JSON body for `POST /v1/cell/definitions:plan` or `:apply`
/// without decoding or rounding numeric tokens.
pub(crate) fn encode_definitions_change_body(
    artifacts: &[Box<RawValue>],
    expected_version: Option<u64>,
) -> String {
    let items: Vec<&str> = artifacts.iter().map(|raw| raw.get()).collect();
    match expected_version {
        Some(version) => format!(
            r#"{{"artifacts":[{}],"expectedVersion":{version}}}"#,
            items.join(",")
        ),
        None => format!(r#"{{"artifacts":[{}]}}"#, items.join(",")),
    }
}

/// Read a single bounded regular JSON file (refusing symlinks) as an exact
/// [`RuntimeValue`], preserving full decimal precision without `f64` rounding.
pub(crate) fn read_runtime_json_file(path: &Path, label: &str) -> Result<RuntimeValue, String> {
    let text = read_regular_file_bounded(path, MAX_PACK_BYTES, label)?;
    RuntimeValue::from_json_text(&text)
        .map_err(|err| format!("{}: invalid JSON: {err}", path.display()))
}

fn parse_raw_artifact_list(raw: &str, origin: &Path) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<Box<RawValue>>>(trimmed).map_err(|err| {
            PackLoadError::Io(format!(
                "{}: invalid artifact array: {err}",
                origin.display()
            ))
        })
    } else if trimmed.starts_with('{') {
        let single: Box<RawValue> = serde_json::from_str(trimmed).map_err(|err| {
            PackLoadError::Io(format!(
                "{}: invalid artifact object: {err}",
                origin.display()
            ))
        })?;
        Ok(vec![single])
    } else {
        Err(PackLoadError::Io(format!(
            "{}: expected JSON object or array of artifact envelopes",
            origin.display()
        )))
    }
}

fn is_authoring_target(path: &Path) -> Result<bool, PackLoadError> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|err| PackLoadError::Io(format!("{}: {err}", path.display())))?;
    if meta.file_type().is_symlink() {
        return Err(PackLoadError::Io(format!(
            "{}: links are refused",
            path.display()
        )));
    }
    if meta.is_file() {
        let ext = path.extension().and_then(std::ffi::OsStr::to_str);
        return Ok(matches!(ext, Some("ts" | "tsx" | "py")));
    }
    if !meta.is_dir() {
        return Err(PackLoadError::Io(format!(
            "{}: not a regular file or directory",
            path.display()
        )));
    }
    let entries = std::fs::read_dir(path)
        .map_err(|err| PackLoadError::Io(format!("{}: {err}", path.display())))?;
    for entry in entries {
        let entry = entry.map_err(|err| PackLoadError::Io(format!("{}: {err}", path.display())))?;
        let name = entry.file_name();
        if name == "norn.json" {
            return Ok(true);
        }
        let ext = Path::new(&name)
            .extension()
            .and_then(std::ffi::OsStr::to_str);
        if matches!(ext, Some("ts" | "tsx" | "py")) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn artifact_id_of(raw: &RawValue) -> Option<String> {
    RuntimeValue::from_json_text(raw.get())
        .ok()
        .and_then(|value| {
            value
                .get("artifactId")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
        })
}
