//! PROOF-BIND-1: serializable identity records; no execution or proof authority.

use serde::{Deserialize, Serialize};

/// Identity of the actual runner, supplied by its trusted controller.
/// This record does not authenticate a runner or attest its execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceTool {
    /// Tool/profile identity.
    pub id: String,
    /// Exact tool version.
    pub version: String,
    /// Lowercase SHA-256 of the implementing binary/source closure.
    pub implementation_sha256: String,
}

/// Complete run identity. Fields are untrusted wire data, not proof capabilities.
/// The challenge digest binds law, writer/mapping closure, unit interpretation,
/// generator/semantics profiles, assumptions and every generated statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidencePins {
    /// Exact checker-owned challenge identity.
    pub challenge_sha256: String,
    /// Index in that challenge's generated obligations, never a supplied theorem.
    pub goal_index: u32,
    /// Exact generated goal, including its statement and assessment.
    pub goal_sha256: String,
    /// Actual runner identity.
    pub tool: EvidenceTool,
    /// Exact JSON object containing the runner's bounds and assumptions.
    pub bounds_json: String,
    /// Explicit deterministic seed (zero is permitted).
    pub seed: u64,
}

/// A result and its source identity. Deserializing or matching it grants no status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRecord {
    /// Complete expected run identity.
    pub pins: EvidencePins,
    /// Exact serialized challenge, retaining all law/writer/unit/mapping pins.
    pub challenge_json: String,
    /// Exact UTF-8 JSON result bytes, including whitespace and number spelling.
    /// The checked driver owns their meaning and assurance level.
    pub result_json: String,
    /// Raw SHA-256 of `result_json`'s UTF-8 bytes.
    pub result_sha256: String,
}
