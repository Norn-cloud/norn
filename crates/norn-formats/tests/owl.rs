#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration tests"
)]
//! The OWL reader on a synthetic, finance-shaped ontology
//! (`fixtures/synthetic/ledger.ttl`, normalised to `ledger.nt` by the pinned
//! rdflib exactly as `scripts/importers/owl_sources.py` normalises a pack's
//! originals). Not FIBO content: three small modules that exercise every
//! lowering rule and every deferral reason.

mod support;

use std::collections::BTreeMap;

use norn_formats::contract::{Disposition, SourceElement, sha256_hex};
use norn_formats::owl::{OwlConfig, OwlGraph, OwlNamespace, OwlNormaliser, OwlReader};
use norn_formats::SchemaImporter;
use norn_ir::{OntologySource, OntologySourceFile};
use serde_json::Value;

const GRAPH: &[u8] = include_bytes!("fixtures/synthetic/ledger.nt");
const ORIGINAL: &[u8] = include_bytes!("fixtures/synthetic/ledger.ttl");

fn graph() -> OwlGraph<'static> {
    OwlGraph {
        path: "ledger.nt",
        bytes: GRAPH,
    }
}

fn config() -> OwlConfig {
    let mut admission =
        serde_json::from_str::<OntologySource>(include_str!("fixtures/synthetic/source.json"))
            .unwrap();
    admission.source_id = "source.test.synthetic-ledger".into();
    admission.license = "MIT".into();
    admission.importer = Some(norn_ir::SourceImporter {
        name: "norn.formats.owl".into(),
        version: "0.1.0".into(),
    });
    admission.files = Some(vec![OntologySourceFile {
        path: "https://example.test/synthetic-ledger/1.0/ledger.ttl".into(),
        sha256: sha256_hex(ORIGINAL),
    }]);
    let namespace = |iri: &str, prefix: &str| OwlNamespace {
        iri: iri.into(),
        prefix: prefix.into(),
    };
    OwlConfig {
        admission,
        observed_at: "2026-11-20T00:00:00Z".into(),
        graph_sha256: sha256_hex(GRAPH),
        normaliser: OwlNormaliser {
            name: "rdflib".into(),
            version: "7.6.0".into(),
        },
        namespaces: vec![
            namespace("https://example.org/onto/Parties/", "ex-pty"),
            namespace("https://example.org/onto/Agreements/", "ex-agr"),
            namespace("https://example.org/onto/Loans/", "ex-ln"),
        ],
    }
}

fn interfaces(import: &norn_formats::SchemaImport) -> BTreeMap<String, Value> {
    import.layers[0]
        .artifacts
        .iter()
        .map(|artifact| {
            let value = serde_json::to_value(artifact).unwrap();
            (
                value["payload"]["interface"]["name"].as_str().unwrap().to_owned(),
                value["payload"]["interface"].clone(),
            )
        })
        .collect()
}

fn code_of<'a>(import: &'a norn_formats::SchemaImport, axiom: &str) -> &'a str {
    let entry = import
        .loss_report
        .entry(&SourceElement::definition("owl.axiom", axiom))
        .unwrap_or_else(|| panic!("no entry for {axiom}"));
    match &entry.disposition {
        Disposition::Deferred { code, .. } => code,
        Disposition::Mapped { .. } => "mapped",
        other => panic!("{axiom}: {other:?}"),
    }
}

#[test]
fn owl_ledger_conformance_and_goldens() {
    let import = support::check_conformance(&OwlReader, graph(), &config(), "ledger");
    support::assert_golden("tests/goldens/owl/ledger.output.json", &support::output_without_loss(&import));
    support::assert_golden("tests/goldens/owl/ledger.loss.jsonl", &support::loss_jsonl(&import.loss_report));
}

