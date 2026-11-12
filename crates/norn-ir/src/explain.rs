//! Read-only explanations over the existing catalog and generated obligations.
use crate::definition_catalog::{DefinitionCatalog, MemberRef, WritesRow};
use crate::{ScopeRef, SliceDeclaration};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExplainKind {
    Action,
    Law,
    Concept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ExplainFormat {
    #[serde(rename = "norn/explain@2")]
    V2,
}

/// EXPLAIN-CLOSURE-1: the entire conservative closure remains pinned and visible.
/// Consumers must establish disclosure authority before supplying this catalog.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExplainReport {
    pub format: ExplainFormat,
    pub target: MemberRef,
    pub scope: ScopeRef,
    /// Whole-set invalidation inventory, not a minimal field-level influence claim.
    pub closure: DefinitionCatalog,
    /// Declared ontology correspondences; their law transport is unproved.
    pub interpretations: Vec<ExplainInterpretation>,
    pub writers: Vec<LawWriter>,
    pub reads: Vec<SliceDeclaration>,
    pub direct_effects: Vec<WritesRow>,
    pub laws: Vec<ExplainedLaw>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExplainedLaw {
    pub declaration: MemberRef,
    pub law: crate::LawIR,
    pub wording: Option<ExplainWording>,
    pub source_path: Option<String>,
    /// Canonical shortest paths from contributing concepts to the authored subject.
    pub inheritance_paths: Vec<Vec<MemberRef>>,
    /// Canonical paths from each containing pack to the authored law artifact.
    pub containment_paths: Vec<Vec<crate::definition_catalog::ArtifactKey>>,
    /// Source-only explanations never invent checked proof evidence.
    pub proof: ExplainProof,
    /// No solver assumption profile is inferred from source declarations.
    pub assumptions: Vec<String>,
    /// The admitted bound invariant enforced by the commit checker.
    pub enforcement: Option<crate::definition_catalog::LawBinding>,
    /// Existing source-bound induction statements, without a proof-level promotion.
    pub obligations: Vec<crate::RuntimeValue>,
    pub why_unknown: Vec<ExplainUnknown>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExplainProof {
    Unknown,
}

/// EXPLAIN-UNKNOWN-1: unknowns retain a concrete writer identity or footprint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExplainUnknown {
    pub writer: Option<LawWriter>,
    pub footprint: String,
    pub reason: String,
    pub fix: String,
}

/// Pinned existing ontology-link data, without promoting correspondence to law proof.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExplainInterpretation {
    pub declaration: crate::definition_catalog::ArtifactKey,
    pub link: crate::business_pack::OntologyLinkRoot,
    pub proof: ExplainProof,
}

/// The existing reviewed slots, rendered through the selected wording locale map.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExplainWording {
    pub locale: String,
    pub direction: String,
    pub sha256: String,
    pub source: crate::deontic::RuleSourceSpan,
    pub slots: Vec<crate::requirement_display::RequirementSlot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExplainOptions {
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub requirements: Vec<crate::deontic::StructuredRequirement>,
}

pub fn explain_options_schema() -> schemars::Schema {
    schemars::schema_for!(ExplainOptions)
}

pub fn explain_schema() -> schemars::Schema {
    schemars::schema_for!(ExplainReport)
}

/// Disjoint writer identities; artifact ids cannot collide with lifecycle coordinates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LawWriter {
    /// A complete admitted behavior artifact (including cascaded/opaque effects).
    Artifact {
        /// The artifact identity, without delimiter interpretation.
        artifact_id: String,
    },
    /// One transition of an effective, possibly inherited lifecycle.
    Transition {
        /// Effective concrete type.
        object_type: String,
        /// Lifecycle property.
        property: String,
        /// Declared transition id.
        transition: String,
    },
}
impl LawWriter {
    /// Tagged wire identity, with independently encoded components.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Artifact { artifact_id } => {
                serde_json::json!({"kind":"artifact","artifactId":artifact_id})
            }
            Self::Transition {
                object_type,
                property,
                transition,
            } => {
                serde_json::json!({"kind":"transition","objectType":object_type,"property":property,"transition":transition})
            }
        }
    }
    /// Conservative UTF-8/JSON-escaping allocation bound, before copying any id.
    #[must_use]
    pub fn encoded_byte_bound(&self) -> usize {
        let lengths = match self {
            Self::Artifact { artifact_id } => artifact_id.len(),
            Self::Transition {
                object_type,
                property,
                transition,
            } => object_type
                .len()
                .saturating_add(property.len())
                .saturating_add(transition.len()),
        };
        lengths.saturating_mul(6).saturating_add(128)
    }
}
