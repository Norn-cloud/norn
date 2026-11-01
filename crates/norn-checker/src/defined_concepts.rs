//! GRAPH-DAG-1: classification dependencies retain exact artifact versions.
use crate::graph::{cycle_groups, topological_order};
use crate::{Diagnostic, DiagnosticCode, Span};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, DefinedConceptRoot, DiagnosticSeverity,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn decode(payload: &ArtifactPayload) -> Result<DefinedConceptRoot, String> {
    let bytes = norn_canonical::canonical_json_of(&payload.to_json_value())
        .map_err(|error| error.to_string())?;
    let root: DefinedConceptRoot =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if norn_canonical::canonical_json_of(&root).map_err(|error| error.to_string())? != bytes {
        return Err("payload must use the declared object representation".into());
    }
    Ok(root)
}

/// GRAPH-DAG-1 refuses actual classification cycles, including retained dependencies.
/// Ambiguous identities remain resolver errors; they never select a graph vertex.
pub(crate) fn check_cycles(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let nodes: Vec<_> = artifacts
        .iter()
        .chain(base)
        .enumerate()
        .filter(|(_, artifact)| artifact.kind() == ArtifactKind::DefinedConcept)
        .filter_map(|(index, artifact)| {
            decode(&artifact.payload)
                .ok()
                .map(|root| (artifact, root, index < artifacts.len()))
        })
        .collect();
    let mut identities = BTreeMap::new();
    for (index, (artifact, _, _)) in nodes.iter().enumerate() {
        identities
            .entry((&artifact.artifact_id, &artifact.version))
            .and_modify(|target| *target = None)
            .or_insert(Some(index));
    }
    let edges: BTreeMap<usize, BTreeSet<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(index, (_, root, _))| {
            (
                index,
                root.dependencies
                    .iter()
                    .filter_map(|dependency| {
                        identities
                            .get(&(&dependency.artifact_id, &dependency.version))
                            .copied()
                            .flatten()
                    })
                    .collect(),
            )
        })
        .collect();
    let (_, blocked) = topological_order(&edges, |index| index);
    cycle_groups(&edges, &blocked)
        .into_iter()
        .filter_map(|group| {
            let (artifact, _, _) = group
                .iter()
                .filter_map(|index| nodes.get(*index))
                .filter(|(_, _, candidate)| *candidate)
                .min_by_key(|(artifact, _, _)| (&artifact.artifact_id, &artifact.version))?;
            Some(Diagnostic {
                code: DiagnosticCode::new("core.graph.cycle"),
                severity: DiagnosticSeverity::Error,
                message: "classification dependencies must be acyclic".into(),
                span: Some(Span::pointer("/payload/dependencies")),
                artifact_id: Some(artifact.artifact_id.clone()),
                remediation: None,
            })
        })
        .collect()
}
