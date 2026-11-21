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

use norn_formats::SchemaImporter;
use norn_formats::contract::{Disposition, SourceElement, sha256_hex};
use norn_formats::owl::{
    OwlConfig, OwlGraph, OwlNamespace, OwlNormaliser, OwlReader, WitnessPolicy,
};
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
        witnesses: WitnessPolicy::Open,
    }
}

fn interfaces(import: &norn_formats::SchemaImport) -> BTreeMap<String, Value> {
    import.layers[0]
        .artifacts
        .iter()
        .map(|artifact| {
            let value = serde_json::to_value(artifact).unwrap();
            (
                value["payload"]["interface"]["name"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
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
        other @ (Disposition::Extension { .. }
        | Disposition::Rejected { .. }
        | Disposition::Dropped { .. }
        | Disposition::Unexplained) => panic!("{axiom}: {other:?}"),
    }
}

#[test]
fn owl_ledger_conformance_and_goldens() {
    let import = support::check_conformance(&OwlReader, graph(), &config(), "ledger");
    support::assert_golden(
        "tests/goldens/owl/ledger.output.json",
        &support::output_without_loss(&import),
    );
    support::assert_golden(
        "tests/goldens/owl/ledger.loss.jsonl",
        &support::loss_jsonl(&import.loss_report),
    );
}

#[test]
fn owl_classes_become_interfaces_with_extends_links_and_properties() {
    let import = OwlReader.import(graph(), &config());
    let interfaces = interfaces(&import);
    assert_eq!(interfaces.len(), 19, "{:?}", interfaces.keys());
    let loan = &interfaces["ex.ln.Loan"];
    assert_eq!(loan["extends"], serde_json::json!(["ex.agr.Contract"]));
    let borrower = loan["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "has_borrower")
        .unwrap();
    assert_eq!(borrower["target"], "ex.pty.Party");
    // someValuesFrom over the range: open-world, so optional; FunctionalProperty → to one.
    assert_eq!(
        borrower["cardinality"],
        serde_json::json!({"to": "one", "from": "many"})
    );
    // `exactly 1 xsd:decimal` on a property with no range counts only its
    // decimal values: filler-scoped, so no member.
    assert!(
        loan["properties"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|p| p["id"] != "has_principal"),
        "{loan}"
    );
    // Domain → an optional member on the domain, targeting the range.
    let agreement = &interfaces["ex.agr.Agreement"];
    let party = agreement["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "has_party")
        .unwrap();
    assert_eq!(
        party["cardinality"],
        serde_json::json!({"to": "many", "from": "many"})
    );
    // min 2 over the range is an open-world claim: the member stays optional.
    let contract = &interfaces["ex.agr.Contract"];
    let party = contract["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "has_party")
        .unwrap();
    assert!(party["cardinality"].get("required").is_none(), "{party}");
    let legal = &interfaces["ex.pty.LegalEntity"];
    let date = legal["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "is_registered_on")
        .unwrap();
    assert_eq!(date["constraints"]["nullable"], true, "max 1 → nullable");
    // `secures some Collateral` and `secures some Guarantee` are separate
    // witnesses, not one link: neither narrows a member, so both edges stay.
    assert_eq!(
        interfaces["ex.ln.PledgedSurety"]["extends"],
        serde_json::json!(["ex.ln.Pledge", "ex.ln.Surety"])
    );
    // A subclass cycle: the edge that closes it is not lowered.
    assert_eq!(
        interfaces["ex.ln.Facility"]["extends"],
        serde_json::json!(["ex.ln.CreditLine"])
    );
    assert!(interfaces["ex.ln.CreditLine"].get("extends").is_none());
}

#[test]
fn owl_closed_witness_policy_requires_the_ledger_witnesses() {
    let mut config = config();
    config.witnesses = WitnessPolicy::Closed;
    let import = OwlReader.import(graph(), &config);
    let interfaces = interfaces(&import);
    let member = |class: &str, id: &str| {
        interfaces[class]["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        member("ex.ln.Loan", "has_borrower")["cardinality"],
        serde_json::json!({"to": "one", "from": "many", "required": true})
    );
    assert_eq!(
        member("ex.agr.Contract", "has_party")["cardinality"]["required"],
        true
    );
}

#[test]
fn owl_every_unlowered_axiom_names_its_reason() {
    let import = OwlReader.import(graph(), &config());
    for (axiom, code) in [
        (
            "DisjointClasses(ex-pty:LegalEntity ex-pty:NaturalPerson)",
            "format.owl.disjointness",
        ),
        (
            "DisjointClasses(ex-pty:Corporation ex-pty:Partnership ex-pty:Trust)",
            "format.owl.disjointness",
        ),
        (
            "SubClassOf(ex-ln:Loan ObjectUnionOf(ex-ln:SecuredLoan ex-ln:UnsecuredLoan))",
            "format.owl.class_expression",
        ),
        (
            "SubClassOf(ex-ln:Loan ObjectHasValue(ex-ln:hasCurrency ex-ln:USD))",
            "format.owl.value_restriction",
        ),
        (
            "SubClassOf(ex-ln:Loan <https://external.example/vocab/Product>)",
            "format.owl.external_reference",
        ),
        ("SubClassOf(ex-ln:PledgedSurety ex-ln:Surety)", "mapped"),
        (
            "SubClassOf(ex-ln:Pledge ObjectSomeValuesFrom(ex-ln:secures ex-ln:Collateral))",
            "format.owl.filler_scoped",
        ),
        (
            "SubClassOf(ex-ln:CreditLine ex-ln:Facility)",
            "format.owl.equivalence",
        ),
        ("SubClassOf(ex-ln:Facility ex-ln:CreditLine)", "mapped"),
        (
            "subPropertyOf(ex-ln:hasBorrower ex-agr:hasParty)",
            "format.owl.property_axiom",
        ),
        (
            "inverseOf(ex-agr:isPartyTo ex-agr:hasParty)",
            "format.owl.property_axiom",
        ),
        (
            "SubClassOf(ex-agr:Contract ObjectMinCardinality(2 ex-agr:hasParty ex-pty:Party))",
            "mapped",
        ),
        ("FunctionalProperty(ex-ln:hasBorrower)", "mapped"),
    ] {
        assert_eq!(code_of(&import, axiom), code, "{axiom}");
    }
    let report = &import.loss_report;
    let individual = report
        .entry(&SourceElement::definition(
            "owl.individual",
            "https://example.org/onto/Loans/USD",
        ))
        .unwrap();
    assert!(
        matches!(&individual.disposition, Disposition::Deferred { code, .. } if code == "format.owl.individual")
    );
    let alias = report
        .entry(&SourceElement::definition(
            "owl.datatype_property",
            "https://example.org/onto/Parties/hasAlias",
        ))
        .unwrap();
    assert!(
        matches!(&alias.disposition, Disposition::Deferred { code, .. } if code == "format.owl.unplaced_property")
    );
    let summary = report.summary();
    assert_eq!(summary.unexplained, 0);
    assert_eq!(
        summary.dropped, 0,
        "nothing is dropped; unlowered axioms are deferred with a reason"
    );
}

#[test]
fn owl_unpinned_graph_fails_closed_with_complete_inventory() {
    let mut config = config();
    config.graph_sha256 = "0".repeat(64);
    let import = OwlReader.import(graph(), &config);
    assert!(import.source_schema.is_none() && import.layers.is_empty());
    assert!(
        import
            .diagnostics
            .iter()
            .any(|d| d.code == "format.owl.invalid")
    );
    assert!(
        import
            .loss_report
            .validate_inventory(&OwlReader.inventory(graph(), &config))
            .is_empty()
    );
    assert_eq!(import.loss_report.summary().unexplained, 0);
}

#[test]
fn owl_malformed_graph_is_refused() {
    let bytes = b"<https://example.org/onto/Parties/A> <rel> <https://e.x/o> .\n";
    let mut config = config();
    config.graph_sha256 = sha256_hex(bytes);
    let import = OwlReader.import(
        OwlGraph {
            path: "bad.nt",
            bytes,
        },
        &config,
    );
    let error = import
        .diagnostics
        .iter()
        .find(|d| d.code == "format.owl.invalid")
        .unwrap();
    assert!(error.message.contains("not absolute"), "{}", error.message);
}

/// A small ratio ontology for the restriction rules (S1/S2 of #1537): two
/// `some` restrictions with different fillers on one property, a qualified
/// `min 0`, a qualified `max 1`, a class-expression domain, and a range no
/// member takes its target from.
const RATIOS: &str = concat!(
    "<https://example.org/onto/Ratios/> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Ontology> .\n",
    "<https://example.org/onto/Ratios/Ratio> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/LoanToValueRatio> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/LoanToValueRatio> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Ratio> .\n",
    "<https://example.org/onto/Ratios/Quantity> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Principal> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Principal> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Quantity> .\n",
    "<https://example.org/onto/Ratios/AppraisedValue> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/AppraisedValue> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Quantity> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Gadget> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Widget> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Widget> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Gadget> .\n",
    // hasArgument: Ratio → Quantity; the LTV ratio has a principal and an appraised value.
    "<https://example.org/onto/Ratios/hasArgument> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#ObjectProperty> .\n",
    "<https://example.org/onto/Ratios/hasArgument> <http://www.w3.org/2000/01/rdf-schema#domain> <https://example.org/onto/Ratios/Ratio> .\n",
    "<https://example.org/onto/Ratios/hasArgument> <http://www.w3.org/2000/01/rdf-schema#range> <https://example.org/onto/Ratios/Quantity> .\n",
    "<https://example.org/onto/Ratios/LoanToValueRatio> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r1 .\n",
    "_:r1 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r1 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/hasArgument> .\n",
    "_:r1 <http://www.w3.org/2002/07/owl#someValuesFrom> <https://example.org/onto/Ratios/Principal> .\n",
    "<https://example.org/onto/Ratios/LoanToValueRatio> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r2 .\n",
    "_:r2 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r2 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/hasArgument> .\n",
    "_:r2 <http://www.w3.org/2002/07/owl#someValuesFrom> <https://example.org/onto/Ratios/AppraisedValue> .\n",
    // holds: Holder → Gadget, and a qualified min 0 over widgets.
    "<https://example.org/onto/Ratios/holds> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#ObjectProperty> .\n",
    "<https://example.org/onto/Ratios/holds> <http://www.w3.org/2000/01/rdf-schema#domain> <https://example.org/onto/Ratios/Holder> .\n",
    "<https://example.org/onto/Ratios/holds> <http://www.w3.org/2000/01/rdf-schema#range> <https://example.org/onto/Ratios/Gadget> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r3 .\n",
    "_:r3 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r3 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/holds> .\n",
    "_:r3 <http://www.w3.org/2002/07/owl#minQualifiedCardinality> \"0\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger> .\n",
    "_:r3 <http://www.w3.org/2002/07/owl#onClass> <https://example.org/onto/Ratios/Widget> .\n",
    // owns: Holder → Gadget, and at most one widget (other gadgets are free).
    "<https://example.org/onto/Ratios/owns> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#ObjectProperty> .\n",
    "<https://example.org/onto/Ratios/owns> <http://www.w3.org/2000/01/rdf-schema#domain> <https://example.org/onto/Ratios/Holder> .\n",
    "<https://example.org/onto/Ratios/owns> <http://www.w3.org/2000/01/rdf-schema#range> <https://example.org/onto/Ratios/Gadget> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r4 .\n",
    "_:r4 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r4 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/owns> .\n",
    "_:r4 <http://www.w3.org/2002/07/owl#maxQualifiedCardinality> \"1\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger> .\n",
    "_:r4 <http://www.w3.org/2002/07/owl#onClass> <https://example.org/onto/Ratios/Widget> .\n",
    // cites: a union domain, placed on Holder only by an `only` restriction.
    "<https://example.org/onto/Ratios/cites> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#ObjectProperty> .\n",
    "<https://example.org/onto/Ratios/cites> <http://www.w3.org/2000/01/rdf-schema#range> <https://example.org/onto/Ratios/Gadget> .\n",
    "<https://example.org/onto/Ratios/cites> <http://www.w3.org/2000/01/rdf-schema#domain> _:u .\n",
    "_:u <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "_:u <http://www.w3.org/2002/07/owl#unionOf> _:l1 .\n",
    "_:l1 <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <https://example.org/onto/Ratios/Ratio> .\n",
    "_:l1 <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> _:l2 .\n",
    "_:l2 <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <https://example.org/onto/Ratios/Holder> .\n",
    "_:l2 <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r5 .\n",
    "_:r5 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r5 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/cites> .\n",
    "_:r5 <http://www.w3.org/2002/07/owl#allValuesFrom> <https://example.org/onto/Ratios/Widget> .\n",
    // keeps: two parents say every value is an unrelated type — a real conflict.
    "<https://example.org/onto/Ratios/keeps> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#ObjectProperty> .\n",
    "<https://example.org/onto/Ratios/Keeper> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Minder> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/KeeperMinder> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/KeeperMinder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Keeper> .\n",
    "<https://example.org/onto/Ratios/KeeperMinder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Minder> .\n",
    "<https://example.org/onto/Ratios/Keeper> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r6 .\n",
    "_:r6 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r6 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/keeps> .\n",
    "_:r6 <http://www.w3.org/2002/07/owl#allValuesFrom> <https://example.org/onto/Ratios/Widget> .\n",
    "<https://example.org/onto/Ratios/Minder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:r7 .\n",
    "_:r7 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:r7 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/keeps> .\n",
    "_:r7 <http://www.w3.org/2002/07/owl#allValuesFrom> <https://example.org/onto/Ratios/Quantity> .\n",
);

fn ratios(witnesses: WitnessPolicy) -> norn_formats::SchemaImport {
    let mut config = config();
    config.graph_sha256 = sha256_hex(RATIOS.as_bytes());
    config.namespaces = vec![OwlNamespace {
        iri: "https://example.org/onto/Ratios/".into(),
        prefix: "ex-rt".into(),
    }];
    config.witnesses = witnesses;
    let graph = OwlGraph {
        path: "ratios.nt",
        bytes: RATIOS.as_bytes(),
    };
    let import = OwlReader.import(graph, &config);
    assert!(
        import
            .diagnostics
            .iter()
            .all(|d| d.severity != norn_ir::DiagnosticSeverity::Error),
        "{:#?}",
        import.diagnostics
    );
    assert_eq!(import.loss_report.summary().unexplained, 0);
    import
}

fn link<'a>(interfaces: &'a BTreeMap<String, Value>, class: &str, id: &str) -> Option<&'a Value> {
    interfaces[class]["links"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|link| link["id"] == id)
}

const LTV_PRINCIPAL: &str =
    "SubClassOf(ex-rt:LoanToValueRatio ObjectSomeValuesFrom(ex-rt:hasArgument ex-rt:Principal))";
const LTV_APPRAISED: &str = "SubClassOf(ex-rt:LoanToValueRatio ObjectSomeValuesFrom(ex-rt:hasArgument ex-rt:AppraisedValue))";

#[test]
fn owl_separate_witnesses_are_not_one_link() {
    let import = ratios(WitnessPolicy::Open);
    let interfaces = interfaces(&import);
    // Neither filler narrows the link, and neither is an inheritance conflict.
    for axiom in [LTV_PRINCIPAL, LTV_APPRAISED] {
        assert_eq!(
            code_of(&import, axiom),
            "format.owl.filler_scoped",
            "{axiom}"
        );
    }
    assert!(
        link(&interfaces, "ex.rt.LoanToValueRatio", "has_argument").is_none(),
        "LoanToValueRatio declares no narrowed has_argument: {}",
        interfaces["ex.rt.LoanToValueRatio"]
    );
    let inherited = link(&interfaces, "ex.rt.Ratio", "has_argument").unwrap();
    assert_eq!(inherited["target"], "ex.rt.Quantity");
    assert_eq!(
        inherited["cardinality"],
        serde_json::json!({"to": "many", "from": "many"})
    );
}

#[test]
fn owl_qualified_min_zero_constrains_nothing() {
    let import = ratios(WitnessPolicy::Open);
    let interfaces = interfaces(&import);
    let holds = link(&interfaces, "ex.rt.Holder", "holds").unwrap();
    assert_eq!(
        holds["target"], "ex.rt.Gadget",
        "min 0 does not narrow: {holds}"
    );
    let axiom = "SubClassOf(ex-rt:Holder ObjectMinCardinality(0 ex-rt:holds ex-rt:Widget))";
    let entry = import
        .loss_report
        .entry(&SourceElement::definition("owl.axiom", axiom))
        .unwrap();
    match &entry.disposition {
        Disposition::Mapped {
            target, degraded, ..
        } => {
            assert_eq!(
                serde_json::to_value(target).unwrap(),
                serde_json::to_value(norn_formats::contract::Target::artifact(
                    "ex.rt.Holder.interface"
                ))
                .unwrap(),
                "mapped to the class, as no member"
            );
            assert!(degraded.is_none());
        }
        other @ (Disposition::Extension { .. }
        | Disposition::Deferred { .. }
        | Disposition::Rejected { .. }
        | Disposition::Dropped { .. }
        | Disposition::Unexplained) => panic!("{other:?}"),
    }
}

#[test]
fn owl_qualified_max_one_does_not_cap_the_link() {
    let import = ratios(WitnessPolicy::Open);
    let interfaces = interfaces(&import);
    let owns = link(&interfaces, "ex.rt.Holder", "owns").unwrap();
    assert_eq!(owns["target"], "ex.rt.Gadget");
    assert_eq!(owns["cardinality"]["to"], "many", "{owns}");
    assert_eq!(
        code_of(
            &import,
            "SubClassOf(ex-rt:Holder ObjectMaxCardinality(1 ex-rt:owns ex-rt:Widget))"
        ),
        "format.owl.filler_scoped"
    );
}

#[test]
fn owl_each_axiom_is_mapped_only_for_its_own_contribution() {
    let import = ratios(WitnessPolicy::Open);
    // `cites` lands on Holder (from `only Widget`), but its union domain
    // contributes nothing, and nor does its range.
    assert_eq!(
        code_of(
            &import,
            "PropertyDomain(ex-rt:cites ObjectUnionOf(ex-rt:Ratio ex-rt:Holder))"
        ),
        "format.owl.class_expression"
    );
    assert_eq!(
        code_of(&import, "PropertyRange(ex-rt:cites ex-rt:Gadget)"),
        "format.owl.unplaced_property"
    );
    assert_eq!(
        code_of(
            &import,
            "SubClassOf(ex-rt:Holder ObjectAllValuesFrom(ex-rt:cites ex-rt:Widget))"
        ),
        "mapped"
    );
    // A range that types a member is mapped.
    assert_eq!(
        code_of(&import, "PropertyRange(ex-rt:holds ex-rt:Gadget)"),
        "mapped"
    );
}

#[test]
fn owl_closed_witness_policy_requires_existence_only() {
    let import = ratios(WitnessPolicy::Closed);
    let interfaces = interfaces(&import);
    // Both witnesses lower to one required link to the range, degraded.
    let argument = link(&interfaces, "ex.rt.LoanToValueRatio", "has_argument").unwrap();
    assert_eq!(argument["target"], "ex.rt.Quantity");
    assert_eq!(argument["cardinality"]["required"], true);
    for axiom in [LTV_PRINCIPAL, LTV_APPRAISED] {
        let entry = import
            .loss_report
            .entry(&SourceElement::definition("owl.axiom", axiom))
            .unwrap();
        assert!(
            matches!(&entry.disposition, Disposition::Mapped { degraded: Some(d), .. } if d.code == "format.owl.filler_scoped"),
            "{axiom}: {:?}",
            entry.disposition
        );
    }
    // The qualified cap is still not the link's.
    let owns = link(&interfaces, "ex.rt.Holder", "owns").unwrap();
    assert_eq!(owns["cardinality"]["to"], "many");
}

#[test]
fn owl_unrelated_universal_targets_still_conflict() {
    let import = ratios(WitnessPolicy::Open);
    let interfaces = interfaces(&import);
    assert_eq!(
        interfaces["ex.rt.KeeperMinder"]["extends"],
        serde_json::json!(["ex.rt.Keeper"])
    );
    assert_eq!(
        code_of(&import, "SubClassOf(ex-rt:KeeperMinder ex-rt:Minder)"),
        "format.owl.inheritance_conflict"
    );
}
