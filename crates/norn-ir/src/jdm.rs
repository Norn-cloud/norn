//! RuleIR v2 decision graph: the GoRules JSON Decision Model (JDM) under the
//! Norn profile (ruling 2026-09-25 §3).
//!
//! These are the wire types only. They mirror the JDM model of the pinned
//! `Norn-cloud/zen` tag (`core/types/src/decision/mod.rs`: `DecisionNode`,
//! `DecisionNodeKind`, `DecisionEdge`, and the per-node content structs) so a
//! graph authored in `jdm-editor` decodes here and reaches `zen-engine`
//! unchanged, but they are closed: every struct denies unknown members, so a
//! graph that `zen-engine` would silently accept with an extra member is a
//! decode diagnostic in `norn-checker` instead.
//!
//! The Norn profile admits `inputNode`, `outputNode`, `expressionNode`,
//! `decisionTableNode`, `switchNode`, and `decisionNode` (sub-decision).
//! `functionNode` (ruling 2026-09-25 §3) and `customNode` (ruling 2026-09-27
//! compute boundary; D33 decided v1: no inline custom node) are still
//! *decodable* — [`JdmNode::FunctionNode`] and [`JdmNode::CustomNode`] — so the
//! checker rejects them with a stable diagnostic at the node's path instead of
//! failing the whole graph decode. The admission policy (which node types the
//! Norn profile admits) lives only in `norn-checker`; `norn-ir` does not
//! validate.
//! No `zen-*` type crosses this boundary and `norn-ir` never depends on
//! `zen-engine`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::present_non_null;
use crate::{JsonValue, ZenExpression};

// JDM members that may be omitted but must not be `null` mirror the pinned
// `zen-engine` model, whose `#[serde(default)]` members (node `content`, column
// `name`, `hitPolicy`, `executionMode`, `passThrough`) and string row values
// (`_description`) reject `null` when present. Members that are `Option` or
// empty-string-is-none in `zen-engine` (`schema`, `field`, `inputField`,
// `outputPath`, column `type`, `sourceHandle`) and editor-only members
// `zen-engine` ignores keep accepting `null`.

/// The JDM `contentType` emitted by `jdm-editor`.
pub const JDM_CONTENT_TYPE: &str = "application/vnd.gorules.decision";

/// A JDM decision graph (`GraphContent` in `zen-engine`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmGraph {
    /// Optional `jdm-editor` content type ([`JDM_CONTENT_TYPE`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub nodes: Vec<JdmNode>,
    pub edges: Vec<JdmEdge>,
    /// Graph imports. In `zen-engine` an import names a policy document (a
    /// dictionary of output-column types). Norn has no such artifact: value
    /// types come from `EntityIR` v2, so no import resolves and the Norn
    /// profile rejects every one (T3-DEC-1); the evaluator never loads one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    /// `jdm-editor` graph settings; editor metadata, ignored by evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<JsonValue>,
}

/// A directed edge between two nodes (`DecisionEdge`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmEdge {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    /// Source handle; a switch statement id when the source is a switch node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_handle: Option<String>,
    /// Editor edge type (`"edge"`); ignored by evaluation.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub edge_type: Option<String>,
}

/// Editor canvas position. Layout only; ignored by evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JdmPosition {
    pub x: f64,
    pub y: f64,
}

/// One JDM node, internally tagged by its JDM `type`.
///
/// Every variant carries the common `id`, `name`, and optional editor
/// `position`; use [`JdmNode::id`], [`JdmNode::name`], and
/// [`JdmNode::node_type`] instead of matching when only those are needed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum JdmNode {
    /// Graph input. `content.schema` is the JDM JSON-Schema string, if any.
    InputNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "JdmSchemaContent")]
        content: Option<JdmSchemaContent>,
    },
    /// Graph output. `content.schema` is the JDM JSON-Schema string, if any.
    OutputNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "JdmSchemaContent")]
        content: Option<JdmSchemaContent>,
    },
    /// Named ZEN Expression assignments.
    ExpressionNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        content: JdmExpressionContent,
    },
    /// Decision table with ZEN unary-test / expression cells.
    DecisionTableNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        content: JdmDecisionTableContent,
    },
    /// Conditional branching on ZEN Expression statements.
    SwitchNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        content: JdmSwitchContent,
    },
    /// Sub-decision: evaluates another decision graph by key.
    DecisionNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        content: JdmSubDecisionContent,
    },
    /// JavaScript function node. Outside the Norn profile: decodable only so
    /// the checker can reject it with a diagnostic; never executed.
    FunctionNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        /// Opaque: `zen-engine` accepts a source string (v1) or `{ "source" }` (v2).
        content: JsonValue,
    },
    /// Custom (adapter-evaluated) node. Outside the Norn profile (D33 v1):
    /// decodable only so the checker can reject it with a diagnostic.
    CustomNode {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<JdmPosition>,
        content: JdmCustomNodeContent,
    },
}

/// The JDM `type` discriminator of a [`JdmNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JdmNodeType {
    InputNode,
    OutputNode,
    ExpressionNode,
    DecisionTableNode,
    SwitchNode,
    DecisionNode,
    FunctionNode,
    CustomNode,
}

