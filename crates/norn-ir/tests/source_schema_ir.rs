#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! `SourceSchemaIR` (T8-SRC-1, #2596): the published JSON Schema is checked
//! in and current, and the SQL, workbook, and Frappe fixtures decode and
//! round-trip byte-for-byte as JSON values. Malformed documents
//! (`tests/fixtures/source_schema/invalid/invalid_document.*`) fail to decode;
//! the other invalid fixtures decode and are `norn-checker` findings.

use std::{fs, path::Path};

use norn_ir::source_schema::{SourceSchemaIr, source_schema_ir_schema};

const UPDATE_ENV: &str = "NORN_IR_UPDATE_PAYLOAD_SCHEMAS";

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/source_schema")
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be JSON: {error}", path.display()))
}

#[test]
fn source_schema_ir_schema_is_checked_in_and_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/source_schema_ir.schema.json");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&source_schema_ir_schema()).expect("schema should serialize")
    );
    if std::env::var_os(UPDATE_ENV).is_some() {
        fs::write(&path, &rendered).expect("schema should write");
    }
    let checked_in: Option<serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let current: serde_json::Value =
        serde_json::from_str(&rendered).expect("rendered schema should parse");
    assert!(
        checked_in.as_ref() == Some(&current),
        "{} is stale; regenerate with {UPDATE_ENV}=1 cargo test -p norn-ir --test source_schema_ir\n\
         ---BEGIN source_schema_ir.schema.json---\n{rendered}---END source_schema_ir.schema.json---",
        path.display()
    );
}

#[test]
fn fixtures_decode_and_round_trip() {
    for name in ["sql", "workbook", "frappe"] {
        let path = fixture_dir().join(format!("{name}.json"));
        let wire = read_json(&path);
        let document: SourceSchemaIr = serde_json::from_value(wire.clone())
            .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
        let encoded = serde_json::to_value(&document).expect("document should encode");
        assert_eq!(encoded, wire, "{name} should round-trip");
        assert!(!document.datasets.is_empty(), "{name} declares datasets");
    }
}

#[test]
fn malformed_documents_fail_to_decode() {
    let invalid = fixture_dir().join("invalid");
    let mut seen = 0_u32;
    for entry in fs::read_dir(&invalid).expect("invalid fixtures should be listable") {
        let path = entry.expect("entry should be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture names are UTF-8")
            .to_owned();
        let decoded = serde_json::from_value::<SourceSchemaIr>(read_json(&path));
        if name.starts_with("invalid_document.") {
            assert!(decoded.is_err(), "{name} should fail to decode");
            seen = seen.saturating_add(1);
        } else {
            assert!(
                decoded.is_ok(),
                "{name} should decode (its finding is the checker's): {decoded:?}"
            );
        }
    }
    assert!(seen >= 3, "every malformed-document fixture was exercised");
}
