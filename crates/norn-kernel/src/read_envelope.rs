//! READ-LOCAL-1: a frontier belongs to one cell/world and one pinned context.
//! Tokens describe read evidence; they are not authorization grants or global snapshots.

use crate::authz::CommitBinding;
use norn_ir::cell_id::CellId;
use serde::{Deserialize, Serialize};

/// One cell's local frontier, never a network snapshot vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalFrontier {
    /// Durable logical cell identity.
    pub cell: CellId,
    /// Bound execution world.
    pub world: String,
    /// Revision in this cell's commit order.
    pub revision: u64,
}

/// Retained definitions used to interpret the answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionPin {
    /// Local installed-definition generation, zero for the initial model.
    pub version: u64,
    /// Semantic model digest.
    pub model_hash: String,
    /// Checker admission receipt, when installed definitions retain one.
    pub admission_receipt: Option<String>,
}

/// Existing [`CommitBinding`] pins, copied without creating another authority model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityPin {
    /// Authorized action.
    pub action_id: String,
    /// Resource type.
    pub resource_kind: String,
    /// Resource identity.
    pub resource_id: String,
    /// Resource artifact version.
    pub artifact_version: Option<String>,
    /// Policy identity.
    pub policy_id: String,
    /// Policy generation.
    pub policy_version: String,
    /// Resource generation.
    pub resource_version: String,
    /// Existing authorization epoch.
    pub authorization_epoch: String,
    /// Policy evaluation time.
    pub evaluated_at_logical_time: String,
}

impl From<&CommitBinding> for AuthorityPin {
    fn from(binding: &CommitBinding) -> Self {
        Self {
            action_id: binding.action_id.clone(),
            resource_kind: binding.resource_kind.clone(),
            resource_id: binding.resource_id.clone(),
            artifact_version: binding.artifact_version.clone(),
            policy_id: binding.policy_id.clone(),
            policy_version: binding.policy_version.clone(),
            resource_version: binding.resource_version.clone(),
            authorization_epoch: binding.authorization_epoch.clone(),
            evaluated_at_logical_time: binding.evaluated_at_logical_time.clone(),
        }
    }
}

/// Whole context compared when reusing an answer or token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadContext {
    /// Single-entry local frontier.
    pub frontier: LocalFrontier,
    /// Exact interpretation pins.
    pub definition_pin: DefinitionPin,
    /// Existing policy/resource/epoch binding.
    pub authority_pin: AuthorityPin,
}

/// Whether the declared local question was fully answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    /// Complete for this local question and authorized projection.
    Complete,
    /// Some requested local coverage is missing.
    Partial,
}

/// Freshness relative to the declared local context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSemantics {
    /// Exact at the envelope's local frontier.
    Exact,
    /// Retained answer at its stated frontier; no currentness claim.
    Stale,
}

/// Consumer's required semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadRequirement {
    /// Require complete, exact local evidence.
    Exact,
    /// Permit explicitly stale or partial evidence at its stated context.
    AllowStale,
}

/// Explicit read-envelope version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadFormat {
    /// First single-cell read envelope.
    #[serde(rename = "norn.cell.read/v1")]
    V1,
}

/// Descriptive read token; possession grants no access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadToken {
    /// Wire contract version.
    pub format: ReadFormat,
    /// Whole local context, also retained for stale answers.
    pub context: ReadContext,
    /// Coverage of the declared question.
    pub completeness: Completeness,
    /// Exact versus explicitly stale semantics.
    pub semantics: ReadSemantics,
}

/// The token cannot satisfy the requested context or semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("read token context or exactness requirement does not match")]
pub struct ReadTokenMismatch;

impl ReadToken {
    /// Compare every context pin against trusted expected server context.
    ///
    /// # Errors
    /// Any context mismatch, or stale/incomplete evidence where exact is required.
    pub fn validate(
        &self,
        expected: &ReadContext,
        requirement: ReadRequirement,
    ) -> Result<(), ReadTokenMismatch> {
        if crate::read_token_check::accepts(
            self.context == *expected,
            self.semantics == ReadSemantics::Exact && self.completeness == Completeness::Complete,
            requirement == ReadRequirement::Exact,
        ) {
            Ok(())
        } else {
            Err(ReadTokenMismatch)
        }
    }
}

/// A result with one explicitly local frontier and its interpretation/authority pins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadEnvelope<T> {
    /// Versioned local evidence and semantics.
    pub token: ReadToken,
    /// Authorized answer to the declared local question.
    pub value: T,
}
