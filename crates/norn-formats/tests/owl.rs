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
use norn_formats::owl::{OwlGraph, OwlNamespace, OwlReader, WitnessPolicy};
use serde_json::Value;
use support::{synthetic_ledger_config as config, synthetic_ledger_graph as graph};

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
    assert_eq!(
        import
            .loss_report
            .validate_inventory(&OwlReader.inventory(graph(), &config))
            .len(),
        0
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
    import_ratios(RATIOS, witnesses)
}

fn ratios_config(bytes: &[u8], witnesses: WitnessPolicy) -> norn_formats::owl::OwlConfig {
    let mut config = config();
    config.graph_sha256 = sha256_hex(bytes);
    config.namespaces = vec![OwlNamespace {
        iri: "https://example.org/onto/Ratios/".into(),
        prefix: "ex-rt".into(),
    }];
    config.witnesses = witnesses;
    config
}

/// Import `text` with the `ex-rt` namespace owned.
fn import_ratios(text: &str, witnesses: WitnessPolicy) -> norn_formats::SchemaImport {
    let config = ratios_config(text.as_bytes(), witnesses);
    let graph = OwlGraph {
        path: "ratios.nt",
        bytes: text.as_bytes(),
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

/// Data properties under filler-scoped `some` (S1 of #1537 round 2) and
/// documentation that a definition displaces (S2): `notes` has no bound,
/// `code` is functional, and `tag` is capped at one on the whole property.
const NOTES: &str = concat!(
    "<https://example.org/onto/Ratios/> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Ontology> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#label> \"holder\"@en .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2004/02/skos/core#definition> \"One who holds.\"@en .\n",
    "<https://example.org/onto/Ratios/Keeper> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/Keeper> <http://www.w3.org/2000/01/rdf-schema#label> \"keeper\"@en .\n",
    "<https://example.org/onto/Ratios/SubHolder> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
    "<https://example.org/onto/Ratios/SubHolder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Holder> .\n",
    // notes: many strings; a holder has some token among them.
    "<https://example.org/onto/Ratios/notes> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#DatatypeProperty> .\n",
    "<https://example.org/onto/Ratios/notes> <http://www.w3.org/2000/01/rdf-schema#range> <http://www.w3.org/2001/XMLSchema#string> .\n",
    "<https://example.org/onto/Ratios/notes> <http://www.w3.org/2000/01/rdf-schema#label> \"notes\"@en .\n",
    "<https://example.org/onto/Ratios/notes> <http://www.w3.org/2004/02/skos/core#definition> \"Free remarks.\"@en .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:n1 .\n",
    "_:n1 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:n1 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/notes> .\n",
    "_:n1 <http://www.w3.org/2002/07/owl#someValuesFrom> <http://www.w3.org/2001/XMLSchema#token> .\n",
    // code: functional, so one value at most.
    "<https://example.org/onto/Ratios/code> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#DatatypeProperty> .\n",
    "<https://example.org/onto/Ratios/code> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#FunctionalProperty> .\n",
    "<https://example.org/onto/Ratios/code> <http://www.w3.org/2000/01/rdf-schema#range> <http://www.w3.org/2001/XMLSchema#string> .\n",
    "<https://example.org/onto/Ratios/code> <http://www.w3.org/2000/01/rdf-schema#label> \"code\"@en .\n",
    "<https://example.org/onto/Ratios/code> <http://www.w3.org/2004/02/skos/core#definition> \"A short identifier.\"@en .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:n2 .\n",
    "_:n2 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:n2 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/code> .\n",
    "_:n2 <http://www.w3.org/2002/07/owl#someValuesFrom> <http://www.w3.org/2001/XMLSchema#token> .\n",
    // tag: at most one on a holder (so on a sub-holder), and some token.
    "<https://example.org/onto/Ratios/tag> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#DatatypeProperty> .\n",
    "<https://example.org/onto/Ratios/tag> <http://www.w3.org/2000/01/rdf-schema#range> <http://www.w3.org/2001/XMLSchema#string> .\n",
    "<https://example.org/onto/Ratios/Holder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:n3 .\n",
    "_:n3 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:n3 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/tag> .\n",
    "_:n3 <http://www.w3.org/2002/07/owl#maxCardinality> \"1\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger> .\n",
    "<https://example.org/onto/Ratios/SubHolder> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:n4 .\n",
    "_:n4 <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Restriction> .\n",
    "_:n4 <http://www.w3.org/2002/07/owl#onProperty> <https://example.org/onto/Ratios/tag> .\n",
    "_:n4 <http://www.w3.org/2002/07/owl#someValuesFrom> <http://www.w3.org/2001/XMLSchema#token> .\n",
    // keeps: Keeper's only documentation is its label, as is the member's.
    "<https://example.org/onto/Ratios/keeps> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#DatatypeProperty> .\n",
    "<https://example.org/onto/Ratios/keeps> <http://www.w3.org/2000/01/rdf-schema#domain> <https://example.org/onto/Ratios/Keeper> .\n",
    "<https://example.org/onto/Ratios/keeps> <http://www.w3.org/2000/01/rdf-schema#range> <http://www.w3.org/2001/XMLSchema#string> .\n",
    "<https://example.org/onto/Ratios/keeps> <http://www.w3.org/2000/01/rdf-schema#label> \"keeps\"@en .\n",
);

fn property<'a>(
    interfaces: &'a BTreeMap<String, Value>,
    class: &str,
    id: &str,
) -> Option<&'a Value> {
    interfaces[class]["properties"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|property| property["id"] == id)
}

