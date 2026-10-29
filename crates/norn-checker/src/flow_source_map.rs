//! Admission checks for the non-executable authoring flow projection.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, FlowSourceMap, FlowSourceMapArtifactRef, FlowSourceMapEdge,
    FlowSourceMapEdgeKind, JdmGraph, JdmNode,
};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, EffectiveCatalog, Span,
};

/// Diagnostic codes emitted by flow source-map admission.
pub mod codes {
    /// Two flows use the same flow id.
    pub const DUPLICATE_FLOW: &str = "artifact_set.flow_source_map.duplicate_flow";
    /// Two steps in one flow use the same authoring id.
    pub const DUPLICATE_STEP: &str = "artifact_set.flow_source_map.duplicate_step";
    /// A map reference or mapping list is duplicated or inconsistent.
    pub const INVALID_MAPPING: &str = "artifact_set.flow_source_map.invalid_mapping";
    /// A referenced lowered artifact is absent or has a different version.
    pub const MISSING_ARTIFACT: &str = "artifact_set.flow_source_map.missing_artifact";
    /// An artifact reference digest does not match its lowered artifact.
    pub const STALE_ARTIFACT: &str = "artifact_set.flow_source_map.stale_artifact";
    /// A mapped state is not declared by a mapped entity artifact.
    pub const MISSING_STATE: &str = "artifact_set.flow_source_map.missing_state";
    /// A mapped row is not declared by a mapped `RuleIR` decision table.
    pub const MISSING_DECISION_ROW: &str = "artifact_set.flow_source_map.missing_decision_row";
    /// An edge endpoint does not identify a step in the same flow.
    pub const INVALID_EDGE: &str = "artifact_set.flow_source_map.invalid_edge";
    /// A source location has an empty file or a zero line/column.
    pub const INVALID_LOCATION: &str = "artifact_set.flow_source_map.invalid_location";
}

