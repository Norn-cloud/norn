//! Typed data contract for the reserved `MappingIR` evidence extension.

use serde::{Deserialize, Serialize};

/// Versioned extension contract; it carries evidence, never approval authority.
pub const EVIDENCE_SLOT: &str = "norn.mapping.evidence/0.1.0";

/// Deterministic generator identity and the source-to-target decisions it made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingEvidence {
    /// Exact generator release.
    pub generator: String,
    /// Norn1 domain-separated SHA-256 binding the complete MappingIR payload,
    /// including retained decisions and other extensions, with only this field
    /// omitted to avoid self-reference. An edit requires regenerated evidence;
    /// this integrity binding confers no approval authority.
    pub mapping_sha256: String,
    /// Source envelope identity; individual edges address its dataset/field nodes.
    pub source: EvidenceArtifact,
    /// Typed decisions, including explicit gaps.
    pub edges: Vec<EvidenceEdge>,
    /// Hash-pinned target table referenced by edge target ids.
    pub targets: Vec<EvidenceArtifact>,
    /// Exact human-reviewed external identity decisions used by generated guards.
    #[serde(default)]
    pub reviewed_keys: Vec<ReviewedKey>,
    /// Reviewed source-owned references into existing canonical records.
    #[serde(default)]
    pub reviewed_references: Vec<ReviewedReference>,
}

/// Explicit reviewed external identity; never inferred from display names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedKey {
    /// Source dataset id.
    pub dataset: String,
    /// Exact source record id.
    pub external_key: String,
    /// Reviewed canonical object key.
    pub canonical_key: String,
    /// Human review reference.
    pub evidence: String,
}

/// One source node's proposed relationship with an exact target definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceEdge {
    /// Source dataset id.
    pub dataset: String,
    /// Source field name, absent for a type decision.
    pub field: Option<String>,
    /// Target envelope's semantic content hash.
    pub target: String,
    /// Target property/link id, absent for a type decision.
    pub member: Option<String>,
    /// Semantics of this proposal edge.
    pub decision: EvidenceDecision,
    /// Reviewable reason, not an inferred equivalence claim.
    pub reason: String,
}

/// Proposal edges distinguish deterministic derivation, reference suggestions and gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDecision {
    /// The importer/generator produced the target member. At type level, the
    /// target retains a source-owned, source-namespaced record identity.
    DerivedFrom,
    /// A declared reference or standard lineage suggests a canonical target;
    /// type-level identity writes require an exact reviewed key table.
    ReferenceCandidate,
    /// The source or target contract is insufficient for a proven transform.
    Gap,
}

/// Serializable semantic envelope identity using the existing receipt hash mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceArtifact {
    /// Exact artifact id.
    pub artifact_id: String,
    /// Exact artifact version.
    pub version: String,
    /// Norn1 semantic envelope hash.
    pub sha256: String,
}

/// Exact source-to-canonical reference configuration, approved with ordinary definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedReference {
    /// Source dataset id.
    pub dataset: String,
    /// Source-native reference field (or an external object's id).
    pub field: String,
    /// Link member on the source-owned object.
    pub link: String,
    /// Canonical object type; this source never acquires ownership of it.
    pub target_type: String,
    /// Exact source-local external key.
    pub external_key: String,
    /// Exact canonical target key.
    pub canonical_key: String,
    /// Optional discriminator for polymorphic source references.
    pub selector_field: Option<String>,
    /// Exact discriminator value; present iff selector_field is present.
    pub selector_value: Option<String>,
    /// Human review reference retained in the definition revision.
    pub evidence: String,
}