/// A data member is one value, so a filler-scoped `some` lowers its
/// existence (closed) only where the property is already bounded to one.
#[test]
fn owl_closed_filler_scoped_data_existence_needs_a_bound_of_one() {
    let import = import_ratios(NOTES, WitnessPolicy::Closed);
    let interfaces = interfaces(&import);
    let notes = "SubClassOf(ex-rt:Holder DataSomeValuesFrom(ex-rt:notes xsd:token))";
    assert_eq!(code_of(&import, notes), "format.owl.filler_scoped");
    let entry = import
        .loss_report
        .entry(&SourceElement::definition("owl.axiom", notes))
        .unwrap();
    let Disposition::Deferred { detail, .. } = &entry.disposition else {
        panic!("{:?}", entry.disposition);
    };
    assert!(detail.contains("one value"), "{detail}");
    assert!(
        property(&interfaces, "ex.rt.Holder", "notes").is_none(),
        "no scalar caps the many notes: {}",
        interfaces["ex.rt.Holder"]
    );
    // Functional: the scalar bound holds, so existence is lowered.
    let code = property(&interfaces, "ex.rt.Holder", "code").unwrap();
    assert!(code.get("constraints").is_none(), "required: {code}");
    assert_eq!(
        code_of(
            &import,
            "SubClassOf(ex-rt:Holder DataSomeValuesFrom(ex-rt:code xsd:token))"
        ),
        "mapped"
    );
    // Capped at one on the whole property by an ancestor's `max 1`.
    let tag = property(&interfaces, "ex.rt.SubHolder", "tag").unwrap();
    assert!(tag.get("constraints").is_none(), "required: {tag}");
    assert_eq!(
        code_of(
            &import,
            "SubClassOf(ex-rt:SubHolder DataSomeValuesFrom(ex-rt:tag xsd:token))"
        ),
        "mapped"
    );
}

fn annotation(
    import: &norn_formats::SchemaImport,
    element: SourceElement,
) -> &norn_formats::contract::LossEntry {
    import
        .loss_report
        .entry(&element)
        .unwrap_or_else(|| panic!("no entry for {element:?}"))
}

/// A label is mapped only where the description carries it; a definition
/// takes precedence, and the label it displaces is deferred.
#[test]
fn owl_only_the_selected_annotation_is_mapped() {
    use norn_formats::contract::Facet;
    const SKOS: &str = "<http://www.w3.org/2004/02/skos/core#definition>";
    let import = import_ratios(NOTES, WitnessPolicy::Closed);
    let interfaces = interfaces(&import);
    let rt = "https://example.org/onto/Ratios/";
    let class = |local: &str, key: &str| {
        SourceElement::dataset_facet(&format!("{rt}{local}"), Facet::Key(key.into()))
    };
    let member = |local: &str, key: &str| {
        SourceElement::definition_facet(
            "owl.datatype_property",
            &format!("{rt}{local}"),
            Facet::Key(key.into()),
        )
    };
    assert_eq!(
        interfaces["ex.rt.Holder"]["description"],
        format!("One who holds. <{rt}Holder>")
    );
    let mapped = |entry: &norn_formats::contract::LossEntry| {
        matches!(entry.disposition, Disposition::Mapped { .. })
    };
    let displaced = |entry: &norn_formats::contract::LossEntry| match &entry.disposition {
        Disposition::Deferred { code, detail, .. } => {
            code == "format.owl.annotation" && detail.contains(SKOS)
        }
        Disposition::Mapped { .. }
        | Disposition::Extension { .. }
        | Disposition::Rejected { .. }
        | Disposition::Dropped { .. }
        | Disposition::Unexplained => false,
    };
    // The class: definition carried, label displaced.
    assert!(mapped(annotation(&import, class("Holder", SKOS))));
    let label = annotation(&import, class("Holder", "rdfs:label"));
    assert!(displaced(label), "{:?}", label.disposition);
    // A class with only a label carries it.
    assert!(mapped(annotation(&import, class("Keeper", "rdfs:label"))));
    // A placed member: the definition wins over the label.
    let code = property(&interfaces, "ex.rt.Holder", "code").unwrap();
    assert_eq!(
        code["description"],
        format!("A short identifier. <{rt}code>")
    );
    assert!(mapped(annotation(&import, member("code", SKOS))));
    let label = annotation(&import, member("code", "rdfs:label"));
    assert!(displaced(label), "{:?}", label.disposition);
    // A member with only a label carries it.
    let keeps = property(&interfaces, "ex.rt.Keeper", "keeps").unwrap();
    assert_eq!(keeps["description"], format!("keeps <{rt}keeps>"));
    assert!(mapped(annotation(&import, member("keeps", "rdfs:label"))));
    // `notes` lowers no member (S1): neither annotation is emitted.
    let notes_label = annotation(&import, member("notes", "rdfs:label"));
    assert!(!mapped(notes_label), "{:?}", notes_label.disposition);
    let notes_definition = annotation(&import, member("notes", SKOS));
    assert!(
        !mapped(notes_definition),
        "{:?}",
        notes_definition.disposition
    );
}

