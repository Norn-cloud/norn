//! PROOF-BIND-1: exact evidence identity, never a proof verdict or authority.

use norn_canonical::JsonValue;
use sha2::{Digest, Sha256};

use super::{Challenge, exact_commitment};

/// PROOF-STALE: old evidence is unknown for changed inputs, never passing evidence.
pub const STALE_EVIDENCE: &str = "core.laws.stale_evidence";
const PROFILE_LIMIT: usize = 65_536;
const RESULT_LIMIT: usize = 1_048_576;

pub use norn_ir::verification::evidence::{EvidencePins, EvidenceRecord, EvidenceTool};

/// Pin freshness only. Neither variant means proved, checked or bounded-pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceIdentity {
    /// All identities match the current checker-owned goal and controller pins.
    Current,
    /// Stale, malformed or substituted evidence: its assurance remains unknown.
    Unknown,
}

impl EvidenceIdentity {
    /// Stable diagnostic for unknown identity.
    #[must_use]
    pub const fn code(self) -> Option<&'static str> {
        match self {
            Self::Current => None,
            Self::Unknown => Some(STALE_EVIDENCE),
        }
    }
}

/// Why a bounded identity record could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceError {
    /// Invalid tool identity, digest or JSON profile/result.
    Malformed,
    /// No generated goal at the requested index.
    MissingGoal,
    /// A profile/result exceeds its byte bound.
    Limit,
    /// Pins belong to a different source/goal.
    Stale,
}

impl std::fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed evidence identity or payload",
            Self::MissingGoal => "generated evidence goal is absent",
            Self::Limit => "evidence identity byte limit exceeded",
            Self::Stale => STALE_EVIDENCE,
        })
    }
}
impl std::error::Error for EvidenceError {}

fn json(text: &str, limit: usize) -> Result<JsonValue, EvidenceError> {
    if text.len() > limit {
        return Err(EvidenceError::Limit);
    }
    norn_canonical::parse_json_verbatim(text.as_bytes()).map_err(|_error| EvidenceError::Malformed)
}

fn digest_valid(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn tool_valid(tool: &EvidenceTool) -> bool {
    tool.id.len().saturating_add(tool.version.len()) <= 1_024
        && !tool.id.trim().is_empty()
        && !tool.version.trim().is_empty()
        && digest_valid(&tool.implementation_sha256)
}

impl Challenge {
    /// PROOF-BIND-1: derive run pins only from an existing generated obligation.
    /// Controller-supplied tool/bounds must describe the real execution; this
    /// function checks identity/shape, not that a tool actually ran.
    /// # Errors
    /// Missing goal, malformed tool/bounds or profile byte limit.
    pub fn evidence_pins(
        &self,
        goal_index: u32,
        tool: EvidenceTool,
        bounds_json: &str,
        seed: u64,
    ) -> Result<EvidencePins, EvidenceError> {
        if !tool_valid(&tool)
            || !matches!(json(bounds_json, PROFILE_LIMIT)?, JsonValue::Object(fields) if !fields.is_empty())
        {
            return Err(EvidenceError::Malformed);
        }
        let goal = self
            .body()
            .get("obligations")
            .and_then(serde_json::Value::as_array)
            .and_then(|goals| {
                usize::try_from(goal_index)
                    .ok()
                    .and_then(|index| goals.get(index))
            })
            .ok_or(EvidenceError::MissingGoal)?;
        let bytes = norn_canonical::serialize_json_verbatim(goal)
            .map_err(|_error| EvidenceError::Malformed)?;
        let goal_sha256 =
            exact_commitment(b"evidence-goal", &[&bytes]).map_err(|_error| EvidenceError::Limit)?;
        Ok(EvidencePins {
            challenge_sha256: self.sha256().to_owned(),
            goal_index,
            goal_sha256,
            tool,
            bounds_json: bounds_json.to_owned(),
            seed,
        })
    }

    /// Bind exact result bytes without interpreting or promoting their status.
    /// # Errors
    /// Stale pins, invalid JSON or a result larger than 1 MiB.
    pub fn record_evidence(
        &self,
        pins: &EvidencePins,
        result_json: &str,
    ) -> Result<EvidenceRecord, EvidenceError> {
        if !tool_valid(&pins.tool) {
            return Err(EvidenceError::Malformed);
        }
        if self.evidence_pins(
            pins.goal_index,
            pins.tool.clone(),
            &pins.bounds_json,
            pins.seed,
        )? != *pins
        {
            return Err(EvidenceError::Stale);
        }
        json(result_json, RESULT_LIMIT)?;
        let bytes = norn_canonical::serialize_json_verbatim(self.body())
            .map_err(|_error| EvidenceError::Malformed)?;
        let challenge_json = String::from_utf8(bytes).map_err(|_error| EvidenceError::Malformed)?;
        Ok(EvidenceRecord {
            pins: pins.clone(),
            challenge_json,
            result_json: result_json.to_owned(),
            result_sha256: format!("{:x}", Sha256::digest(result_json.as_bytes())),
        })
    }

    /// PROOF-STALE: compare with freshly derived controller pins. Any mismatch
    /// yields unknown, including result edits accompanied by stale status text.
    /// The expected result digest comes independently from the trusted checked
    /// driver, never from the untrusted record being inspected. Even Current
    /// authenticates neither the runner nor its claimed result.
    #[must_use]
    pub fn evidence_identity(
        &self,
        current: &EvidencePins,
        expected_result_sha256: &str,
        record: &EvidenceRecord,
    ) -> EvidenceIdentity {
        if !tool_valid(&current.tool)
            || !digest_valid(expected_result_sha256)
            || record.result_sha256 != expected_result_sha256
        {
            return EvidenceIdentity::Unknown;
        }
        let Ok(expected) = self.evidence_pins(
            current.goal_index,
            current.tool.clone(),
            &current.bounds_json,
            current.seed,
        ) else {
            return EvidenceIdentity::Unknown;
        };
        if expected != *current || record.pins != expected {
            return EvidenceIdentity::Unknown;
        }
        let Ok(rebuilt) = self.record_evidence(current, &record.result_json) else {
            return EvidenceIdentity::Unknown;
        };
        if rebuilt == *record {
            EvidenceIdentity::Current
        } else {
            EvidenceIdentity::Unknown
        }
    }
}
