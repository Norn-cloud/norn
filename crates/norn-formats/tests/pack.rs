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
use norn_formats::xsd::{XsdConfig, XsdFile, XsdReader};
use norn_ir::OntologySource;
use serde_json::{Value, json};

const PATH: &str = "https://example.test/synthetic-invoice/1.0/invoice-shaped.xsd";
const XSD: &[u8] = include_bytes!("fixtures/synthetic/invoice-shaped.xsd");
const DOC: &str = "{urn:test:invoice}InvoiceDocumentType";

fn projection() -> Value {
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
    let schema = XsdReader.import(&files, &config).source_schema.unwrap();
    let flat = |path: &[&str], name: &str| Flatten {
        dataset: DOC.into(),
        path: path.iter().map(|s| (*s).to_owned()).collect(),
        name: name.into(),
    };
    let spec = ProjectionSpec {
        contract: "unused".into(),
        select: vec![DOC.into(), "{urn:test:invoice}LineType".into()],
        flatten: vec![
            flat(&["Supplier", "Endpoint"], "supplier_endpoint"),
            flat(&["Totals", "Payable"], "payable"),
        ],
    };
    project(&schema, &spec).unwrap()
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