#[test]
fn owl_dag_blank_node_blowup_is_refused_within_budget() {
    use std::fmt::Write as _;

    let mut nt = String::from(
        "<https://example.org/onto/Ratios/A> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n\
         <https://example.org/onto/Ratios/A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:b0 .\n",
    );
    for level in 0..24_u32 {
        let next = level.saturating_add(1);
        writeln!(
            nt,
            "_:b{level} <https://example.org/onto/Ratios/l> _:b{next} .\n\
             _:b{level} <https://example.org/onto/Ratios/r> _:b{next} ."
        )
        .unwrap();
    }
    nt.push_str("_:b24 <https://example.org/onto/Ratios/leaf> \"x\" .\n");
    let config = ratios_config(nt.as_bytes(), WitnessPolicy::Open);
    let import = OwlReader.import(
        OwlGraph {
            path: "ratios.nt",
            bytes: nt.as_bytes(),
        },
        &config,
    );
    assert!(
        import
            .diagnostics
            .iter()
            .any(|d| d.message.contains("rendering size limit")),
        "{:?}",
        import.diagnostics
    );
}

#[test]
fn owl_accepts_long_rdf_lists_and_all_disjoint_classes_without_spurious_statements() {
    use std::fmt::Write as _;

    let mut nt = String::new();
    nt.push_str(
        "_:disj <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#AllDisjointClasses> .\n\
         _:disj <http://www.w3.org/2002/07/owl#members> _:n0 .\n",
    );
    for i in 0..40_u32 {
        writeln!(
            nt,
            "<https://example.org/onto/Ratios/C{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n\
             _:n{i} <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <https://example.org/onto/Ratios/C{i}> ."
        )
        .unwrap();
        let next = i.saturating_add(1);
        if next < 40 {
            writeln!(
                nt,
                "_:n{i} <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> _:n{next} ."
            )
            .unwrap();
        } else {
            writeln!(
                nt,
                "_:n{i} <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> ."
            )
            .unwrap();
        }
    }
    let import = import_ratios(&nt, WitnessPolicy::Open);
    assert_eq!(import.diagnostics, [], "{:?}", import.diagnostics);
    assert!(
        !import
            .loss_report
            .entries
            .iter()
            .any(|e| matches!(&e.element, SourceElement::Definition { family, .. } if family == "owl.statement")),
        "list cons-cells owned by owl:AllDisjointClasses must not leak as owl.statement entries"
    );
}

#[test]
fn owl_self_subclass_is_mapped_and_external_subclass_names_subclass() {
    let nt = concat!(
        "<https://example.org/onto/Ratios/Selfie> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
        "<https://example.org/onto/Ratios/Selfie> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Selfie> .\n",
        "<https://external.example/Ext> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .\n",
        "<https://external.example/Ext> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <https://example.org/onto/Ratios/Selfie> .\n",
    );
    let import = import_ratios(nt, WitnessPolicy::Open);
    assert_eq!(import.diagnostics, [], "{:?}", import.diagnostics);
    assert_eq!(
        code_of(&import, "SubClassOf(ex-rt:Selfie ex-rt:Selfie)"),
        "mapped"
    );
    let ext = import
        .loss_report
        .entry(&SourceElement::definition(
            "owl.axiom",
            "SubClassOf(<https://external.example/Ext> ex-rt:Selfie)",
        ))
        .expect("ext subclass entry");
    let Disposition::Deferred { detail, .. } = &ext.disposition else {
        panic!("{:?}", ext.disposition);
    };
    assert!(detail.contains("the subclass"), "{detail}");
}
