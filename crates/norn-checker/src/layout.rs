//! Lossless migration and reference checking for versioned editor layout.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Diagnostic, DiagnosticCode, Span};
use norn_ir::{
    ArtifactEnvelope, ArtifactLayout, ArtifactPayload, CanvasPosition, ColumnLayout, Decimal,
    DiagnosticRemediation, DiagnosticSeverity, EdgeLayout, JsonValue, LayoutField, LayoutSidecar,
    LayoutViewport, NodeLayout, PayloadField, RULE_IR_V2_VERSION, RuntimeObject, RuntimeValue,
};

/// One artifact prepared for admission from exact runtime JSON.
///
/// `source` is the semantic document after inline editor fields have been
/// extracted. `layout` retains those fields for the caller's sidecar output.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedArtifact {
    /// The decoded semantic envelope.
    pub artifact: ArtifactEnvelope,
    /// The exact semantic source document after extraction.
    pub source: RuntimeValue,
    /// Editor metadata extracted from the original source.
    pub layout: LayoutSidecar,
}

/// Extract inline layout from exact source JSON and decode the semantic envelope.
///
/// This is the raw admission preparation boundary. It consumes the source and
/// returns its normalized exact value beside the decoded envelope and layout.
///
/// # Errors
/// Returns extraction or envelope decode diagnostics.
pub fn prepare_artifact_source(source: RuntimeValue) -> Result<PreparedArtifact, Vec<Diagnostic>> {
    let mut semantic_source = source;
    let layout = extract_inline_layout(&mut semantic_source)?;
    let bytes = semantic_source.canonical_json().map_err(|error| {
        vec![invalid_source_diagnostic(
            None,
            &format!("normalized artifact source cannot be canonicalized: {error}"),
        )]
    })?;
    let document = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
        vec![invalid_source_diagnostic(
            None,
            &format!("normalized artifact source cannot be decoded: {error}"),
        )]
    })?;
    let artifact = crate::decode_artifact_json(&document).map_err(|legacy| {
        legacy
            .into_iter()
            .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, &document))
            .collect::<Vec<_>>()
    })?;
    Ok(PreparedArtifact {
        artifact,
        source: semantic_source,
        layout,
    })
}

/// Normalize one typed envelope for checker admission while retaining any
/// lenient root diagnostics. Only the raw graph field is replaced: serializing
/// the whole envelope would discard the payload root's recorded unknown keys.
pub(crate) fn normalize_artifact(
    artifact: &ArtifactEnvelope,
) -> Result<(ArtifactEnvelope, LayoutSidecar), Vec<Diagnostic>> {
    let ArtifactPayload::Rule(rule) = &artifact.payload else {
        return Ok((artifact.clone(), LayoutSidecar::default()));
    };
    let Some(rule_version) = rule.rule_version.as_value() else {
        return Ok((artifact.clone(), LayoutSidecar::default()));
    };
    if rule_version.as_string() != Some(RULE_IR_V2_VERSION) {
        return Ok((artifact.clone(), LayoutSidecar::default()));
    }
    let Some(graph) = rule.graph.as_value() else {
        return Ok((artifact.clone(), LayoutSidecar::default()));
    };
    let graph = RuntimeValue::from_ir(graph).map_err(|error| {
        vec![invalid_source_diagnostic(
            Some(artifact.artifact_id.clone()),
            &format!("rule graph cannot be represented as runtime JSON: {error}"),
        )]
    })?;
    let mut payload = RuntimeObject::new();
    payload.insert(
        "ruleVersion".to_owned(),
        RuntimeValue::String(RULE_IR_V2_VERSION.to_owned()),
    );
    payload.insert("graph".to_owned(), graph);
    let mut envelope = RuntimeObject::new();
    envelope.insert(
        "artifactId".to_owned(),
        RuntimeValue::String(artifact.artifact_id.clone()),
    );
    envelope.insert("kind".to_owned(), RuntimeValue::String("rule".to_owned()));
    envelope.insert("payload".to_owned(), RuntimeValue::Object(payload));
    let mut source = RuntimeValue::Object(envelope);
    let layout = extract_inline_layout(&mut source)?;
    let normalized_graph = source
        .get("payload")
        .and_then(|value| value.get("graph"))
        .ok_or_else(|| {
            vec![invalid_source_diagnostic(
                Some(artifact.artifact_id.clone()),
                "normalized rule graph is missing",
            )]
        })?;
    let graph: JsonValue = norn_ir::runtime::decode(normalized_graph).map_err(|error| {
        vec![invalid_source_diagnostic(
            Some(artifact.artifact_id.clone()),
            &format!("normalized rule graph cannot be decoded: {error}"),
        )]
    })?;
    let mut normalized = artifact.clone();
    if let ArtifactPayload::Rule(rule) = &mut normalized.payload {
        rule.graph = PayloadField::present(graph);
    }
    Ok((normalized, layout))
}