#[test]
fn owl_classes_become_interfaces_with_extends_links_and_properties() {
    let import = OwlReader.import(graph(), &config());
    let interfaces = interfaces(&import);
    assert_eq!(interfaces.len(), 19, "{:?}", interfaces.keys());
    let loan = &interfaces["ex.ln.Loan"];
    assert_eq!(loan["extends"], serde_json::json!(["ex.agr.Contract"]));
    let borrower = loan["links"].as_array().unwrap().iter().find(|l| l["id"] == "has_borrower").unwrap();
    assert_eq!(borrower["target"], "ex.pty.Party");
    // someValuesFrom → required; FunctionalProperty → to one.
    assert_eq!(borrower["cardinality"], serde_json::json!({"to": "one", "from": "many", "required": true}));
    let principal = loan["properties"].as_array().unwrap().iter().find(|p| p["id"] == "has_principal").unwrap();
    assert_eq!(principal["type"], serde_json::json!({"kind": "text"}), "unbounded decimal stays lexical");
    assert!(principal.get("constraints").is_none(), "exactly one → not nullable");
    // Domain → an optional member on the domain, targeting the range.
    let agreement = &interfaces["ex.agr.Agreement"];
    let party = agreement["links"].as_array().unwrap().iter().find(|l| l["id"] == "has_party").unwrap();
    assert_eq!(party["cardinality"], serde_json::json!({"to": "many", "from": "many"}));
    // min 2 narrows the inherited member to required.
    let contract = &interfaces["ex.agr.Contract"];
    let party = contract["links"].as_array().unwrap().iter().find(|l| l["id"] == "has_party").unwrap();
    assert_eq!(party["cardinality"]["required"], true);
    let legal = &interfaces["ex.pty.LegalEntity"];
    let date = legal["properties"].as_array().unwrap().iter().find(|p| p["id"] == "is_registered_on").unwrap();
    assert_eq!(date["constraints"]["nullable"], true, "max 1 → nullable");
    // Two parents pass `secures` on with unrelated targets: one edge stays.
    assert_eq!(interfaces["ex.ln.PledgedSurety"]["extends"], serde_json::json!(["ex.ln.Pledge"]));
    // A subclass cycle: the edge that closes it is not lowered.
    assert_eq!(interfaces["ex.ln.Facility"]["extends"], serde_json::json!(["ex.ln.CreditLine"]));
    assert!(interfaces["ex.ln.CreditLine"].get("extends").is_none());
}

#[test]
fn owl_every_unlowered_axiom_names_its_reason() {
    let import = OwlReader.import(graph(), &config());
    for (axiom, code) in [
        ("DisjointClasses(ex-pty:LegalEntity ex-pty:NaturalPerson)", "format.owl.disjointness"),
        ("DisjointClasses(ex-pty:Corporation ex-pty:Partnership ex-pty:Trust)", "format.owl.disjointness"),
        ("SubClassOf(ex-ln:Loan ObjectUnionOf(ex-ln:SecuredLoan ex-ln:UnsecuredLoan))", "format.owl.class_expression"),
        ("SubClassOf(ex-ln:Loan ObjectHasValue(ex-ln:hasCurrency ex-ln:USD))", "format.owl.value_restriction"),
        ("SubClassOf(ex-ln:Loan <https://external.example/vocab/Product>)", "format.owl.external_reference"),
        ("SubClassOf(ex-ln:PledgedSurety ex-ln:Surety)", "format.owl.inheritance_conflict"),
        ("SubClassOf(ex-ln:CreditLine ex-ln:Facility)", "format.owl.equivalence"),
        ("SubClassOf(ex-ln:Facility ex-ln:CreditLine)", "mapped"),
        ("subPropertyOf(ex-ln:hasBorrower ex-agr:hasParty)", "format.owl.property_axiom"),
        ("inverseOf(ex-agr:isPartyTo ex-agr:hasParty)", "format.owl.property_axiom"),
        ("SubClassOf(ex-agr:Contract ObjectMinCardinality(2 ex-agr:hasParty ex-pty:Party))", "mapped"),
        ("FunctionalProperty(ex-ln:hasBorrower)", "mapped"),
    ] {
        assert_eq!(code_of(&import, axiom), code, "{axiom}");
    }
    let report = &import.loss_report;
    let individual = report
        .entry(&SourceElement::definition("owl.individual", "https://example.org/onto/Loans/USD"))
        .unwrap();
    assert!(matches!(&individual.disposition, Disposition::Deferred { code, .. } if code == "format.owl.individual"));
    let alias = report
        .entry(&SourceElement::definition("owl.datatype_property", "https://example.org/onto/Parties/hasAlias"))
        .unwrap();
    assert!(matches!(&alias.disposition, Disposition::Deferred { code, .. } if code == "format.owl.unplaced_property"));
    let summary = report.summary();
    assert_eq!(summary.unexplained, 0);
    assert_eq!(summary.dropped, 0, "nothing is dropped; unlowered axioms are deferred with a reason");
}

#[test]
fn owl_unpinned_graph_fails_closed_with_complete_inventory() {
    let mut config = config();
    config.graph_sha256 = "0".repeat(64);
    let import = OwlReader.import(graph(), &config);
    assert!(import.source_schema.is_none() && import.layers.is_empty());
    assert!(import.diagnostics.iter().any(|d| d.code == "format.owl.invalid"));
    assert!(import.loss_report.validate_inventory(&OwlReader.inventory(graph(), &config)).is_empty());
    assert_eq!(import.loss_report.summary().unexplained, 0);
}

#[test]
fn owl_malformed_graph_is_refused() {
    let bytes = b"<https://example.org/onto/Parties/A> <rel> <https://e.x/o> .\n";
    let mut config = config();
    config.graph_sha256 = sha256_hex(bytes);
    let import = OwlReader.import(OwlGraph { path: "bad.nt", bytes }, &config);
    let error = import.diagnostics.iter().find(|d| d.code == "format.owl.invalid").unwrap();
    assert!(error.message.contains("not absolute"), "{}", error.message);
}
