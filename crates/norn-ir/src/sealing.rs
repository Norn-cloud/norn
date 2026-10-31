//! Sealing IR types for customization overlays. The pure, deterministic sealing
//! rules live in `norn_checker::sealing`.
use crate::{ArtifactKind, Diagnostic, JsonValue, ScopeRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub const SEALING_IR_VERSION: &str = "0.1";
pub const SEAL_PRECEDENCE_LADDER: [&str; 4] =
    ["kernel", "ir_seal", "extension_point", "tenant_additive"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SealableNodeKind {
    Entity,
    Relation,
    Action,
    Rule,
    Connector,
    Widget,
    Customization,
    Setting,
    BusinessPack,
    Policy,
    Derived,
    SourceSchema,
    Mapping,
    /// Scoped temporal law.
    Law,
    /// A claim about definitions.
    Claim,
    /// A governed ruling.
    Ruling,
    Field,
    SurfaceSlot,
}
impl From<ArtifactKind> for SealableNodeKind {
    fn from(value: ArtifactKind) -> Self {
        match value {
            ArtifactKind::Entity => Self::Entity,
            ArtifactKind::Relation => Self::Relation,
            ArtifactKind::Action => Self::Action,
            ArtifactKind::Rule => Self::Rule,
            ArtifactKind::Connector => Self::Connector,
            ArtifactKind::Widget => Self::Widget,
            ArtifactKind::Customization => Self::Customization,
            ArtifactKind::Setting => Self::Setting,
            ArtifactKind::BusinessPack => Self::BusinessPack,
            ArtifactKind::Policy => Self::Policy,
            ArtifactKind::Derived => Self::Derived,
            ArtifactKind::SourceSchema => Self::SourceSchema,
            ArtifactKind::Mapping => Self::Mapping,
            ArtifactKind::Law => Self::Law,
            ArtifactKind::Claim => Self::Claim,
            ArtifactKind::Ruling => Self::Ruling,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionContributionKind {
    Field,
    Relation,
    Action,
    Rule,
    Widget,
    SurfaceSlot,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SealAnnotation {
    pub seal_version: String,
    pub node_id: String,
    pub node_kind: SealableNodeKind,
    pub sealed: bool,
    pub allows_mutation: bool,
    pub extension_point_ids: Vec<String>,
    pub scope_capabilities: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPointDefinition {
    pub extension_point_id: String,
    pub owner_node_id: String,
    pub allowed_contribution_kind: ExtensionContributionKind,
    pub contract: BTreeMap<String, JsonValue>,
    pub scope_capabilities: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum CustomizationOverlayOperation {
    Add {
        node_id: String,
        node_kind: ExtensionContributionKind,
        tenant_namespace: String,
        scope: ScopeRef,
        payload: BTreeMap<String, JsonValue>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_point_id: Option<String>,
        required_capabilities: Vec<String>,
    },
    Mutate {
        node_id: String,
        base_node_id: String,
        payload: BTreeMap<String, JsonValue>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_point_id: Option<String>,
        required_capabilities: Vec<String>,
    },
    Unseal {
        base_node_id: String,
        requested_by: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SealValidationResult {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SealInput {
    CustomizationOverlay {
        base_seals: Vec<SealAnnotation>,
        extension_points: Vec<ExtensionPointDefinition>,
        operations: Vec<CustomizationOverlayOperation>,
    },
    RuntimeCapabilityBackstop {
        requested_node_id: String,
        seals: Vec<SealAnnotation>,
        capabilities: Vec<String>,
    },
    SealUpgrade {
        previous_base_seals: Vec<SealAnnotation>,
        upgraded_base_seals: Vec<SealAnnotation>,
        #[serde(default)]
        extension_points: Vec<ExtensionPointDefinition>,
        overlay_operations: Vec<CustomizationOverlayOperation>,
    },
}
pub type SealOutcome = SealValidationResult;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    InvalidInput { reason: String },
}
impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { reason } => write!(f, "invalid sealing input: {reason}"),
        }
    }
}
impl Error for SealError {}
