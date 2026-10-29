//! Scoped temporal declarations over the existing ZEN profile. No evaluator or proof engine.
use crate::members::present_non_null;
use crate::{IrVersion01, ProvenanceSource, ZenExpression};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One protected law; its predicates use the subject's `self` environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LawIR {
    /// Wire format version.
    pub law_version: IrVersion01,
    /// Stable identity, preserved by all authoring faces.
    pub id: String,
    /// Principal or registry owner of changes to this law.
    pub owner: String,
    /// Reporting severity, independent of proof status.
    pub severity: LawSeverity,
    /// Reviewed explanation.
    pub rationale: String,
    /// Source and interpretation provenance.
    pub provenance: LawProvenance,
    /// Applicability, independent of the artifact's authorization scope.
    pub scope: LawScope,
    /// Exact ontology object type whose record is bound as `self`.
    pub subject: String,
    /// Optional pointwise activation predicate (not an edge trigger).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenExpression")]
    pub condition: Option<ZenExpression>,
    /// Admitted temporal wrapper.
    pub modality: LawModality,
    /// Optional progress bound.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "LawTiming")]
    pub timing: Option<LawTiming>,
    /// Predicate or exact ontology-bound behavior/invariant identity.
    pub response: LawResponse,
}

/// Applicability of a law. All filters are conjunctive; type lists denote a union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LawScope {
    /// Company/app/cell/entity scope identity; no implicit global default.
    pub root: LawRoot,
    /// Nonempty exact type names, including the subject or its ancestors.
    pub object_types: Vec<String>,
    /// Optional lifecycle-state filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_states: Vec<LawStateScope>,
    /// Commitment registry object types, not a new process primitive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commitments: Vec<String>,
}

/// Named application boundary; authorization still comes from the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum LawRoot {
    /// Organization/company boundary.
    Company(String),
    /// Application boundary.
    App(String),
    /// Cell ownership root.
    Cell(String),
    /// Exact entity type boundary.
    Entity(String),
}

/// One lifecycle-state selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LawStateScope {
    /// Exact object type name.
    pub object_type: String,
    /// Lifecycle property identity.
    pub property: String,
    /// Nonempty admitted state names.
    pub states: Vec<String>,
}

/// Scoped temporal vocabulary; nested temporal operators have no wire representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LawModality {
    /// Response holds at every applicable point.
    Always {},
    /// Response holds at no applicable point.
    Never {},
    /// Response must occur in the future; fairness belongs to a later verifier.
    Eventually {},
    /// Strong until: response holds until the Boolean stop predicate occurs.
    Until {
        /// Ordinary ZEN stop predicate.
        predicate: ZenExpression,
    },
}

/// Progress timing. Exact milliseconds are decimal text, avoiding JS number rounding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LawTiming {
    /// Relative elapsed duration from activation.
    Within {
        /// Positive canonical unsigned milliseconds, bounded by u64.
        millis: String,
    },
    /// Absolute deadline stored on the subject record.
    BeforeDeadline {
        /// Exact timestamp property identity.
        property: String,
    },
}

/// Response slots resolve to existing ontology definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LawResponse {
    /// Boolean state predicate.
    Predicate {
        /// Ordinary ZEN text.
        expression: ZenExpression,
    },
    /// Existing invariant, never a second expression copy.
    Invariant {
        /// Effective owner object type.
        object_type: String,
        /// Stable invariant member id.
        invariant_id: String,
    },
    /// Exact ActionIR artifact identity.
    Action {
        /// Artifact id, resolved through a pinned dependency.
        artifact_id: String,
    },
    /// Existing lifecycle transition.
    Transition {
        /// Exact object type.
        object_type: String,
        /// Lifecycle property.
        property: String,
        /// Transition member id.
        transition_id: String,
    },
}

/// User-facing reporting severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LawSeverity {
    /// Violations require attention.
    Error,
    /// Advisory law.
    Warning,
}

/// Reviewed law provenance; optional members are absent rather than synthesized nulls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LawProvenance {
    /// Existing source taxonomy.
    pub source: ProvenanceSource,
    /// Original source path or reviewed source document.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub source_path: Option<String>,
    /// Reviewing/authoring principal.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub actor_id: Option<String>,
    /// Source timestamp, never read from a clock by this library.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub created_at: Option<String>,
}
