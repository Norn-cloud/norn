//! Native Rust Python authoring compilation (`norn-authoring-py`, zero `CPython`).
//!
//! All Python modules are read through trusted FD-pinned descriptors before reaching
//! [`compile_python_files`], which elaborates in-memory sources via `elaborate_source`
//! without ever reopening pathnames.

use std::collections::{BTreeMap, BTreeSet};

use norn_authoring_py::{
    CODE_IO, EnvelopeOptions, JsonValue, PyAuthoringError, canonical_json, elaborate_source,
};
use serde_json::json;

use super::AuthoringFailure;
use super::fs_io::{InvocationBudget, LoadedFile, display_posix_path};

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

/// Elaborates pre-loaded Python authoring sources in-process into a canonical JSON artifact array.
pub(crate) fn compile_python_files(
    files: &[LoadedFile],
    org: &str,
    env: &str,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    let mut scope = BTreeMap::new();
    scope.insert("environmentId".to_owned(), env.to_owned());
    scope.insert("orgId".to_owned(), org.to_owned());

    let mut all_artifacts: Vec<JsonValue> = Vec::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();

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
            all_artifacts.push(artifact);
        }
    }

    let rendered = canonical_json(&JsonValue::Array(all_artifacts))
        .map_err(|err| AuthoringFailure::Diagnostics(format_py_error_json(&err)))?;
    budget
        .charge_output_bytes("python compile output", rendered.len())
        .map_err(AuthoringFailure::Io)?;
    Ok(rendered)
}
