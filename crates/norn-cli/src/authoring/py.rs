//! Native Rust Python authoring compilation (`norn-authoring-py`, zero `CPython`).
//!
//! All Python modules are read through trusted FD-pinned descriptors before reaching
//! [`compile_python_files`], which elaborates in-memory sources via `elaborate_source`
//! and streams each artifact directly through [`BoundedJsonWriter`] without ever
//! reopening pathnames or accumulating unbounded serialized JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use norn_authoring_py::{CODE_IO, EnvelopeOptions, JsonValue, PyAuthoringError, elaborate_source};
use serde_json::json;

use super::AuthoringFailure;
use super::fs_io::{
    BoundedJsonArray, BoundedJsonWriter, InvocationBudget, LoadedFile, display_posix_path,
    write_json_string,
};

fn format_py_error_json(error: &PyAuthoringError) -> String {
    let diag = match error.span() {
        Some(span) => json!({
            "code": error.code(),
            "message": error.message(),
            "file": span.file,
            "line": span.line,
            "column": span.column,
        }),
        None => json!({
            "code": error.code(),
            "message": error.message(),
        }),
    };
    json!({
        "ok": false,
        "diagnostics": [diag],
    })
    .to_string()
}

fn artifact_id_of(artifact: &JsonValue) -> Option<&str> {
    let JsonValue::Object(map) = artifact else {
        return None;
    };
    let JsonValue::String(id) = map.get("artifactId")? else {
        return None;
    };
    Some(id.as_str())
}

fn write_py_json_value(
    value: &JsonValue,
    writer: &mut BoundedJsonWriter<'_>,
) -> Result<(), String> {
    match value {
        JsonValue::Null => writer.write_all(b"null").map_err(|err| err.to_string()),
        JsonValue::Bool(true) => writer.write_all(b"true").map_err(|err| err.to_string()),
        JsonValue::Bool(false) => writer.write_all(b"false").map_err(|err| err.to_string()),
        JsonValue::Number(raw) => writer
            .write_all(raw.as_bytes())
            .map_err(|err| err.to_string()),
        JsonValue::String(text) => write_json_string(writer, text),
        JsonValue::Array(items) => {
            writer.write_all(b"[").map_err(|err| err.to_string())?;
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    writer.write_all(b",").map_err(|err| err.to_string())?;
                }
                write_py_json_value(item, writer)?;
            }
            writer.write_all(b"]").map_err(|err| err.to_string())
        }
        JsonValue::Object(map) => {
            let mut entries: Vec<(&str, &JsonValue)> =
                map.iter().map(|(key, val)| (key.as_str(), val)).collect();
            entries.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
            writer.write_all(b"{").map_err(|err| err.to_string())?;
            for (idx, (key, val)) in entries.into_iter().enumerate() {
                if idx > 0 {
                    writer.write_all(b",").map_err(|err| err.to_string())?;
                }
                write_json_string(writer, key)?;
                writer.write_all(b":").map_err(|err| err.to_string())?;
                write_py_json_value(val, writer)?;
            }
            writer.write_all(b"}").map_err(|err| err.to_string())
        }
    }
}

/// Elaborates pre-loaded Python authoring sources in-process, streaming each artifact
/// incrementally into `out` under `budget`.
pub(crate) fn compile_python_files_into(
    files: &[LoadedFile],
    org: &str,
    env: &str,
    seen_ids: &mut BTreeSet<String>,
    out: &mut BoundedJsonArray,
    budget: &mut InvocationBudget,
) -> Result<(), AuthoringFailure> {
    let mut scope = BTreeMap::new();
    scope.insert("environmentId".to_owned(), env.to_owned());
    scope.insert("orgId".to_owned(), org.to_owned());

    for file in files {
        let source_path = display_posix_path(&file.display_path);
        let options = EnvelopeOptions::new(scope.clone(), source_path.clone());
        let outcome = elaborate_source(&file.content, &options).map_err(|err| {
            if err.code() == CODE_IO {
                AuthoringFailure::Io(err.to_string())
            } else {
                AuthoringFailure::Diagnostics(format_py_error_json(&err))
            }
        })?;
        for artifact in outcome.artifacts {
            if let Some(id) = artifact_id_of(&artifact)
                && !seen_ids.insert(id.to_owned())
            {
                let refused = json!({
                    "ok": false,
                    "diagnostics": [{
                        "code": "NORN-PY-SCHEMA",
                        "message": format!("duplicate artifactId `{id}` declared in `{source_path}`"),
                        "file": source_path,
                    }],
                })
                .to_string();
                return Err(AuthoringFailure::Diagnostics(refused));
            }
            out.push_prefix("python compile output", budget)
                .map_err(AuthoringFailure::Io)?;
            {
                let mut writer = out.writer("python compile output", budget);
                write_py_json_value(&artifact, &mut writer).map_err(AuthoringFailure::Io)?;
            }
            out.mark_pushed();
        }
    }
    Ok(())
}

/// Elaborates pre-loaded Python authoring sources in-process into a bounded JSON artifact array.
pub(crate) fn compile_python_files(
    files: &[LoadedFile],
    org: &str,
    env: &str,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    let mut seen_ids = BTreeSet::new();
    let mut out = BoundedJsonArray::new();
    compile_python_files_into(files, org, env, &mut seen_ids, &mut out, budget)?;
    out.finish("python compile output", budget)
        .map_err(AuthoringFailure::Io)
}
