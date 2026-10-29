//! Versioned editor layout sidecar for definition artifacts.
//!
//! Layout is stored beside, rather than inside, semantic definitions. The
//! maps are keyed by artifact and stable authoring ids; they never imply an
//! ordering for rule nodes, table rows, widget children, or other semantic
//! sequences. `RuntimeValue` keeps arbitrary settings and editor values exact,
//! including explicit `null` values.

use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Decimal, IrVersion01, RuntimeValue};

/// The current layout sidecar format version (`0.1`).
pub const LAYOUT_SIDECAR_VERSION: &str = "0.1";

/// A JSON member that preserves the difference between being absent and being
/// present with any value, including `null`.
///
/// The containing struct uses `skip_serializing_if = "LayoutField::is_absent"`
/// so an absent field stays omitted on the wire. A present field serializes as
/// its contained JSON value without an extra wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LayoutField<T> {
    /// The member was not present.
    #[default]
    Absent,
    /// The member was present, including when its value is `null`.
    Present(T),
}

impl<T> LayoutField<T> {
    /// Whether the member was omitted.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// The present value, if any.
    #[must_use]
    pub const fn as_present(&self) -> Option<&T> {
        match self {
            Self::Absent => None,
            Self::Present(value) => Some(value),
        }
    }
}

impl<T: Serialize> Serialize for LayoutField<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Absent => serializer.serialize_none(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for LayoutField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

impl<T: JsonSchema> JsonSchema for LayoutField<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("LayoutField_{}", T::schema_name()).into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        generator.subschema_for::<T>()
    }
}

/// Versioned sidecar containing editor state for semantic artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutSidecar {
    /// Version of this layout wire format.
    pub layout_version: IrVersion01,
    /// Layout state keyed by the semantic artifact's stable artifact id.
    pub artifacts: BTreeMap<String, ArtifactLayout>,
}

impl Default for LayoutSidecar {
    fn default() -> Self {
        Self {
            layout_version: IrVersion01::V0_1,
            artifacts: BTreeMap::new(),
        }
    }
}

/// Editor state for one semantic artifact.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactLayout {
    /// Canvas/editor state keyed by stable node, page, or component id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<String, NodeLayout>,
    /// Editor state keyed by stable edge id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub edges: BTreeMap<String, EdgeLayout>,
    /// Optional viewport state in the editor's canvas coordinate system.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub viewport: LayoutField<Option<LayoutViewport>>,
    /// Optional editor settings preserved as exact JSON.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub settings: LayoutField<RuntimeValue>,
}

/// Layout for one stable node or component id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeLayout {
    /// Canvas position with exact decimal coordinates.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub position: LayoutField<Option<CanvasPosition>>,
    /// Optional editor description, retaining explicit `null` during migration.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub description: LayoutField<Option<String>>,
    /// Optional collapsed/folded state.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub folded: LayoutField<Option<bool>>,
    /// Row editor state keyed by stable JDM row id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rows: BTreeMap<String, RowLayout>,
    /// Column editor state keyed by stable JDM column id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub columns: BTreeMap<String, ColumnLayout>,
}

/// Editor state for one stable decision-table row id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RowLayout {
    /// Optional collapsed/folded state.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub folded: LayoutField<Option<bool>>,
}

/// Editor state for one stable decision-table column id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColumnLayout {
    /// Optional editor column width, retained as an exact decimal.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub width: LayoutField<Option<Decimal>>,
}

/// Editor-only edge labels and handles keyed by the stable JDM edge id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdgeLayout {
    /// Optional editor edge name.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub name: LayoutField<Option<String>>,
    /// Optional target-side editor handle.
    #[serde(default, skip_serializing_if = "LayoutField::is_absent")]
    pub target_handle: LayoutField<Option<String>>,
    /// Optional JDM edge type, which is ignored by evaluation.
    #[serde(
        rename = "type",
        default,
        skip_serializing_if = "LayoutField::is_absent"
    )]
    pub edge_type: LayoutField<Option<String>>,
}

/// Exact editor canvas coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CanvasPosition {
    /// Horizontal canvas coordinate.
    pub x: Decimal,
    /// Vertical canvas coordinate.
    pub y: Decimal,
}

/// Exact pan and zoom state of the editor canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutViewport {
    /// Horizontal viewport offset.
    pub x: Decimal,
    /// Vertical viewport offset.
    pub y: Decimal,
    /// Canvas zoom factor.
    pub zoom: Decimal,
}
