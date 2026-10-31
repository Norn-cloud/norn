//! Claims and rulings (norn#62; Astra verdict 2026-10-30): narrow, typed,
//! governed statements about definitions.
//!
//! A **claim** ([`ClaimIr`], the payload of a `claim` artifact) states a fact
//! about definitions that the checker can act on, with its evidence. Each
//! piece of evidence carries its own [`EvidenceStrength`]; a claim has no
//! decision authority.
//!
//! A **ruling** ([`RulingIr`], the payload of a `ruling` artifact) decides the
//! claims it resolves. Its authority is its decider ([`Decider`]) and its
//! governance state is the envelope lifecycle: `approved` or `active` is
//! *ratified*, and only the governed review path can persist those states
//! for a ruling (`authoring_lifecycle_allowed`). `superseded` with a later
//! ruling's `supersedes` is supersession.
//!
//! **A ruling never waives a contradiction.** Its outcome records how the
//! definitions changed (a choice, a parameter, scopes); the checker rechecks
//! the claim against the definitions actually admitted, and citing a ruling
//! never turns a failing check green.
//!
//! Every definition reference is a catalog [`MemberRef`]: an exact artifact
//! id, version, and stable member. Free text is only ever an optional
//! `note`; no rule reads it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::definition_catalog::{ArtifactKey, MemberRef};
use crate::members::{IrVersion01, present_non_null};
use crate::rule::JsonObject;
use crate::{JsonValue, ScopeRef};

/// How strong one piece of evidence is: the AGENTS.md evidence labels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Structural,
    ExhaustiveFinite,
    Bounded,
    CheckedProof,
    RuntimeEnforced,
    Sampled,
    Refuted,
    Unknown,
}

/// Where a piece of evidence lives, pinned exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EvidenceSource {
    /// A Norn artifact by its Norn1 semantic hash (a definition, a proof
    /// receipt, a test fixture).
    Artifact {
        artifact_id: String,
        version: String,
        sha256: String,
    },
    /// A file of an upstream system at an immutable release.
    Upstream {
        /// The system (`erpnext`, `shopify`).
        system: String,
        /// An immutable release, tag, or revision hash.
        release: String,
        /// A path (and optional `#anchor`) within that release.
        locator: String,
    },
}

/// One piece of evidence and its strength.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub strength: EvidenceStrength,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub note: Option<String>,
}

/// The payload of a `claim` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimIr {
    pub claim_version: IrVersion01,
    /// Stable identity, unique in a set.
    pub id: String,
    pub statement: ClaimStatement,
    /// Non-empty.
    pub evidence: Vec<Evidence>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub note: Option<String>,
}

/// What a claim states. Closed: a new statement is a checker change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ClaimStatement {
    /// A lifecycle state is a function of other facts, so no declared
    /// transition may target it.
    DerivedState {
        /// A `state` member.
        state: MemberRef,
        /// The members it is derived from (non-empty).
        from: Vec<MemberRef>,
    },
    /// Two mappings observe the same object and disagree on one member.
    Divergence { witness: DivergenceWitness },
}

/// A concrete witness of a divergence: one real object, observed by two
/// mappings, mapped to different values of `target`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DivergenceWitness {
    /// The canonical `property` member both observations map.
    pub target: MemberRef,
    pub left: Observation,
    pub right: Observation,
}

/// One source record and the object map that reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    /// The mapping where the divergence was observed. Rechecking evaluates
    /// the admitted version of this mapping id.
    pub mapping: ArtifactKey,
    /// The object map id.
    pub map: String,
    /// The source record, as the map's dataset delivers it.
    pub record: JsonObject,
}

/// Who decided a ruling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decider {
    pub principal: String,
    pub authority: DecisionAuthority,
}

/// The authority level of a decider.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAuthority {
    Owner,
    Lead,
    Agent,
}

/// Who may set a parameter a ruling introduces.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ParameterAuthority {
    Tenant,
    Jurisdiction,
    NormativeProfile,
}

/// One case of a scoped outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopedCase {
    pub scope: ScopeRef,
    pub definition: MemberRef,
}

/// When a deferred ruling is revisited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RevisitTrigger {
    /// On a date (`YYYY-MM-DD`).
    Date { on: String },
    /// When this evidence changes.
    Evidence { source: EvidenceSource },
}

/// The structured outcome of a ruling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RulingOutcome {
    /// One definition is chosen.
    Choose { definition: MemberRef },
    /// A parameter decides; its default and who may set it.
    Parameterise {
        /// The `setting` artifact holding the parameter.
        parameter: MemberRef,
        default: JsonValue,
        /// Non-empty.
        set_by: Vec<ParameterAuthority>,
    },
    /// Different definitions in disjoint scopes (two or more cases).
    Scoped { cases: Vec<ScopedCase> },
    /// Not decided yet.
    Defer { revisit: RevisitTrigger },
    /// The claim is rejected.
    Reject {},
}

/// An option considered and not chosen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RulingAlternative {
    pub outcome: RulingOutcome,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub note: Option<String>,
}

/// The payload of a `ruling` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RulingIr {
    pub ruling_version: IrVersion01,
    /// Stable identity, unique in a set.
    pub id: String,
    /// The `claim` artifacts it decides (non-empty, exact versions).
    pub resolves: Vec<ArtifactKey>,
    pub outcome: RulingOutcome,
    /// The definitions it governs (non-empty), at the versions it ruled on.
    pub affects: Vec<MemberRef>,
    pub decided_by: Decider,
    /// `YYYY-MM-DD`.
    pub decided_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<RulingAlternative>,
    /// The ruling this one supersedes.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ArtifactKey")]
    pub supersedes: Option<ArtifactKey>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub note: Option<String>,
}