fn invalid_source_diagnostic(artifact_id: Option<String>, message: &str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new("ir.validation.invalid_envelope"),
        severity: DiagnosticSeverity::Error,
        message: message.to_owned(),
        span: Some(Span::pointer("/payload/graph")),
        artifact_id,
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// Extract legacy inline editor fields from one raw artifact envelope.
///
/// `artifact` is the exact runtime JSON value of the full envelope. The
/// mutation is atomic: malformed editor metadata returns an error and leaves
/// the source untouched, so the caller can reject it through normal admission.
/// Non-rule artifacts and rules outside `RuleIR` v2 are returned unchanged with
/// an empty sidecar.
///
/// # Errors
/// Malformed metadata or duplicate stable ids leave the input untouched.
pub fn extract_inline_layout(
    artifact: &mut RuntimeValue,
) -> Result<LayoutSidecar, Vec<Diagnostic>> {
    let mut candidate = artifact.clone();
    match extract_inline_layout_in_place(&mut candidate) {
        Ok(layout) => {
            *artifact = candidate;
            Ok(layout)
        }
        Err(diagnostic) => Err(vec![diagnostic]),
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::result_large_err,
    reason = "one atomic upgrade maps each supported legacy editor field"
)]
fn extract_inline_layout_in_place(
    artifact: &mut RuntimeValue,
) -> Result<LayoutSidecar, Diagnostic> {
    let Some(envelope) = artifact.as_object_mut() else {
        return Ok(LayoutSidecar::default());
    };
    if envelope.get("kind").and_then(RuntimeValue::as_str) != Some("rule") {
        return Ok(LayoutSidecar::default());
    }
    let artifact_id = envelope
        .get("artifactId")
        .and_then(RuntimeValue::as_str)
        .map(str::to_owned);
    let Some(artifact_id) = artifact_id else {
        return Ok(LayoutSidecar::default());
    };
    let Some(payload) = envelope
        .get_mut("payload")
        .and_then(RuntimeValue::as_object_mut)
    else {
        return Ok(LayoutSidecar::default());
    };
    if payload.get("ruleVersion").and_then(RuntimeValue::as_str) != Some(RULE_IR_V2_VERSION) {
        return Ok(LayoutSidecar::default());
    }
    let Some(graph) = payload
        .get_mut("graph")
        .and_then(RuntimeValue::as_object_mut)
    else {
        return Ok(LayoutSidecar::default());
    };

    let mut layout = ArtifactLayout::default();
    if let Some(settings) = graph.remove("settings") {
        layout.settings = LayoutField::Present(settings);
    }
    if let Some(viewport) = graph.remove("viewport") {
        layout.viewport = decode_optional(
            &viewport,
            &artifact_id,
            &["payload", "graph", "viewport"],
            norn_ir::runtime::decode::<Option<LayoutViewport>>,
        )?;
    }

    let mut seen_nodes = BTreeSet::new();
    if let Some(RuntimeValue::Array(nodes)) = graph.get_mut("nodes") {
        for (index, node) in nodes.iter_mut().enumerate() {
            let Some(node_object) = node.as_object_mut() else {
                continue;
            };
            let node_id = node_object
                .get("id")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned);
            let Some(node_id) = node_id else {
                continue;
            };
            if !seen_nodes.insert(node_id.clone()) {
                return Err(duplicate_jdm_id(
                    &artifact_id,
                    &node_id,
                    "node",
                    ["payload", "graph", "nodes", &index.to_string(), "id"],
                ));
            }
            let mut node_layout = NodeLayout::default();
            let position_path = vec![
                "payload".to_owned(),
                "graph".to_owned(),
                "nodes".to_owned(),
                index.to_string(),
                "position".to_owned(),
            ];
            node_layout.position = take_optional(
                node_object,
                "position",
                &artifact_id,
                &position_path,
                norn_ir::runtime::decode::<Option<CanvasPosition>>,
            )?;
            let description_path = vec![
                "payload".to_owned(),
                "graph".to_owned(),
                "nodes".to_owned(),
                index.to_string(),
                "description".to_owned(),
            ];
            node_layout.description = take_optional(
                node_object,
                "description",
                &artifact_id,
                &description_path,
                norn_ir::runtime::decode::<Option<String>>,
            )?;
            let folded_path = vec![
                "payload".to_owned(),
                "graph".to_owned(),
                "nodes".to_owned(),
                index.to_string(),
                "folded".to_owned(),
            ];
            node_layout.folded = take_optional(
                node_object,
                "folded",
                &artifact_id,
                &folded_path,
                norn_ir::runtime::decode::<Option<bool>>,
            )?;

            if let Some(content) = node_object
                .get_mut("content")
                .and_then(RuntimeValue::as_object_mut)
            {
                extract_rows_and_columns(content, &artifact_id, index, &mut node_layout)?;
            }
            if !node_layout_is_empty(&node_layout) {
                layout.nodes.insert(node_id, node_layout);
            }
        }
    }

    let mut seen_edges = BTreeSet::new();
    if let Some(RuntimeValue::Array(edges)) = graph.get_mut("edges") {
        for (index, edge) in edges.iter_mut().enumerate() {
            let Some(edge_object) = edge.as_object_mut() else {
                continue;
            };
            let edge_id = edge_object
                .get("id")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned);
            let Some(edge_id) = edge_id else {
                continue;
            };
            if !seen_edges.insert(edge_id.clone()) {
                return Err(duplicate_jdm_id(
                    &artifact_id,
                    &edge_id,
                    "edge",
                    ["payload", "graph", "edges", &index.to_string(), "id"],
                ));
            }
            let mut edge_layout = EdgeLayout::default();
            for (member, field) in [
                ("name", &mut edge_layout.name),
                ("targetHandle", &mut edge_layout.target_handle),
                ("type", &mut edge_layout.edge_type),
            ] {
                let path = [
                    "payload".to_owned(),
                    "graph".to_owned(),
                    "edges".to_owned(),
                    index.to_string(),
                    member.to_owned(),
                ];
                *field = take_optional(
                    edge_object,
                    member,
                    &artifact_id,
                    &path,
                    norn_ir::runtime::decode::<Option<String>>,
                )?;
            }
            if !edge_layout.name.is_absent()
                || !edge_layout.target_handle.is_absent()
                || !edge_layout.edge_type.is_absent()
            {
                layout.edges.insert(edge_id, edge_layout);
            }
        }
    }

    let mut sidecar = LayoutSidecar::default();
    if !artifact_layout_is_empty(&layout) {
        sidecar.artifacts.insert(artifact_id, layout);
    }
    Ok(sidecar)
}

