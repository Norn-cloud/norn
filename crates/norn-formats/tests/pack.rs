#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    missing_docs,
    reason = "integration tests"
)]
//! A pack's hand-written source contract may claim no more than the pinned
//! originals: each field must exist upstream with the reader's candidate type,
//! and may be required only where upstream requires it.

use norn_formats::SchemaImporter;
use norn_formats::pack::{Flatten, ProjectionSpec, check_contract, project};
use norn_formats::xsd::{XsdConfig, XsdFile, XsdReader, repeating_fields};
use norn_ir::OntologySource;
use serde_json::{Value, json};

const PATH: &str = "https://example.test/synthetic-invoice/1.0/invoice-shaped.xsd";
const XSD: &[u8] = include_bytes!("fixtures/synthetic/invoice-shaped.xsd");
const DOC: &str = "{urn:test:invoice}InvoiceDocumentType";

fn schema() -> norn_ir::source_schema::SourceSchemaIr {
    let mut admission =
        serde_json::from_str::<OntologySource>(include_str!("fixtures/synthetic/source.json"))
            .unwrap();
    admission.files = Some(vec![norn_ir::OntologySourceFile {
        path: PATH.into(),
        sha256: norn_formats::contract::sha256_hex(XSD),
    }]);
    let config = XsdConfig {
        admission,
        observed_at: "2026-11-11T00:00:00Z".into(),
    };
    let files = [XsdFile {
        path: PATH,
        bytes: XSD,
    }];
    XsdReader.import(&files, &config).source_schema.unwrap()
}

fn flat(path: &[&str], name: &str) -> Flatten {
    Flatten {
        dataset: DOC.into(),
        path: path.iter().map(|s| (*s).to_owned()).collect(),
        name: name.into(),
    }
}

fn spec(flatten: Vec<Flatten>) -> ProjectionSpec {
    ProjectionSpec {
        contract: "unused".into(),
        select: vec![DOC.into(), "{urn:test:invoice}LineType".into()],
        flatten,
    }
}

fn projection() -> Value {
    let schema = schema();
    let spec = spec(vec![
        flat(&["Supplier", "Endpoint"], "supplier_endpoint"),
        flat(&["Totals", "Payable"], "payable"),
    ]);
    project(&schema, &spec, &repeating_fields(&schema)).unwrap()
}

/// A faithful contract: four of the document's fields, as a mapping reads them.
fn contract(projection: &Value) -> Value {
    let field = |name: &str, native: &str, kind: &str, required: bool| {
        json!({"dataset": "InvoiceDocumentType", "name": name, "nativeType": native,
               "typeCandidate": {"type": kind, "origin": "inferred"}, "required": required})
    };
    json!({
        "rawMetadata": projection["rawMetadata"],
        "datasets": [{"id": "InvoiceDocumentType", "nativeName": DOC, "kind": "document"}],
        "fields": [
            field("ID", "{urn:test:invoice}IdentifierType", "string", true),
            field("IssueDate", "{http://www.w3.org/2001/XMLSchema}date", "date", true),
            field("DueDate", "{http://www.w3.org/2001/XMLSchema}date", "date", false),
            field("payable", "{urn:test:invoice}AmountType", "json", true),
            field("supplier_endpoint", "{urn:test:invoice}IdentifierType", "string", false),
        ],
        "constraints": [{"id": "lines", "kind": "parent_child", "dataset": "LineType",
            "fields": [], "target": {"dataset": "InvoiceDocumentType", "fields": []},
            "collection": "Line", "origin": "declared"}],
    })
}

fn mutated(edit: impl FnOnce(&mut Value)) -> Vec<String> {
    let projection = projection();
    let mut contract = contract(&projection);
    edit(&mut contract);
    check_contract(&contract, &projection)
}

#[test]
fn a_faithful_contract_is_a_subset_of_the_originals() {
    let projection = projection();
    assert_eq!(
        check_contract(&contract(&projection), &projection),
        Vec::<String>::new()
    );
}

#[test]
fn a_field_missing_upstream_fails() {
    let problems = mutated(|c| {
        c["fields"].as_array_mut().unwrap().push(json!({"dataset": "InvoiceDocumentType",
            "name": "Discount", "nativeType": "x", "typeCandidate": {"type": "string", "origin": "inferred"}}));
    });
    assert!(
        problems
            .iter()
            .any(|p| p.contains("Discount does not exist upstream")),
        "{problems:?}"
    );
}

#[test]
fn a_wrong_type_fails() {
    let problems = mutated(|c| c["fields"][1]["typeCandidate"]["type"] = json!("string"));
    assert!(
        problems.iter().any(|p| p.contains("IssueDate claims type")),
        "{problems:?}"
    );
}

#[test]
fn required_but_optional_upstream_fails() {
    let problems = mutated(|c| c["fields"][2]["required"] = json!(true));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("DueDate is required but optional upstream")),
        "{problems:?}"
    );
}

#[test]
fn a_contract_for_other_originals_fails() {
    let problems = mutated(|c| c["rawMetadata"]["sha256"] = json!("0".repeat(64)));
    assert!(
        problems.iter().any(|p| p.contains("rawMetadata")),
        "{problems:?}"
    );
}

#[test]
fn a_different_native_type_fails() {
    let problems = mutated(|c| c["fields"][0]["nativeType"] = json!("{urn:test:invoice}Other"));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("ID has a different native type")),
        "{problems:?}"
    );
}

#[test]
fn a_dataset_naming_another_upstream_type_fails() {
    let problems =
        mutated(|c| c["datasets"][0]["nativeName"] = json!("{urn:test:invoice}LineType"));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("names a different upstream type")),
        "{problems:?}"
    );
}

#[test]
fn a_nesting_missing_upstream_fails() {
    let problems = mutated(|c| c["constraints"][0]["collection"] = json!("Lines"));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("does not exist upstream")),
        "{problems:?}"
    );
}

#[test]
fn a_constraint_kind_upstream_lacks_fails() {
    let problems = mutated(|c| {
        c["constraints"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "pk", "kind": "primary_key",
            "dataset": "InvoiceDocumentType", "fields": ["ID"], "origin": "declared"}));
    });
    assert!(
        problems
            .iter()
            .any(|p| p.contains("primary_key InvoiceDocumentType[ID]")),
        "{problems:?}"
    );
}

#[test]
fn a_row_missing_its_keys_is_refused_not_skipped() {
    let problems = mutated(|c| {
        c["fields"][0].as_object_mut().unwrap().remove("name");
        c["datasets"][0].as_object_mut().unwrap().remove("id");
    });
    assert!(
        problems
            .iter()
            .any(|p| p == "a contract field has no dataset or name"),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p == "a contract dataset has no id"),
        "{problems:?}"
    );
}

#[test]
fn flattening_through_a_repeating_step_is_refused() {
    let schema = schema();
    let refused = project(
        &schema,
        &spec(vec![flat(&["Line", "ID"], "first_line")]),
        &repeating_fields(&schema),
    );
    assert!(
        refused
            .unwrap_err()
            .contains("step Line can occur more than once")
    );
}

#[test]
fn a_section_that_is_not_an_array_is_refused() {
    let problems = mutated(|c| c["fields"] = json!({"ID": {}}));
    assert!(
        problems
            .iter()
            .any(|p| p == "contract fields is not an array"),
        "{problems:?}"
    );
}
