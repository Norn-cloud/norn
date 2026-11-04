//! STRESS-PROPOSAL: agent-shaped changes receive stable, readable refusals.
#![allow(clippy::expect_used, clippy::panic, missing_docs, reason = "negative proposal scenarios")]
use std::collections::BTreeSet;
use norn_checker::{check_artifact_json, check_artifact_set, EntityArtifactSetContext};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Proposal {
    name: String,
    artifact: String,
    pointer: String,
    value: Value,
    code: String,
    text: String,
}
#[derive(Deserialize)]
struct Corpus { cases: Vec<Proposal> }
#[test]
fn invalid_agent_proposals_have_specific_readable_refusals() {
    let corpus: Corpus = serde_json::from_str(include_str!("../../../fixtures/stress/invalid-proposals.json")).expect("proposal corpus");
    assert_eq!(corpus.cases.len(), 20);
    let original: Vec<Value> = serde_json::from_str(include_str!("../../../fixtures/stress/purchase.json")).expect("generated definitions");
    let base: Vec<ArtifactEnvelope> = [include_str!("../../../registry/core/artifacts/core.Process.interface.json"), include_str!("../../../registry/core/artifacts/core.units.json")].iter().map(|s| serde_json::from_str(s).expect("base")).collect();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext { registry: &registry, base: &base, tenant: None };
    let mut failures = Vec::new();
    for proposal in corpus.cases {
        let mut values = original.clone();
        let artifact = values.iter_mut().find(|v| v.get("artifactId").and_then(Value::as_str) == Some(proposal.artifact.as_str())).expect("proposal subject");
        let (parent, field) = proposal.pointer.rsplit_once('/').expect("JSON pointer");
        let parent = artifact.pointer_mut(parent).expect("existing proposal parent");
        if let Some(object) = parent.as_object_mut() { object.insert(field.into(), proposal.value); }
        else { *parent.pointer_mut(&format!("/{field}")).expect("existing index") = proposal.value; }
        let mut diagnostics = check_artifact_json(artifact);
        let typed: Vec<ArtifactEnvelope> = values.into_iter().map(|v| serde_json::from_value(v).expect("opaque envelope")).collect();
        diagnostics.extend(check_artifact_set(&typed, &context));
        let matched = diagnostics.iter().any(|d| d.severity == DiagnosticSeverity::Error && d.code.as_str() == proposal.code && !d.message.trim().is_empty() && d.message.contains(&proposal.text));
        if !matched { failures.push(format!("{}: expected {} containing {:?}; got {:?}", proposal.name, proposal.code, proposal.text, diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).map(|d| (d.code.as_str(), d.message.as_str())).collect::<Vec<_>>())); }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
