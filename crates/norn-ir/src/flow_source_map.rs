//! Non-executable authoring projection for visual workflow editors.
//!
//! A flow source map records how authoring-level flow nodes and edges map to
//! ordinary admitted Norn artifacts. It is metadata only: it is not an
//! artifact kind, runtime definition, or executable flow representation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A versioned collection of authoring-level flow projections.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowSourceMap {
    /// Wire format identifier, currently `norn-flow-source-map/1`.
    pub format: FlowSourceMapFormat,
    /// The independently authored flows included in this projection.
    pub flows: Vec<FlowSourceMapFlow>,
}

/// The closed set of source-map wire formats accepted by the checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FlowSourceMapFormat {
    /// First version of the non-executable authoring projection.
    #[serde(rename = "norn-flow-source-map/1")]
    V1,
}

/// One authoring flow and its references into the admitted artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowSourceMapFlow {
    /// Stable authoring flow identifier.
    pub flow_id: String,
    /// Authoring flow version.
    pub flow_version: String,
    /// Source file path relative to the artifact provenance root, using `/` separators.
    pub source_path: String,
    /// Exact artifact versions and content hashes produced for this flow.
    pub artifact_refs: Vec<FlowSourceMapArtifactRef>,
    /// Authoring-level steps in the flow graph.
    pub steps: Vec<FlowSourceMapStep>,
    /// Directed authoring-level control-flow and syntactic containment edges.
    pub edges: Vec<FlowSourceMapEdge>,
}

/// One exact artifact identity used by a flow source map.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowSourceMapArtifactRef {
    /// The artifact envelope's `artifactId`.
    pub artifact_id: String,
    /// The artifact envelope's `version`.
    pub version: String,
    /// Lower-case hexadecimal Norn1 SHA-256 of the artifact envelope.
    pub sha256: String,
}

/// An authoring-level flow operation and the lowered artifacts it produced.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowSourceMapStep {
    /// Authoring-site id, stable across formatting changes. Semantic edits or
    /// insertion of identical repeated sites can change it between revisions.
    pub id: String,
    /// The authoring operation represented by this node.
    pub kind: FlowSourceMapStepKind,
    /// Optional source location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<FlowSourceMapSourceLocation>,
    /// Lowered artifact ids associated with this step.
    pub artifact_ids: Vec<String>,
    /// Lowered lifecycle state ids associated with this step.
    pub state_ids: Vec<String>,
    /// Lowered decision-table row ids associated with this step.
    pub decision_row_ids: Vec<String>,
}

/// The closed set of authoring operations represented in a flow source map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FlowSourceMapStepKind {
    /// An external or pool-backed perform step.
    Perform,
    /// A typed failure catch step.
    CatchTag,
    /// A conditional branch.
    If,
    /// A multi-branch selection.
    Switch,
    /// A retry policy.
    Retry,
    /// A timer-backed sleep.
    Sleep,
    /// A composed group of steps.
    All,
    /// A successful terminal result.
    Succeed,
    /// A failed terminal result.
    Fail,
    /// A named result composition.
    Compose,
}

/// One authoring source location.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowSourceMapSourceLocation {
    /// Source snapshot file relative to the artifact provenance root, using `/` separators.
    pub file: String,
    /// One-based source line.
    pub line: u32,
    /// One-based source column in UTF-16 code units, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// One edge between authoring-level steps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowSourceMapEdge {
    /// Source step id.
    pub from: String,
    /// Destination step id.
    pub to: String,
    /// Control-flow or syntactic containment relationship represented by this edge.
    pub kind: FlowSourceMapEdgeKind,
    /// Optional branch, catch tag, or containment slot label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The closed set of edge relationships represented in a flow source map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FlowSourceMapEdgeKind {
    /// Ordinary sequential control flow.
    Next,
    /// A conditional or switch branch.
    Branch,
    /// A caught failure path.
    Catch,
    /// A retry path.
    Retry,
    /// Direct syntactic membership in a container, independent of control flow.
    Contains,
}

/// Emit the JSON Schema for a non-executable flow source map.
#[must_use]
pub fn flow_source_map_schema() -> schemars::Schema {
    schemars::schema_for!(FlowSourceMap)
}
