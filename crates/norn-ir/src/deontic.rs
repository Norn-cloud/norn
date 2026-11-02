//! Structured normative declarations. Data only; these are not authorization grants.
use crate::{IrVersion01, LawIR, ZenExpression};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// LAW-NORMATIVE-1: normative grades stay in the existing Law artifact family.
/// Both forms retain the complete law, grade and source under the artifact pin.
/// Decoding this document confers no grounding, execution or authorization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(transform = crate::members::one_of_forms)]
pub enum LawDocument {
    /// Existing protected safety law, with unchanged wire form.
    Safety(Box<LawIR>),
    /// Reviewed normative declaration; never erase its grade or source metadata.
    Structured(Box<StructuredLaw>),
}

/// Structured form of a `law` payload. Authority rank and enactment are absent:
/// only the trusted retained enacting instance/layer may supply those inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredLaw {
    pub requirement: StructuredRequirement,
}

/// Reviewed structured language; predicates retain the existing ZEN semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeonticIR {
    pub deontic_version: IrVersion01,
    pub authorities: Vec<AuthoritySource>,
    pub rules: Vec<DeonticRule>,
}

/// A declared signed source, not a claim that its signature has been verified.
/// Verification and tenant selection belong to the authority admission boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoritySource {
    pub id: String,
    pub version: String,
    pub document_sha256: String,
    pub signer: String,
    pub signature: String,
    pub source: RuleSourceSpan,
}

/// Source JSON pointer plus an optional half-open UTF-8 byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleSourceSpan {
    pub artifact_id: String,
    pub pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<RuleByteRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleByteRange {
    #[serde(deserialize_with = "crate::members::deserialize_u32")]
    pub start: u32,
    #[serde(deserialize_with = "crate::members::deserialize_u32")]
    pub end: u32,
}

/// The normative grade is separate from the temporal wrapper in `law`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeonticModality {
    Obligation,
    Permission,
    Prohibition,
}

/// A law template with explicit authority, exceptions and preferences.
/// `law.condition`, `law.timing` and `law.response` supply typed slots;
/// `law.modality` must agree with the normative grade before law lowering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeonticRule {
    pub law: LawIR,
    pub modality: DeonticModality,
    pub authority_id: String,
    pub authority_version: String,
    pub source: RuleSourceSpan,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<RuleException>,
    /// Explicit rule identities preferred to this rule; not numeric authority ranking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defeated_by: Vec<String>,
}

/// A reviewed conditional exception. No implicit necessity or legal judgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleException {
    pub id: String,
    pub condition: ZenExpression,
    pub source: RuleSourceSpan,
}

/// Structured authoring over the existing law and normative source model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredRequirement {
    pub norm: DeonticRule,
    pub timing: RequirementTiming,
    pub wording: RequirementWording,
}

/// Closed safety fragment. No event activation, clocks, fairness or progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequirementTiming {
    /// Condition and response are evaluated at every applicable committed state.
    EveryCommittedState,
    /// Condition and response refer to the same proposed commit's state.
    /// This is not an edge trigger or a promise to respond in a later commit.
    SameCommit,
}

/// Reviewed bilingual templates; identifiers and expressions remain in `norm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementWording {
    pub scope: RequirementText,
    pub activation: RequirementText,
    pub component: RequirementText,
    pub response: RequirementText,
}

/// Reviewed English and Arabic display, never executable free prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequirementText {
    pub en: String,
    pub ar: String,
}