#[allow(
    clippy::result_large_err,
    reason = "upgrade errors retain the structured checker diagnostic"
)]
fn extract_rows_and_columns(
    content: &mut RuntimeObject,
    artifact_id: &str,
    node_index: usize,
    node_layout: &mut NodeLayout,
) -> Result<(), Diagnostic> {
    let mut seen_rows = BTreeSet::new();
    if let Some(RuntimeValue::Array(rows)) = content.get_mut("rules") {
        for (index, row) in rows.iter_mut().enumerate() {
            let Some(row_object) = row.as_object_mut() else {
                continue;
            };
            let row_id = row_object
                .get("_id")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned);
            let Some(row_id) = row_id else {
                continue;
            };
            if !seen_rows.insert(row_id.clone()) {
                return Err(duplicate_jdm_id(
                    artifact_id,
                    &row_id,
                    "row",
                    [
                        "payload",
                        "graph",
                        "nodes",
                        &node_index.to_string(),
                        "content",
                        "rules",
                        &index.to_string(),
                        "_id",
                    ],
                ));
            }
            // Row layout is sidecar-only. Do not migrate any unprefixed row
            // member: every key other than the stable `_id` and `_description`
            // can be a declared semantic expression-cell id.
        }
    }

    let mut seen_columns = BTreeSet::new();
    for collection in ["inputs", "outputs"] {
        if let Some(RuntimeValue::Array(columns)) = content.get_mut(collection) {
            for (index, column) in columns.iter_mut().enumerate() {
                let Some(column_object) = column.as_object_mut() else {
                    continue;
                };
                let column_id = column_object
                    .get("id")
                    .and_then(RuntimeValue::as_str)
                    .map(str::to_owned);
                let Some(column_id) = column_id else {
                    continue;
                };
                if !seen_columns.insert(column_id.clone()) {
                    return Err(duplicate_jdm_id(
                        artifact_id,
                        &column_id,
                        "column",
                        [
                            "payload",
                            "graph",
                            "nodes",
                            &node_index.to_string(),
                            "content",
                            collection,
                            &index.to_string(),
                            "id",
                        ],
                    ));
                }
                let path = [
                    "payload".to_owned(),
                    "graph".to_owned(),
                    "nodes".to_owned(),
                    node_index.to_string(),
                    "content".to_owned(),
                    collection.to_owned(),
                    index.to_string(),
                    "width".to_owned(),
                ];
                let width = take_optional(
                    column_object,
                    "width",
                    artifact_id,
                    &path,
                    norn_ir::runtime::decode::<Option<Decimal>>,
                )?;
                if !width.is_absent() {
                    node_layout
                        .columns
                        .insert(column_id, ColumnLayout { width });
                }
            }
        }
    }
    Ok(())
}

fn duplicate_jdm_id<const N: usize>(
    artifact_id: &str,
    id: &str,
    kind: &str,
    path: [&str; N],
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new("rule.validation.jdm_duplicate_id"),
        severity: DiagnosticSeverity::Error,
        message: format!("{kind} id {id:?} is not unique"),
        span: Some(Span::from_segments(path)),
        artifact_id: Some(artifact_id.to_owned()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "upgrade errors retain the structured checker diagnostic"
)]
fn take_optional<T>(
    object: &mut RuntimeObject,
    member: &str,
    artifact_id: &str,
    path: &[String],
    decode: impl FnOnce(&RuntimeValue) -> Result<Option<T>, String>,
) -> Result<LayoutField<Option<T>>, Diagnostic> {
    object
        .remove(member)
        .map(|value| decode_optional(&value, artifact_id, path, decode))
        .transpose()
        .map(|field| field.unwrap_or(LayoutField::Absent))
}

#[allow(
    clippy::result_large_err,
    reason = "upgrade errors retain the structured checker diagnostic"
)]
fn decode_optional<T>(
    value: &RuntimeValue,
    artifact_id: &str,
    path: &[impl AsRef<str>],
    decode: impl FnOnce(&RuntimeValue) -> Result<Option<T>, String>,
) -> Result<LayoutField<Option<T>>, Diagnostic> {
    decode(value)
        .map(LayoutField::Present)
        .map_err(|detail| invalid_inline_field(artifact_id, path, &detail))
}

