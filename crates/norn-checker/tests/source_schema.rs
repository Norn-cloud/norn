#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! `SourceSchemaIR` checks (T8-SRC-1, #2596) over the shared fixtures in
//! `crates/norn-ir/tests/fixtures/source_schema/`: the SQL, workbook, and
//! Frappe documents check clean, and each invalid fixture reports the code
//! its file name starts with (`<detail>.<case>.json` →
//! `source_schema.validation.<detail>`).

use std::{fs, path::Path};

use norn_checker::source_schema::decode_source_schema;

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../norn-ir/tests/fixtures/source_schema")
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be JSON: {error}", path.display()))
}

#[test]
fn sql_workbook_and_frappe_fixtures_check_clean() {
    for name in ["sql", "workbook", "frappe"] {
        let value = read_json(&fixture_dir().join(format!("{name}.json")));
        if let Err(findings) = decode_source_schema(&value) {
            panic!("{name} should check clean: {findings:#?}");
        }
    }
}

#[test]
fn source_identity_components_reject_every_control_character() {
    let fixture = read_json(&fixture_dir().join("frappe.json"));
    for member in ["system", "instance"] {
        for byte in (0_u8..=31).chain(127..=159) {
            let mut value = fixture.clone();
            *value
                .get_mut("source")
                .and_then(|source| source.get_mut(member))
                .expect("source identity component") =
                serde_json::json!(format!("prefix{}suffix", char::from(byte)));
            let findings = decode_source_schema(&value)
                .expect_err("controls refuse before a source namespace is admitted");
            assert!(
                findings.iter().any(|finding| {
                    finding.code == "source_schema.validation.invalid_source_identity"
                        && finding.registration().is_some()
                }),
                "{member} U+{byte:04X}: {findings:#?}"
            );
        }
    }
}

#[test]
fn invalid_fixtures_report_their_code() {
    let mut cases = 0_u32;
    for entry in fs::read_dir(fixture_dir().join("invalid")).expect("fixtures should be listable") {
        let path = entry.expect("entry should be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture names are UTF-8")
            .to_owned();
        let detail = name.split('.').next().expect("a name has a first segment");
        let expected = format!("source_schema.validation.{detail}");
        let findings = decode_source_schema(&read_json(&path))
            .expect_err("an invalid fixture must not check clean");
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == expected.as_str()),
            "{name} should report {expected}: {findings:#?}"
        );
        assert!(
            findings
                .iter()
                .all(|finding| finding.registration().is_some()),
            "{name}: every code is registered"
        );
        cases = cases.saturating_add(1);
    }
    assert!(cases >= 20, "every invalid fixture was exercised");
}
