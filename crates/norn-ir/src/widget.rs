//! Typed WidgetIR root: the payload of a `widget` artifact (T1-IR-1d, #2509).
//!
//! WidgetIR is the canonical UI model (AGENTS.md); OpenUI/shadcn renderers are
//! projection hints only. The shape mirrors today's validator exactly; nested
//! objects keep unchecked members in `additional` (see [`crate::members`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, ProposalMetadata, present_non_null};
use crate::rule::JsonObject;
use crate::widget_ux::WidgetUx;

/// Typed payload of a `widget` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetRoot {
    pub widget_version: IrVersion01,
    pub widget_id: String,
    /// Object reads the widget renders.
    pub data_dependencies: Vec<WidgetDataDependency>,
    /// Non-empty set of render states.
    pub view_states: Vec<WidgetViewState>,
    /// Norn actions the widget can invoke; each needs a matching action
    /// dependency on the envelope.
    pub action_bindings: Vec<WidgetActionBinding>,
    /// Permissions needed to see the widget.
    pub permissions: Vec<WidgetPermission>,
    /// New typed page and interaction contract. Omitted on legacy WidgetIR;
    /// the checker enforces that legacy dependency and action arrays are
    /// empty when this contract is present.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetUx")]
    pub ux: Option<WidgetUx>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetProjectionHints")]
    pub projection_hints: Option<WidgetProjectionHints>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ProposalMetadata")]
    pub proposal: Option<ProposalMetadata>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// One object read a widget depends on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetDataDependency {
    pub dependency_id: String,
    pub object_kind: String,
    pub query: WidgetQuery,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// The read shape of a widget data dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WidgetQuery {
    ObjectList {
        #[serde(flatten)]
        additional: JsonObject,
    },
    ObjectDetail {
        #[serde(flatten)]
        additional: JsonObject,
    },
    RelationTraversal {
        relation_kind: String,
        #[serde(flatten)]
        additional: JsonObject,
    },
}

/// One render state of a widget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetViewState {
    pub state_id: String,
    pub kind: WidgetViewStateKind,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Render-state kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetViewStateKind {
    Empty,
    Loading,
    Ready,
    Error,
    Preview,
}

/// A widget control bound to a Norn action (never a raw mutation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetActionBinding {
    pub binding_id: String,
    pub action_id: String,
    /// Exact action version; the dependency check defaults it to `0.1.0`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub action_version: Option<String>,
    pub label: String,
    pub required_permissions: Vec<String>,
    /// Members the validator does not check, kept verbatim (for example
    /// `inputMapping`). `unsafeRawMutation: true` is rejected by the checker.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// One permission needed to see a widget, with its reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetPermission {
    pub permission: String,
    pub reason: String,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Renderer and layout hints for widget projections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WidgetProjectionHints {
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetRenderer")]
    pub renderer: Option<WidgetRenderer>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetLayout")]
    pub layout: Option<WidgetLayout>,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Projection renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetRenderer {
    React,
    Openui,
    Shadcn,
    Native,
}

/// Projection layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetLayout {
    Table,
    Detail,
    Kanban,
    Form,
    Dashboard,
}
