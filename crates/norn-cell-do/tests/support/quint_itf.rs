//! Reading Quint ITF traces (verification/hosted-timers-v1/traces) for the
//! trace-driven conformance tests: the model's states are the oracle.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

pub(crate) fn normalize(v: Value) -> Value {
    match v {
        Value::Object(mut map) => {
            if let Some(n) = map.remove("#bigint") {
                return json!(n.as_str().expect("bigint").parse::<i64>().expect("bounded"));
            }
            if let Some(entries) = map.remove("#map") {
                return Value::Array(
                    entries
                        .as_array()
                        .expect("map entries")
                        .iter()
                        .map(|entry| normalize(entry.clone()))
                        .collect(),
                );
            }
            for tag in ["#set", "#list", "#tup"] {
                if let Some(items) = map.remove(tag) {
                    return normalize(items);
                }
            }
            Value::Object(map.into_iter().map(|(k, v)| (k, normalize(v))).collect())
        }
        Value::Array(xs) => Value::Array(xs.into_iter().map(normalize).collect()),
        other => other,
    }
}

/// The trace files of `model` (a model file stem), from the committed set
/// unless NORN_QUINT_TRACE_DIR names another.
pub(crate) fn traces(model: &str) -> Vec<(PathBuf, Vec<Value>)> {
    let root = std::env::var_os("NORN_QUINT_TRACE_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../verification/hosted-timers-v1/traces")
        },
        PathBuf::from,
    );
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("generate the traces first (verification/hosted-timers-v1/generate_traces.py)")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.file_name()
                .expect("name")
                .to_string_lossy()
                .starts_with(&format!("{model}-"))
                && path.extension().is_some_and(|ext| ext == "json")
        })
        .collect();
    paths.sort();
    assert!(paths.len() >= 8, "too few {model} traces in {}", root.display());
    paths
        .into_iter()
        .map(|path| {
            let raw: Value = serde_json::from_slice(&std::fs::read(&path).expect("bytes")).expect("ITF");
            let states = normalize(raw)["states"].as_array().expect("states").clone();
            assert!(states.len() > 1, "empty trace {}", path.display());
            (path, states)
        })
        .collect()
}

/// A model map variable as slot -> value.
pub(crate) fn map(state: &Value, var: &str) -> BTreeMap<i64, Value> {
    state[var]
        .as_array()
        .unwrap_or_else(|| panic!("map {var}"))
        .iter()
        .map(|pair| (pair[0].as_i64().expect("slot"), pair[1].clone()))
        .collect()
}

pub(crate) fn int(state: &Value, var: &str) -> i64 {
    state[var].as_i64().unwrap_or_else(|| panic!("int {var}"))
}

/// A model map variable keyed by strings.
pub(crate) fn str_map(value: &Value) -> BTreeMap<String, Value> {
    value
        .as_array()
        .expect("map")
        .iter()
        .map(|pair| (pair[0].as_str().expect("key").to_owned(), pair[1].clone()))
        .collect()
}
