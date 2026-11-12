//! Boundary contract: the extractor's golden output decodes into the pure
//! contract, and the contract's open and closed parts behave as documented.
#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "integration assertions on the host code-ontology golden"
)]
use norn_code_ontology::{CodeOntology, Confidence, Diagnostic, FORMAT, Relation};

const GOLDEN: &str = include_str!("../../../tools/norn-port/code-ontology-fixtures/golden.json");

fn golden() -> CodeOntology {
    serde_json::from_str(GOLDEN).expect("the extractor's output uses the pure contract")
}

#[test]
fn the_extractor_golden_decodes_and_every_gap_is_typed() {
    let graph = golden();
    assert_eq!(graph.format, FORMAT);
    for relation in [
        Relation::Reads,
        Relation::Writes,
        Relation::TriggersOn,
        Relation::Calls,
        Relation::Schedules,
        Relation::ThrowsIf,
        Relation::ExposesApi,
    ] {
        assert!(
            graph.facts.iter().any(|fact| fact.relation == relation),
            "{relation:?}"
        );
    }
    // Unresolved exactly when a diagnostic says why.
    assert!(
        graph
            .facts
            .iter()
            .all(|fact| (fact.confidence == Confidence::Unresolved) == fact.diagnostic.is_some())
    );
    assert!(
        graph
            .facts
            .iter()
            .any(|fact| fact.confidence == Confidence::Unresolved)
    );
    // Every code the extractor emits today is one this crate names.
    let codes = graph
        .facts
        .iter()
        .filter_map(|fact| fact.diagnostic.as_ref())
        .chain(graph.diagnostics.iter().map(|gap| &gap.code))
        .chain(
            graph
                .coverage
                .values()
                .flat_map(|app| app.unresolved.keys()),
        );
    for code in codes {
        assert!(code.is_known(), "{}", code.as_str());
    }
    assert!(
        graph.facts.iter().any(|fact| {
            fact.diagnostic.as_ref().map(Diagnostic::as_str) == Some(Diagnostic::UNKNOWN_ENTITY)
        })
    );
}

#[test]
fn a_newer_diagnostic_code_decodes_and_a_newer_relation_does_not() {
    let newer = GOLDEN.replacen(
        r#""diagnostic": "unknown_entity""#,
        r#""diagnostic": "code_from_a_newer_extractor""#,
        1,
    );
    assert_ne!(newer, GOLDEN);
    let graph: CodeOntology = serde_json::from_str(&newer).expect("diagnostic codes are open");
    assert!(
        graph
            .facts
            .iter()
            .filter_map(|fact| fact.diagnostic.as_ref())
            .any(|code| !code.is_known() && code.as_str() == "code_from_a_newer_extractor")
    );
    let relation = GOLDEN.replacen(
        r#""relation": "reads""#,
        r#""relation": "relation_from_a_newer_format""#,
        1,
    );
    assert_ne!(relation, GOLDEN);
    assert!(serde_json::from_str::<CodeOntology>(&relation).is_err());
}
