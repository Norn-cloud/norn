//! Boundary contract: the host tool's golden output decodes into the pure
//! contract, and the inventory join requires identical bytes and revision.
#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "integration assertions on the host code-ontology golden"
)]
use norn_code_ontology::{CodeOntology, Confidence, FORMAT, Relation};

#[test]
fn host_golden_decodes_into_pure_contract() {
    let graph: CodeOntology = serde_json::from_str(include_str!(
        "../../../tools/norn-port/code-ontology-fixtures/golden.json"
    ))
    .expect("host JSON uses the pure importer contract");
    assert_eq!(graph.format, FORMAT);
    assert!(
        graph
            .facts
            .iter()
            .any(|f| f.relation == Relation::Schedules)
    );
    assert!(
        graph
            .facts
            .iter()
            .any(|f| { f.confidence == Confidence::Unresolved && f.diagnostic.is_some() })
    );
    assert!(
        graph
            .facts
            .iter()
            .all(|f| { (f.confidence == Confidence::Unresolved) == f.diagnostic.is_some() })
    );
}

#[test]
fn graph_join_requires_original_bytes_and_revision() {
    let graph: CodeOntology = serde_json::from_str(include_str!(
        "../../../tools/norn-port/code-ontology-fixtures/golden.json"
    ))
    .expect("host JSON");
    let fact = graph
        .facts
        .iter()
        .find(|f| f.handler.ends_with(":update"))
        .expect("fixture API facts");
    let mut unit: norn_code_ontology::CodeUnit = serde_json::from_value(serde_json::json!({
        "id": fact.handler,
        "app": "demo",
        "doctype": "",
        "kind": "function",
        "source": fact.source,
        "symbol": "update",
        "reads": [], "writes": [], "calls": [], "triggers": [], "clause": null
    }))
    .expect("existing behaviour unit contract");
    unit.source.path = format!("code/{}", unit.source.path);
    assert!(graph.facts_for(&unit).next().is_some());
    unit.source.revision = "other-revision".into();
    assert!(graph.facts_for(&unit).next().is_none());
    unit.source.revision.clone_from(&fact.source.revision);
    unit.source.sha256 = "different-bytes".into();
    assert!(graph.facts_for(&unit).next().is_none());
}

#[test]
fn callback_join_retains_registration_evidence() {
    let graph: CodeOntology = serde_json::from_str(include_str!(
        "../../../tools/norn-port/code-ontology-fixtures/golden.json"
    ))
    .expect("host JSON");
    for (symbol, relation) in [
        ("on_update", Relation::TriggersOn),
        ("daily", Relation::Schedules),
    ] {
        let handler = graph
            .handlers
            .iter()
            .find(|h| h.id == format!("demo/demo/api.py:{symbol}"))
            .expect("fixture callback");
        let mut unit: norn_code_ontology::CodeUnit =
            serde_json::from_value(serde_json::json!({
                "id": handler.id, "app": "demo", "doctype": "", "kind": "function",
                "source": handler.source, "symbol": symbol,
                "reads": [], "writes": [], "calls": [], "triggers": [], "clause": null
            }))
            .expect("behaviour callback unit");
        unit.source.path = format!("code/{}", unit.source.path);
        let registration = graph
            .facts_for(&unit)
            .find(|f| f.relation == relation)
            .expect("callback retains hook/scheduler registration");
        assert_eq!(registration.source.path, "demo/demo/hooks.py");
        assert_ne!(registration.source.sha256, unit.source.sha256);
        unit.source.sha256 = "stale-callback-bytes".into();
        assert!(graph.facts_for(&unit).next().is_none());
    }
}