fn invalid_inline_field(artifact_id: &str, path: &[impl AsRef<str>], detail: &str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new("layout.validation.invalid_inline_field"),
        severity: DiagnosticSeverity::Error,
        message: format!("legacy layout field is invalid: {detail}"),
        span: Some(Span::from_segments(path.iter())),
        artifact_id: Some(artifact_id.to_owned()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

fn node_layout_is_empty(layout: &NodeLayout) -> bool {
    layout.position.is_absent()
        && layout.description.is_absent()
        && layout.folded.is_absent()
        && layout.rows.is_empty()
        && layout.columns.is_empty()
}

fn artifact_layout_is_empty(layout: &ArtifactLayout) -> bool {
    layout.nodes.is_empty()
        && layout.edges.is_empty()
        && layout.viewport.is_absent()
        && layout.settings.is_absent()
}

fn edge_layout_is_empty(layout: &EdgeLayout) -> bool {
    layout.name.is_absent() && layout.target_handle.is_absent() && layout.edge_type.is_absent()
}

/// Restore one artifact's sidecar state into a raw envelope.
///
/// This is the inverse of [`extract_inline_layout`] for fields that originated
/// inline. Values that were explicitly `null` are restored as `null`; absent
/// sidecar fields remain absent.
///
/// # Errors
/// An exact editor value cannot be encoded in the runtime JSON profile.
#[allow(
    clippy::too_many_lines,
    reason = "the inverse upgrade restores each supported editor field by stable id"
)]
pub fn hydrate_inline_layout(
    artifact: &mut RuntimeValue,
    sidecar: &LayoutSidecar,
) -> Result<(), String> {
    let Some(envelope) = artifact.as_object_mut() else {
        return Ok(());
    };
    let Some(artifact_id) = envelope
        .get("artifactId")
        .and_then(RuntimeValue::as_str)
        .map(str::to_owned)
    else {
        return Ok(());
    };
    let Some(layout) = sidecar.artifacts.get(&artifact_id) else {
        return Ok(());
    };
    let Some(graph) = envelope
        .get_mut("payload")
        .and_then(RuntimeValue::as_object_mut)
        .and_then(|payload| payload.get_mut("graph"))
        .and_then(RuntimeValue::as_object_mut)
    else {
        return Ok(());
    };

    restore_runtime_field(graph, "settings", &layout.settings);
    restore_optional_field(
        graph,
        "viewport",
        &layout.viewport,
        norn_ir::runtime::encode::<LayoutViewport>,
    )?;

    if let Some(RuntimeValue::Array(nodes)) = graph.get_mut("nodes") {
        for node in nodes {
            let Some(node_object) = node.as_object_mut() else {
                continue;
            };
            let Some(node_id) = node_object
                .get("id")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            let Some(node_layout) = layout.nodes.get(&node_id) else {
                continue;
            };
            restore_optional_field(
                node_object,
                "position",
                &node_layout.position,
                norn_ir::runtime::encode::<CanvasPosition>,
            )?;
            restore_optional_field(
                node_object,
                "description",
                &node_layout.description,
                norn_ir::runtime::encode::<String>,
            )?;
            restore_optional_field(
                node_object,
                "folded",
                &node_layout.folded,
                norn_ir::runtime::encode::<bool>,
            )?;
            if let Some(content) = node_object
                .get_mut("content")
                .and_then(RuntimeValue::as_object_mut)
            {
                for collection in ["inputs", "outputs"] {
                    if let Some(RuntimeValue::Array(columns)) = content.get_mut(collection) {
                        for column in columns {
                            let Some(column_object) = column.as_object_mut() else {
                                continue;
                            };
                            let Some(column_id) = column_object
                                .get("id")
                                .and_then(RuntimeValue::as_str)
                                .map(str::to_owned)
                            else {
                                continue;
                            };
                            if let Some(column_layout) = node_layout.columns.get(&column_id) {
                                restore_optional_field(
                                    column_object,
                                    "width",
                                    &column_layout.width,
                                    norn_ir::runtime::encode::<Decimal>,
                                )?;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(RuntimeValue::Array(edges)) = graph.get_mut("edges") {
        for edge in edges {
            let Some(edge_object) = edge.as_object_mut() else {
                continue;
            };
            let Some(edge_id) = edge_object
                .get("id")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            let Some(edge_layout) = layout.edges.get(&edge_id) else {
                continue;
            };
            restore_optional_field(
                edge_object,
                "name",
                &edge_layout.name,
                norn_ir::runtime::encode::<String>,
            )?;
            restore_optional_field(
                edge_object,
                "targetHandle",
                &edge_layout.target_handle,
                norn_ir::runtime::encode::<String>,
            )?;
            restore_optional_field(
                edge_object,
                "type",
                &edge_layout.edge_type,
                norn_ir::runtime::encode::<String>,
            )?;
        }
    }
    Ok(())
}

fn restore_runtime_field(
    object: &mut RuntimeObject,
    member: &str,
    field: &LayoutField<RuntimeValue>,
) {
    if let LayoutField::Present(value) = field {
        object.insert(member.to_owned(), value.clone());
    }
}

fn restore_optional_field<T>(
    object: &mut RuntimeObject,
    member: &str,
    field: &LayoutField<Option<T>>,
    encode: impl FnOnce(&T) -> Result<RuntimeValue, String>,
) -> Result<(), String> {
    match field {
        LayoutField::Absent => {}
        LayoutField::Present(None) => {
            object.insert(member.to_owned(), RuntimeValue::Null);
        }
        LayoutField::Present(Some(value)) => {
            object.insert(member.to_owned(), encode(value)?);
        }
    }
    Ok(())
}

/// Prune layout references that no longer exist in the semantic artifact set.
/// Every finding is a warning; orphaned editor state never blocks semantic
/// admission.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "artifact, node, edge, row and column pruning each retain their diagnostic paths"
)]
pub fn prune_layout(
    sidecar: &mut LayoutSidecar,
    artifacts: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let by_id: BTreeMap<&str, &ArtifactEnvelope> = artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect();
    let mut diagnostics = Vec::new();
    let orphaned_artifacts: Vec<String> = sidecar
        .artifacts
        .keys()
        .filter(|artifact_id| !by_id.contains_key(artifact_id.as_str()))
        .cloned()
        .collect();
    for artifact_id in orphaned_artifacts {
        sidecar.artifacts.remove(&artifact_id);
        diagnostics.push(orphaned_layout_diagnostic(
            &artifact_id,
            ["artifacts", artifact_id.as_str()],
            "artifact",
            &artifact_id,
        ));
    }

    for (artifact_id, layout) in &mut sidecar.artifacts {
        let Some(artifact) = by_id.get(artifact_id.as_str()) else {
            continue;
        };
        let valid = artifact_layout_ids(artifact);

        let orphaned_nodes: Vec<String> = layout
            .nodes
            .keys()
            .filter(|node_id| !valid.nodes.contains(node_id.as_str()))
            .cloned()
            .collect();
        for node_id in orphaned_nodes {
            layout.nodes.remove(&node_id);
            diagnostics.push(orphaned_layout_diagnostic(
                artifact_id,
                ["artifacts", artifact_id.as_str(), "nodes", node_id.as_str()],
                "node",
                &node_id,
            ));
        }

        for (node_id, node_layout) in &mut layout.nodes {
            let rows = valid
                .rows
                .get(node_id.as_str())
                .cloned()
                .unwrap_or_default();
            let orphaned_rows: Vec<String> = node_layout
                .rows
                .keys()
                .filter(|row_id| !rows.contains(row_id.as_str()))
                .cloned()
                .collect();
            for row_id in orphaned_rows {
                node_layout.rows.remove(&row_id);
                diagnostics.push(orphaned_layout_diagnostic(
                    artifact_id,
                    [
                        "artifacts",
                        artifact_id.as_str(),
                        "nodes",
                        node_id.as_str(),
                        "rows",
                        row_id.as_str(),
                    ],
                    "row",
                    &row_id,
                ));
            }

            let columns = valid
                .columns
                .get(node_id.as_str())
                .cloned()
                .unwrap_or_default();
            let orphaned_columns: Vec<String> = node_layout
                .columns
                .keys()
                .filter(|column_id| !columns.contains(column_id.as_str()))
                .cloned()
                .collect();
            for column_id in orphaned_columns {
                node_layout.columns.remove(&column_id);
                diagnostics.push(orphaned_layout_diagnostic(
                    artifact_id,
                    [
                        "artifacts",
                        artifact_id.as_str(),
                        "nodes",
                        node_id.as_str(),
                        "columns",
                        column_id.as_str(),
                    ],
                    "column",
                    &column_id,
                ));
            }

            node_layout
                .rows
                .retain(|_, row_layout| !row_layout.folded.is_absent());
            node_layout
                .columns
                .retain(|_, column_layout| !column_layout.width.is_absent());
        }

        let orphaned_edges: Vec<String> = layout
            .edges
            .keys()
            .filter(|edge_id| !valid.edges.contains(edge_id.as_str()))
            .cloned()
            .collect();
        for edge_id in orphaned_edges {
            layout.edges.remove(&edge_id);
            diagnostics.push(orphaned_layout_diagnostic(
                artifact_id,
                ["artifacts", artifact_id.as_str(), "edges", edge_id.as_str()],
                "edge",
                &edge_id,
            ));
        }

        let empty_edges: Vec<String> = layout
            .edges
            .iter()
            .filter(|(_, edge_layout)| edge_layout_is_empty(edge_layout))
            .map(|(edge_id, _)| edge_id.clone())
            .collect();
        for edge_id in empty_edges {
            layout.edges.remove(&edge_id);
        }

        let empty_nodes: Vec<String> = layout
            .nodes
            .iter()
            .filter(|(_, node_layout)| node_layout_is_empty(node_layout))
            .map(|(node_id, _)| node_id.clone())
            .collect();
        for node_id in empty_nodes {
            layout.nodes.remove(&node_id);
        }
    }

    let empty_artifacts: Vec<String> = sidecar
        .artifacts
        .iter()
        .filter(|(_, layout)| artifact_layout_is_empty(layout))
        .map(|(artifact_id, _)| artifact_id.clone())
        .collect();
    for artifact_id in empty_artifacts {
        sidecar.artifacts.remove(&artifact_id);
    }
    diagnostics
}

#[derive(Default)]
struct LayoutIds {
    nodes: BTreeSet<String>,
    edges: BTreeSet<String>,
    rows: BTreeMap<String, BTreeSet<String>>,
    columns: BTreeMap<String, BTreeSet<String>>,
}

fn artifact_layout_ids(artifact: &ArtifactEnvelope) -> LayoutIds {
    match &artifact.payload {
        ArtifactPayload::Rule(payload) => payload
            .graph
            .as_value()
            .map(layout_ids_from_jdm)
            .unwrap_or_default(),
        ArtifactPayload::Widget(payload) => payload
            .ux
            .as_value()
            .map(layout_ids_from_widget)
            .unwrap_or_default(),
        ArtifactPayload::Entity(_)
        | ArtifactPayload::Relation(_)
        | ArtifactPayload::Action(_)
        | ArtifactPayload::Connector(_)
        | ArtifactPayload::Customization(_)
        | ArtifactPayload::Setting(_)
        | ArtifactPayload::BusinessPack(_)
        | ArtifactPayload::Policy(_)
        | ArtifactPayload::Derived(_)
        | ArtifactPayload::SourceSchema(_)
        | ArtifactPayload::Mapping(_)
        | ArtifactPayload::Law(_)
        | ArtifactPayload::Claim(_)
        | ArtifactPayload::Ruling(_) => LayoutIds::default(),
    }
}

fn layout_ids_from_jdm(graph: &JsonValue) -> LayoutIds {
    let mut ids = LayoutIds::default();
    let Some(graph) = graph.as_object() else {
        return ids;
    };
    if let Some(edges) = graph.get("edges").and_then(JsonValue::as_array) {
        for edge in edges {
            if let Some(edge_id) = object_member(edge, "id") {
                ids.edges.insert(edge_id.to_owned());
            }
        }
    }
    if let Some(nodes) = graph.get("nodes").and_then(JsonValue::as_array) {
        for node in nodes {
            let Some(node_id) = object_member(node, "id") else {
                continue;
            };
            ids.nodes.insert(node_id.to_owned());
            let Some(content) = node
                .as_object()
                .and_then(|object| object.get("content"))
                .and_then(JsonValue::as_object)
            else {
                continue;
            };
            if let Some(rows) = content.get("rules").and_then(JsonValue::as_array) {
                let row_ids = rows
                    .iter()
                    .filter_map(|row| object_member(row, "_id"))
                    .map(str::to_owned)
                    .collect();
                ids.rows.insert(node_id.to_owned(), row_ids);
            }
            for collection in ["inputs", "outputs"] {
                if let Some(columns) = content.get(collection).and_then(JsonValue::as_array) {
                    let column_ids = columns
                        .iter()
                        .filter_map(|column| object_member(column, "id"))
                        .map(str::to_owned)
                        .collect();
                    ids.columns.insert(node_id.to_owned(), column_ids);
                }
            }
        }
    }
    ids
}

fn layout_ids_from_widget(ux: &JsonValue) -> LayoutIds {
    let mut ids = LayoutIds::default();
    let Some(ux_object) = ux.as_object() else {
        return ids;
    };
    let mut field_ids: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    if let Some(fields) = ux_object.get("fields").and_then(JsonValue::as_array) {
        for field in fields {
            let Some(field_id) = object_member(field, "fieldId") else {
                continue;
            };
            let Some(property) = field
                .as_object()
                .and_then(|object| object.get("target"))
                .and_then(JsonValue::as_object)
                .filter(|target| {
                    target.get("kind").and_then(JsonValue::as_string) == Some("property")
                })
                .and_then(|target| target.get("property"))
                .and_then(JsonValue::as_object)
            else {
                continue;
            };
            let (Some(binding_id), Some(property_id)) = (
                property.get("bindingId").and_then(JsonValue::as_string),
                property.get("propertyId").and_then(JsonValue::as_string),
            ) else {
                continue;
            };
            field_ids
                .entry((binding_id.to_owned(), property_id.to_owned()))
                .or_default()
                .insert(field_id.to_owned());
        }
    }
    let Some(pages) = ux_object.get("pages").and_then(JsonValue::as_array) else {
        return ids;
    };
    for page in pages {
        if let Some(page_id) = object_member(page, "pageId") {
            ids.nodes.insert(page_id.to_owned());
        }
        if let Some(root) = page.as_object().and_then(|object| object.get("root")) {
            collect_widget_node_ids(root, &field_ids, &mut ids);
        }
    }
    ids
}

fn collect_widget_node_ids(
    node: &JsonValue,
    field_ids: &BTreeMap<(String, String), BTreeSet<String>>,
    ids: &mut LayoutIds,
) {
    let Some(object) = node.as_object() else {
        return;
    };
    let mut node_keys = Vec::new();
    for member in ["nodeId", "semanticId"] {
        if let Some(node_id) = object.get(member).and_then(JsonValue::as_string) {
            ids.nodes.insert(node_id.to_owned());
            node_keys.push(node_id.to_owned());
        }
    }
    if let Some(component) = object.get("component").and_then(JsonValue::as_object)
        && component.get("kind").and_then(JsonValue::as_string) == Some("list")
    {
        let column_ids = component
            .get("fields")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|field| {
                let object = field.as_object()?;
                let binding_id = object.get("bindingId")?.as_string()?;
                let property_id = object.get("propertyId")?.as_string()?;
                let key = (binding_id.to_owned(), property_id.to_owned());
                Some(field_ids.get(&key).cloned().unwrap_or_else(|| {
                    BTreeSet::from([widget_list_column_key(binding_id, property_id)])
                }))
            })
            .flatten()
            .collect::<BTreeSet<_>>();
        for node_key in node_keys {
            ids.columns
                .entry(node_key)
                .or_default()
                .extend(column_ids.iter().cloned());
        }
    }
    if let Some(children) = object.get("children").and_then(JsonValue::as_array) {
        for child in children {
            collect_widget_node_ids(child, field_ids, ids);
        }
    }
}

/// Stable sidecar column key for a Widget UX list field with no matching
/// `WidgetFieldPresentation`. The two ids are RFC 6901 escaped, so this
/// representation cannot confuse a binding/property pair with another pair.
#[must_use]
pub fn widget_list_column_key(binding_id: &str, property_id: &str) -> String {
    let escape = |segment: &str| segment.replace('~', "~0").replace('/', "~1");
    format!("$property/{}/{}", escape(binding_id), escape(property_id))
}

/// Report that retained or explicit sidecar layout won over conflicting
/// inline editor defaults during migration.
#[must_use]
pub fn layout_merge_conflict_diagnostic(artifact_id: &str, path: &str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new("layout.validation.merge_conflict"),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "retained or explicit layout value at {path} took precedence over conflicting inline layout"
        ),
        span: Some(Span::pointer(path.to_owned())),
        artifact_id: Some(artifact_id.to_owned()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

fn object_member<'a>(value: &'a JsonValue, member: &str) -> Option<&'a str> {
    value.as_object()?.get(member)?.as_string()
}

fn orphaned_layout_diagnostic<const N: usize>(
    artifact_id: &str,
    path: [&str; N],
    kind: &str,
    entry_id: &str,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new("layout.validation.orphaned_entry"),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "layout {kind} id {entry_id:?} has no matching semantic entry and was pruned"
        ),
        span: Some(Span::from_segments(path)),
        artifact_id: Some(artifact_id.to_owned()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "layout migration round-trip assertions"
)]
mod tests {
    use std::collections::BTreeMap;

    use super::{extract_inline_layout, hydrate_inline_layout, prune_layout};
    use norn_ir::{
        ArtifactEnvelope, ArtifactLayout, ColumnLayout, Decimal, DiagnosticSeverity, LayoutField,
        LayoutSidecar, NodeLayout, RuntimeValue,
    };

    fn raw_rule() -> RuntimeValue {
        RuntimeValue::from_json_text(
            r#"{
              "irVersion":"0.1","artifactId":"rule.test","kind":"rule",
              "namespace":"test","name":"Test","version":"0.1.0","lifecycle":"draft",
              "scope":{"orgId":"org","environmentId":"dev"},"dependencies":[],
              "provenance":{"source":"authoring_sdk","sourcePath":"test.ts"},
              "payload":{"ruleId":"test.rule","ruleVersion":"2.0","graph":{
                "settings":{"grid":{"enabled":true,"step":0.1234567890123456789012345678},"extra":null},
                "viewport":{"x":-3.125,"y":0.5,"zoom":0.75},
                "nodes":[{"id":"table","type":"decisionTableNode","name":"Table",
                  "position":{"x":12345678901234567890.12345,"y":-0.0000000000000000000000000001},
                  "description":null,"folded":false,
                  "content":{"inputs":[{"id":"folded","name":"Flag","field":"flag","width":240.5}],
                    "outputs":[{"id":"result","name":"Result","field":"result","width":null}],
                    "rules":[{"_id":"row-1","_description":"meaning stays semantic",
                      "folded":"input.flag == true","result":"output.result"}]}}],
                "edges":[{"id":"edge-1","sourceId":"table","targetId":"table",
                  "type":"edge","name":null,"targetHandle":"target"}]
              }}
            }"#,
        )
        .expect("raw rule should be valid JSON")
    }

    #[test]
    fn extracts_and_hydrates_editor_fields_without_changing_jdm_cells() {
        let mut artifact = raw_rule();
        let original = artifact.clone();

        let sidecar = extract_inline_layout(&mut artifact).expect("layout extraction");
        let layout = &sidecar.artifacts["rule.test"];
        let node = &layout.nodes["table"];
        assert!(matches!(&node.description, LayoutField::Present(None)));
        assert!(matches!(&node.folded, LayoutField::Present(Some(false))));
        assert!(matches!(
            &node.position,
            LayoutField::Present(Some(position))
                if position.x
                    == Decimal::parse("12345678901234567890.12345").expect("exact x")
        ));
        assert!(matches!(
            &node.columns["folded"].width,
            LayoutField::Present(Some(width))
                if *width == Decimal::parse("240.5").expect("exact width")
        ));
        assert!(matches!(
            &layout.edges["edge-1"].name,
            LayoutField::Present(None)
        ));
        assert!(matches!(&layout.settings, LayoutField::Present(_)));

        let row = artifact
            .get("payload")
            .and_then(|value| value.get("graph"))
            .and_then(|value| value.get("nodes"))
            .and_then(RuntimeValue::as_array)
            .and_then(|nodes| nodes.first())
            .and_then(|node| node.get("content"))
            .and_then(|value| value.get("rules"))
            .and_then(RuntimeValue::as_array)
            .and_then(|rows| rows.first())
            .expect("decision row");
        assert_eq!(
            row.get("folded").and_then(RuntimeValue::as_str),
            Some("input.flag == true")
        );
        assert_eq!(
            row.get("_description").and_then(RuntimeValue::as_str),
            Some("meaning stays semantic")
        );

        hydrate_inline_layout(&mut artifact, &sidecar).expect("layout hydration");
        assert_eq!(artifact, original);
    }

    #[test]
    fn extraction_is_atomic_for_invalid_layout_and_duplicate_ids() {
        let mut invalid = raw_rule();
        let original = invalid.clone();
        let viewport = invalid
            .as_object_mut()
            .and_then(|root| root.get_mut("payload"))
            .and_then(RuntimeValue::as_object_mut)
            .and_then(|payload| payload.get_mut("graph"))
            .and_then(RuntimeValue::as_object_mut)
            .and_then(|graph| graph.get_mut("viewport"))
            .and_then(RuntimeValue::as_object_mut)
            .expect("viewport object");
        viewport.insert("zoom".to_owned(), RuntimeValue::String("large".to_owned()));
        let invalid_original = invalid.clone();
        let error = extract_inline_layout(&mut invalid).expect_err("invalid viewport is rejected");
        assert_eq!(
            error[0].code.as_str(),
            "layout.validation.invalid_inline_field"
        );
        assert_eq!(invalid, invalid_original);

        let mut duplicate = original;
        let graph = duplicate
            .as_object_mut()
            .and_then(|root| root.get_mut("payload"))
            .and_then(RuntimeValue::as_object_mut)
            .and_then(|payload| payload.get_mut("graph"))
            .and_then(RuntimeValue::as_object_mut)
            .expect("graph object");
        let RuntimeValue::Array(nodes) = graph.get_mut("nodes").expect("nodes") else {
            panic!("nodes array");
        };
        nodes.push(nodes[0].clone());
        let duplicate_original = duplicate.clone();
        let error = extract_inline_layout(&mut duplicate).expect_err("duplicate ids are rejected");
        assert_eq!(error[0].code.as_str(), "rule.validation.jdm_duplicate_id");
        assert_eq!(duplicate, duplicate_original);
    }

    #[test]
    fn empty_geometry_does_not_create_a_layout_artifact() {
        let mut artifact = RuntimeValue::from_json_text(
            r#"{
                "artifactId":"rule.empty","kind":"rule",
                "payload":{"ruleVersion":"2.0","graph":{
                    "nodes":[{"id":"node","name":"Node","type":"inputNode"}],"edges":[]
                }}
            }"#,
        )
        .expect("empty rule");
        let original = artifact.clone();
        let sidecar = extract_inline_layout(&mut artifact).expect("empty extraction");
        assert!(sidecar.artifacts.is_empty());
        assert_eq!(artifact, original);
    }

    #[test]
    fn widget_layout_prunes_removed_nodes_and_stale_list_columns() {
        let artifact: ArtifactEnvelope = norn_ir::runtime::decode(
            &RuntimeValue::from_json_text(include_str!(
                "../../../fixtures/ir/ux-orders-customer.widget.json"
            ))
            .expect("widget fixture JSON"),
        )
        .expect("typed widget fixture");
        let mut sidecar = LayoutSidecar::default();
        let mut columns = BTreeMap::new();
        for id in ["order-number", "order-status", "stale-field"] {
            columns.insert(
                id.to_owned(),
                ColumnLayout {
                    width: LayoutField::Present(Some(Decimal::from(180_u32))),
                },
            );
        }
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "orders-list-node".to_owned(),
            NodeLayout {
                columns,
                ..NodeLayout::default()
            },
        );
        nodes.insert("removed-node".to_owned(), NodeLayout::default());
        sidecar.artifacts.insert(
            artifact.artifact_id.clone(),
            ArtifactLayout {
                nodes,
                ..ArtifactLayout::default()
            },
        );
        sidecar
            .artifacts
            .insert("removed-artifact".to_owned(), ArtifactLayout::default());

        let diagnostics = prune_layout(&mut sidecar, &[artifact]);
        assert_eq!(diagnostics.len(), 3);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
        );
        let layout = &sidecar.artifacts["commerce.widget.orders"];
        assert!(
            layout.nodes["orders-list-node"]
                .columns
                .contains_key("order-number")
        );
        assert!(
            layout.nodes["orders-list-node"]
                .columns
                .contains_key("order-status")
        );
        assert!(
            !layout.nodes["orders-list-node"]
                .columns
                .contains_key("stale-field")
        );
        assert!(!layout.nodes.contains_key("removed-node"));
        assert!(!sidecar.artifacts.contains_key("removed-artifact"));
    }
}