impl JdmNodeType {
    /// The wire `type` string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InputNode => "inputNode",
            Self::OutputNode => "outputNode",
            Self::ExpressionNode => "expressionNode",
            Self::DecisionTableNode => "decisionTableNode",
            Self::SwitchNode => "switchNode",
            Self::DecisionNode => "decisionNode",
            Self::FunctionNode => "functionNode",
            Self::CustomNode => "customNode",
        }
    }

    /// Parse a wire `type` string.
    #[must_use]
    pub fn from_wire(node_type: &str) -> Option<Self> {
        [
            Self::InputNode,
            Self::OutputNode,
            Self::ExpressionNode,
            Self::DecisionTableNode,
            Self::SwitchNode,
            Self::DecisionNode,
            Self::FunctionNode,
            Self::CustomNode,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == node_type)
    }
}

impl JdmNode {
    /// The node's graph-unique id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::InputNode { id, .. }
            | Self::OutputNode { id, .. }
            | Self::ExpressionNode { id, .. }
            | Self::DecisionTableNode { id, .. }
            | Self::SwitchNode { id, .. }
            | Self::DecisionNode { id, .. }
            | Self::FunctionNode { id, .. }
            | Self::CustomNode { id, .. } => id,
        }
    }

    /// The node's display name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::InputNode { name, .. }
            | Self::OutputNode { name, .. }
            | Self::ExpressionNode { name, .. }
            | Self::DecisionTableNode { name, .. }
            | Self::SwitchNode { name, .. }
            | Self::DecisionNode { name, .. }
            | Self::FunctionNode { name, .. }
            | Self::CustomNode { name, .. } => name,
        }
    }

    /// The node's JDM `type`.
    #[must_use]
    pub const fn node_type(&self) -> JdmNodeType {
        match self {
            Self::InputNode { .. } => JdmNodeType::InputNode,
            Self::OutputNode { .. } => JdmNodeType::OutputNode,
            Self::ExpressionNode { .. } => JdmNodeType::ExpressionNode,
            Self::DecisionTableNode { .. } => JdmNodeType::DecisionTableNode,
            Self::SwitchNode { .. } => JdmNodeType::SwitchNode,
            Self::DecisionNode { .. } => JdmNodeType::DecisionNode,
            Self::FunctionNode { .. } => JdmNodeType::FunctionNode,
            Self::CustomNode { .. } => JdmNodeType::CustomNode,
        }
    }
}

/// Input/output node content (`InputNodeContent` / `OutputNodeContent`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JdmSchemaContent {
    /// JSON Schema serialized as a string, as JDM stores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// JDM hit policy shared by decision tables and switch nodes.
///
/// DMN policies other than `first` and `collect` translate or are rejected at
/// DMN ingress (T3-DEC-4); they have no JDM representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum JdmHitPolicy {
    First,
    Collect,
}

/// `executionMode` of a transforming node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum JdmExecutionMode {
    Single,
    Loop,
}

/// Decision-table content (`DecisionTableContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmDecisionTableContent {
    pub hit_policy: JdmHitPolicy,
    pub inputs: Vec<JdmTableInput>,
    pub outputs: Vec<JdmTableOutput>,
    /// Ordered rows; each carries a stable `_id`.
    pub rules: Vec<JdmTableRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JdmExecutionMode")]
    pub execution_mode: Option<JdmExecutionMode>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    pub pass_through: Option<bool>,
}

/// A decision-table input column (`DecisionTableInputField`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmTableInput {
    pub id: String,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub name: Option<String>,
    /// Input path; absent means the column's cells are full expressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Editor column type (for example `"expression"`); ignored by evaluation.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub column_type: Option<String>,
}

/// A decision-table output column (`DecisionTableOutputField`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmTableOutput {
    pub id: String,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub name: Option<String>,
    pub field: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub column_type: Option<String>,
}

/// One decision-table row: a stable `_id`, an optional `_description`, and one
/// ZEN Expression cell per column id (unary tests for inputs, expressions for
/// outputs). An empty cell matches anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JdmTableRow {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(
        rename = "_description",
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub cells: BTreeMap<String, ZenExpression>,
}

/// Expression-node content (`ExpressionNodeContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmExpressionContent {
    pub expressions: Vec<JdmExpression>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JdmExecutionMode")]
    pub execution_mode: Option<JdmExecutionMode>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    pub pass_through: Option<bool>,
}

/// One `key = value` assignment of an expression node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JdmExpression {
    pub id: String,
    pub key: String,
    pub value: ZenExpression,
}

/// Switch-node content (`SwitchNodeContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmSwitchContent {
    /// Absent means `first` (the `zen-engine` default).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JdmHitPolicy")]
    pub hit_policy: Option<JdmHitPolicy>,
    pub statements: Vec<JdmSwitchStatement>,
}

/// One switch branch; edges leave the switch with `sourceHandle` = `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JdmSwitchStatement {
    pub id: String,
    /// Boolean ZEN Expression; empty means the default branch.
    pub condition: ZenExpression,
    /// Editor default-branch marker. Evaluation treats an empty `condition`
    /// as the default branch; this flag is editor metadata.
    #[serde(rename = "isDefault", default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

/// Sub-decision content (`DecisionNodeContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JdmSubDecisionContent {
    /// Key of the referenced decision graph: the `artifactId` of a `RuleIR` v2
    /// rule that the envelope declares as a required `rule` dependency. The
    /// checker resolves it and bounds the sub-decision closure (T3-DEC-1).
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JdmExecutionMode")]
    pub execution_mode: Option<JdmExecutionMode>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    pub pass_through: Option<bool>,
}

/// Custom-node content (`CustomNodeContent`). Decodable only for rejection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JdmCustomNodeContent {
    pub kind: String,
    pub config: JsonValue,
}
