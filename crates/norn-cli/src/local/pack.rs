//! Pack loading for local cell runtimes (`norn test` and `norn dev`).
//!
//! Loads compiled JSON artifact files/directories (bounded, no symlinks) or
//! compiles `.ts`/`.py` authoring sources in-process via `norn-authoring` /
//! `norn-authoring-py`, merging optional repeatable `--base` packs cleanly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use serde_json::value::RawValue;

use crate::authoring::{AuthoringFailure, compile_source_artifacts};
use crate::cell::{read_file_or_json_dir_bounded, read_regular_file_bounded};

const MAX_PACK_BYTES: u64 = 16 * 1024 * 1024;

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

/// Load a JSON artifact file (array or single envelope) or directory of `*.json`
/// envelopes as exact [`RawValue`]s preserving numeric tokens.
pub(crate) fn load_json_artifacts(path: &Path) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    let raw = read_file_or_json_dir_bounded(path, MAX_PACK_BYTES, "16 MiB pack input limit")
        .map_err(PackLoadError::Io)?;
    parse_raw_artifact_list(&raw, path)
}

/// Compile a `.ts`/`.py` source file, directory, or `norn.json` project into exact [`RawValue`]s.
pub(crate) fn load_source_artifacts(
    path: &Path,
    org: &str,
    env: &str,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    let compiled = compile_source_artifacts(path, org, env)?;
    parse_raw_artifact_list(&compiled, path)
}

/// Auto-detect whether a `--base` path is authoring sources (`.ts`/`.py`/`norn.json`)
/// or JSON artifacts, and load it accordingly.
pub(crate) fn load_auto_pack(
    path: &Path,
    org: &str,
    env: &str,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    if is_authoring_target(path)? {
        load_source_artifacts(path, org, env)
    } else {
        load_json_artifacts(path)
    }
}

/// Load all repeatable `--base` packs followed by the optional app pack,
/// deduplicating by `artifactId` across packs (later pack wins) while keeping
/// any intra-pack duplicates intact for the real admission gate to diagnose.
pub(crate) fn load_combined_packs(
    base_paths: &[PathBuf],
    app_pack: Option<&PackInput>,
    org: &str,
    env: &str,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    let mut layers: Vec<Vec<Box<RawValue>>> = Vec::new();
    for base_path in base_paths {
        layers.push(load_auto_pack(base_path, org, env)?);
    }
    if let Some(app) = app_pack {
        let artifacts = match app {
            PackInput::Source(path) => load_source_artifacts(path, org, env)?,
            PackInput::Artifacts(path) => load_json_artifacts(path)?,
        };
        layers.push(artifacts);
    }
    merge_pack_layers(layers)
}

/// Load one step/request pack from either a `source` or `artifacts` path.
pub(crate) fn load_pack_input(
    input: &PackInput,
    org: &str,
    env: &str,
) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    match input {
        PackInput::Source(path) => load_source_artifacts(path, org, env),
        PackInput::Artifacts(path) => load_json_artifacts(path),
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

/// Read a single bounded regular JSON file (refusing symlinks) and parse as [`Value`].
pub(crate) fn read_json_file(path: &Path, label: &str) -> Result<Value, String> {
    let text = read_regular_file_bounded(path, MAX_PACK_BYTES, label)?;
    serde_json::from_str(&text).map_err(|err| format!("{}: invalid JSON: {err}", path.display()))
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
    serde_json::from_str::<Value>(raw.get())
        .ok()
        .and_then(|value| {
            value
                .get("artifactId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

fn merge_pack_layers(layers: Vec<Vec<Box<RawValue>>>) -> Result<Vec<Box<RawValue>>, PackLoadError> {
    // Track which later layer overrides a given artifactId so cross-layer
    // overrides replace cleanly while intra-layer duplicates remain untouched.
    let mut last_layer_by_id: BTreeMap<String, usize> = BTreeMap::new();
    for (layer_idx, layer) in layers.iter().enumerate() {
        for raw in layer {
            if let Some(id) = artifact_id_of(raw) {
                last_layer_by_id.insert(id, layer_idx);
            }
        }
    }
    let mut merged = Vec::new();
    for (layer_idx, layer) in layers.into_iter().enumerate() {
        for raw in layer {
            if let Some(id) = artifact_id_of(&raw)
                && last_layer_by_id.get(&id).copied() != Some(layer_idx)
            {
                continue;
            }
            merged.push(raw);
        }
    }
    Ok(merged)
}
