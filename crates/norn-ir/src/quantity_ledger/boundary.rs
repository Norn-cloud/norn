//! Wire types for the machine-checkable boundary between conservation checking
//! and authority commit. The boundary functions and the sealed
//! `LedgerPackSeal` live in `norn_checker::quantity_ledger` (T1-CHK-1).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{EvaluatedLedgerLeg, LedgerParameterValue};

/// Caller proposal at the proof-to-authority boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityPostingIntent {
    pub contract_version: String,
    pub scope_key: String,
    pub transition_id: String,
    pub idempotency_key: String,
    pub expected_owner_version: u64,
    pub pack_id: String,
    pub pack_sha256: String,
    pub checker_version: String,
    pub template_id: String,
    pub parameters: Vec<LedgerParameterValue>,
}

/// Deterministic proof-bearing posting payload produced after exact evaluation.
///
/// This value is evidence, not ambient authority. A process boundary that
/// deserializes it must recompute it from the installed
/// `norn_checker::quantity_ledger::LedgerPackSeal` and original
/// [`QuantityPostingIntent`] rather than trusting its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityPostingCommitment {
    pub contract_version: String,
    pub scope_key: String,
    pub transition_id: String,
    pub idempotency_key: String,
    pub expected_owner_version: u64,
    pub pack_id: String,
    pub pack_sha256: String,
    pub checker_version: String,
    pub template_id: String,
    pub certificate_sha256s: Vec<String>,
    pub parameter_sha256: String,
    pub evaluation_sha256: String,
    pub posting_input_sha256: String,
    pub outbox_message_id: String,
    pub legs: Vec<EvaluatedLedgerLeg>,
}

/// Durable idempotency record required by a completed authority commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityIdempotencyRecord {
    pub scope_key: String,
    pub idempotency_key: String,
    pub posting_input_sha256: String,
    pub stable_result_sha256: String,
}

/// Durable outbox record required by a completed authority commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityOutboxRecord {
    pub message_id: String,
    pub transition_id: String,
    pub posting_input_sha256: String,
}

/// Reopened-store observation used by crash and conformance tests.
///
/// Every correctness-critical component is optional so a test can represent
/// corrupt partial persistence. The verifier accepts only the all-absent and
/// all-present-and-matching states.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityAuthorityCommitObservation {
    pub quantity_transaction_input_sha256: Option<String>,
    pub entries_input_sha256: Option<String>,
    pub balance_heads_input_sha256: Option<String>,
    pub balance_history_input_sha256: Option<String>,
    pub entity_events_input_sha256: Option<String>,
    pub owner_version: Option<u64>,
    pub business_effect_input_sha256: Option<String>,
    pub canonical_event_input_sha256: Option<String>,
    pub checkpoint_input_sha256: Option<String>,
    pub idempotency_result: Option<QuantityIdempotencyRecord>,
    pub outbox: Option<QuantityOutboxRecord>,
}

/// Result of checking reopened authority state against a posting commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuantityCommitStatus {
    NotCommitted,
    Committed { stable_result_sha256: String },
}

/// Idempotency decision for a new proposal against an existing stable result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuantityReplayDecision {
    New,
    Replay { stable_result_sha256: String },
    Conflict,
}