/// Validate a flow source map against the complete candidate and installed-base set.
///
/// The indexes make work linear in the artifacts and sidecar entries, apart
/// from ordered-map/set lookups. No sidecar field is interpreted as executable
/// input.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one indexed admission pass reports every flow mapping failure without rehashing artifacts"
)]
pub fn validate_flow_source_map(
    source_map: &FlowSourceMap,
    candidates: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let all_artifacts = base
        .iter()
        .chain(candidates)
        .map(|artifact| {
            (
                artifact.artifact_id.as_str(),
                (
                    artifact,
                    crate::receipt::ArtifactRef::of(artifact).map_err(|error| error.to_string()),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let artifact_values = base.iter().chain(candidates).cloned().collect::<Vec<_>>();
    let catalog = EffectiveCatalog::from_artifacts(&artifact_values);

    let mut artifacts_by_state = BTreeMap::<String, BTreeSet<String>>::new();
    let mut artifacts_by_row = BTreeMap::<String, BTreeSet<String>>::new();
    for artifact in &artifact_values {
        if let Some(Ok(object_type)) = catalog.object_type(&artifact.artifact_id) {
            let states = object_type
                .lifecycles
                .iter()
                .flat_map(|lifecycle| lifecycle.states.iter().cloned())
                .collect::<BTreeSet<_>>();
            for state in &states {
                artifacts_by_state
                    .entry(state.clone())
                    .or_default()
                    .insert(artifact.artifact_id.clone());
            }
        }
        let ArtifactPayload::Rule(payload) = &artifact.payload else {
            continue;
        };
        let Some(graph) = payload.graph.as_value() else {
            continue;
        };
        // Payload fields retain their raw wire value so ordinary artifact
        // validation can report malformed graphs. Index only a decoded graph.
        let Ok(graph) = serde_json::to_value(graph).and_then(serde_json::from_value::<JdmGraph>)
        else {
            continue;
        };
        let rows = graph
            .nodes
            .iter()
            .filter_map(|node| match node {
                JdmNode::DecisionTableNode { content, .. } => Some(&content.rules),
                JdmNode::InputNode { .. }
                | JdmNode::OutputNode { .. }
                | JdmNode::ExpressionNode { .. }
                | JdmNode::SwitchNode { .. }
                | JdmNode::DecisionNode { .. }
                | JdmNode::FunctionNode { .. }
                | JdmNode::CustomNode { .. } => None,
            })
            .flatten()
            .map(|row| row.id.clone())
            .collect::<BTreeSet<_>>();
        for row in &rows {
            artifacts_by_row
                .entry(row.clone())
                .or_default()
                .insert(artifact.artifact_id.clone());
        }
    }

    let mut flow_ids = BTreeSet::new();
    for (flow_index, flow) in source_map.flows.iter().enumerate() {
        let flow_pointer = format!("/flowSourceMap/flows/{flow_index}");
        if !flow_ids.insert(flow.flow_id.as_str()) {
            diagnostics.push(finding(
                codes::DUPLICATE_FLOW,
                format!("flow id {:?} appears more than once", flow.flow_id),
                format!("{flow_pointer}/flowId"),
                None,
            ));
        }
        if flow.flow_id.trim().is_empty()
            || flow.flow_version.trim().is_empty()
            || flow.source_path.trim().is_empty()
        {
            diagnostics.push(finding(
                codes::INVALID_MAPPING,
                "flow id, version, and source path must be non-empty".to_owned(),
                flow_pointer.clone(),
                None,
            ));
        }
        if flow.steps.is_empty() {
            diagnostics.push(finding(
                codes::INVALID_MAPPING,
                "a flow source map must contain at least one authoring step".to_owned(),
                format!("{flow_pointer}/steps"),
                None,
            ));
        }
        if flow.artifact_refs.is_empty() {
            diagnostics.push(finding(
                codes::INVALID_MAPPING,
                "a flow source map must bind at least one lowered artifact".to_owned(),
                format!("{flow_pointer}/artifactRefs"),
                None,
            ));
        }

        let mut refs = BTreeMap::<&str, &FlowSourceMapArtifactRef>::new();
        for (ref_index, artifact_ref) in flow.artifact_refs.iter().enumerate() {
            let pointer = format!("{flow_pointer}/artifactRefs/{ref_index}");
            if refs
                .insert(artifact_ref.artifact_id.as_str(), artifact_ref)
                .is_some()
            {
                diagnostics.push(finding(
                    codes::INVALID_MAPPING,
                    format!(
                        "artifact id {:?} is listed more than once",
                        artifact_ref.artifact_id
                    ),
                    pointer.clone(),
                    Some(artifact_ref.artifact_id.clone()),
                ));
                continue;
            }
            let Some((artifact, hash)) = all_artifacts.get(artifact_ref.artifact_id.as_str())
            else {
                diagnostics.push(finding(
                    codes::MISSING_ARTIFACT,
                    format!(
                        "flow artifact {:?} version {:?} is not in the admitted set",
                        artifact_ref.artifact_id, artifact_ref.version
                    ),
                    pointer,
                    Some(artifact_ref.artifact_id.clone()),
                ));
                continue;
            };
            if artifact.version != artifact_ref.version {
                diagnostics.push(finding(
                    codes::MISSING_ARTIFACT,
                    format!(
                        "flow artifact {:?} version {:?} is not admitted; found {:?}",
                        artifact_ref.artifact_id, artifact_ref.version, artifact.version
                    ),
                    format!("{pointer}/version"),
                    Some(artifact_ref.artifact_id.clone()),
                ));
                continue;
            }
            match hash {
                Ok(actual) if actual.sha256 == artifact_ref.sha256 => {}
                Ok(_) => diagnostics.push(finding(
                    codes::STALE_ARTIFACT,
                    format!(
                        "flow artifact {:?} hash does not match its admitted version",
                        artifact_ref.artifact_id
                    ),
                    format!("{pointer}/sha256"),
                    Some(artifact_ref.artifact_id.clone()),
                )),
                Err(error) => diagnostics.push(finding(
                    codes::STALE_ARTIFACT,
                    format!(
                        "flow artifact {:?} cannot be hashed: {error}",
                        artifact_ref.artifact_id
                    ),
                    format!("{pointer}/sha256"),
                    Some(artifact_ref.artifact_id.clone()),
                )),
            }
        }

        let mut step_ids = BTreeSet::new();
        let mut mapped_artifacts = BTreeSet::new();
        for (step_index, step) in flow.steps.iter().enumerate() {
            let step_pointer = format!("{flow_pointer}/steps/{step_index}");
            if step.id.trim().is_empty() || !step_ids.insert(step.id.as_str()) {
                diagnostics.push(finding(
                    codes::DUPLICATE_STEP,
                    format!("step id {:?} is empty or appears more than once", step.id),
                    format!("{step_pointer}/id"),
                    None,
                ));
            }
            if let Some(at) = &step.at
                && (at.file.trim().is_empty() || at.line == 0 || at.column == Some(0))
            {
                diagnostics.push(finding(
                    codes::INVALID_LOCATION,
                    "source location requires a file and one-based line and column numbers"
                        .to_owned(),
                    format!("{step_pointer}/at"),
                    None,
                ));
            }
            check_unique_strings(
                &step.artifact_ids,
                &format!("{step_pointer}/artifactIds"),
                "artifact id",
                &mut diagnostics,
            );
            check_unique_strings(
                &step.state_ids,
                &format!("{step_pointer}/stateIds"),
                "state id",
                &mut diagnostics,
            );
            check_unique_strings(
                &step.decision_row_ids,
                &format!("{step_pointer}/decisionRowIds"),
                "decision row id",
                &mut diagnostics,
            );
            let step_artifact_ids = step
                .artifact_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            let mut produced_artifact_ids = BTreeSet::new();

            for (id_index, artifact_id) in step.artifact_ids.iter().enumerate() {
                mapped_artifacts.insert(artifact_id.as_str());
                if !refs.contains_key(artifact_id.as_str()) {
                    diagnostics.push(finding(
                        codes::INVALID_MAPPING,
                        format!(
                            "step references artifact {artifact_id:?} absent from flow artifactRefs"
                        ),
                        format!("{step_pointer}/artifactIds/{id_index}"),
                        Some(artifact_id.clone()),
                    ));
                }
            }
            for (state_index, state_id) in step.state_ids.iter().enumerate() {
                let owners = artifacts_by_state.get(state_id);
                let exists = owners.is_some_and(|owners| {
                    owners
                        .iter()
                        .any(|artifact_id| step_artifact_ids.contains(artifact_id.as_str()))
                });
                if !exists {
                    diagnostics.push(finding(
                        codes::MISSING_STATE,
                        format!(
                            "state id {state_id:?} is not declared by a mapped entity artifact"
                        ),
                        format!("{step_pointer}/stateIds/{state_index}"),
                        None,
                    ));
                } else if let Some(owners) = owners {
                    produced_artifact_ids.extend(
                        owners
                            .iter()
                            .filter(|artifact_id| step_artifact_ids.contains(artifact_id.as_str()))
                            .map(String::as_str),
                    );
                }
            }
            for (row_index, row_id) in step.decision_row_ids.iter().enumerate() {
                let owners = artifacts_by_row.get(row_id);
                let exists = owners.is_some_and(|owners| {
                    owners
                        .iter()
                        .any(|artifact_id| step_artifact_ids.contains(artifact_id.as_str()))
                });
                if !exists {
                    diagnostics.push(finding(
                        codes::MISSING_DECISION_ROW,
                        format!(
                            "decision row id {row_id:?} is not declared by a mapped RuleIR artifact"
                        ),
                        format!("{step_pointer}/decisionRowIds/{row_index}"),
                        None,
                    ));
                } else if let Some(owners) = owners {
                    produced_artifact_ids.extend(
                        owners
                            .iter()
                            .filter(|artifact_id| step_artifact_ids.contains(artifact_id.as_str()))
                            .map(String::as_str),
                    );
                }
            }
            if produced_artifact_ids != step_artifact_ids {
                diagnostics.push(finding(
                    codes::INVALID_MAPPING,
                    "step artifact ids must be exactly the artifacts that declare its mapped states or decision rows".to_owned(),
                    format!("{step_pointer}/artifactIds"),
                    None,
                ));
            }
        }
        for (edge_index, edge) in flow.edges.iter().enumerate() {
            if !step_ids.contains(edge.from.as_str()) || !step_ids.contains(edge.to.as_str()) {
                diagnostics.push(finding(
                    codes::INVALID_EDGE,
                    format!(
                        "edge {:?} -> {:?} must reference steps in the same flow",
                        edge.from, edge.to
                    ),
                    format!("{flow_pointer}/edges/{edge_index}"),
                    None,
                ));
            }
        }
        check_containment_edges(&step_ids, &flow.edges, &flow_pointer, &mut diagnostics);
        for (ref_index, artifact_ref) in flow.artifact_refs.iter().enumerate() {
            if !mapped_artifacts.contains(artifact_ref.artifact_id.as_str()) {
                diagnostics.push(finding(
                    codes::INVALID_MAPPING,
                    format!(
                        "artifact {:?} is not mapped from any flow step",
                        artifact_ref.artifact_id
                    ),
                    format!("{flow_pointer}/artifactRefs/{ref_index}"),
                    Some(artifact_ref.artifact_id.clone()),
                ));
            }
        }
    }
    diagnostics
}

fn check_containment_edges(
    step_ids: &BTreeSet<&str>,
    edges: &[FlowSourceMapEdge],
    flow_pointer: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut parents = BTreeMap::new();
    for (index, edge) in edges.iter().enumerate() {
        if edge.kind != FlowSourceMapEdgeKind::Contains
            || !step_ids.contains(edge.from.as_str())
            || !step_ids.contains(edge.to.as_str())
        {
            continue;
        }
        if parents
            .insert(edge.to.as_str(), edge.from.as_str())
            .is_some()
        {
            diagnostics.push(finding(
                codes::INVALID_EDGE,
                format!(
                    "step {:?} must have only one direct containment parent",
                    edge.to
                ),
                format!("{flow_pointer}/edges/{index}"),
                None,
            ));
        }
    }
    // Completed ancestry paths are reused so a deep or wide submitted graph
    // is visited once instead of walking every ancestor for every step.
    let mut completed = BTreeSet::new();
    for start in parents.keys().copied() {
        let mut path = BTreeSet::new();
        let mut current = start;
        while let Some(parent) = parents.get(current) {
            if completed.contains(current) {
                break;
            }
            if !path.insert(current) {
                diagnostics.push(finding(
                    codes::INVALID_EDGE,
                    format!("containment ancestry at step {current:?} must not contain a cycle"),
                    format!("{flow_pointer}/edges"),
                    None,
                ));
                break;
            }
            current = parent;
        }
        completed.extend(path);
    }
}

fn check_unique_strings(
    values: &[String],
    pointer: &str,
    what: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut unique = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if !unique.insert(value.as_str()) {
            diagnostics.push(finding(
                codes::INVALID_MAPPING,
                format!("{what} {value:?} appears more than once"),
                format!("{pointer}/{index}"),
                None,
            ));
        }
    }
}

fn finding(
    code: &str,
    message: String,
    pointer: impl Into<String>,
    artifact_id: Option<String>,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id,
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
