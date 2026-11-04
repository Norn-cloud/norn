//! Conjunctive specialisation provenance; no cache, solver or proof promotion.
use norn_ir::{ArtifactKind, LawIR, LawModality, PropertyTypeName};
use serde_json::Value;
use std::collections::BTreeSet;

use super::evidence::{EvidenceIdentity, EvidencePins, EvidenceRecord};
use super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource, exact_commitment};

/// Versioned, conservative conjunction-only reuse rule.
pub const RULE: &str = "specialisation-conjunction/1";

/// Independently retained runner identity and its corresponding evidence record.
/// The caller supplies trusted pins/digest, never values taken from the candidate.
pub struct PriorEvidence<'a> {
    /// Controller-owned run pins.
    pub pins: &'a EvidencePins,
    /// Controller-owned checked result digest.
    pub result_sha256: &'a str,
    /// Untrusted record, checked against the other fields before reuse.
    pub record: &'a EvidenceRecord,
}

/// Origin of a carried record. Its assurance stays exactly that of the base run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusedEvidence {
    /// Target goal index in the freshly admitted specialisation.
    pub goal_index: usize,
    /// Complete prior evidence record commitment, not merely its result digest.
    pub base_evidence_sha256: String,
    /// Exact candidate/base source commitment.
    pub extension_sha256: String,
    /// Commitment to rule, source evidence, extension and target challenge/goal.
    pub sha256: String,
}

/// A live-admitted challenge and a partition of its required proof work.
/// This is identity provenance only: customer checks still run in admission.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecialisationChallenge {
    /// Complete generated target; no obligation is deleted from the inventory.
    pub challenge: Challenge,
    /// Valid carried records under [`RULE`], with no increase in assurance.
    pub reused: Vec<ReusedEvidence>,
    /// Every goal not covered by a carried record must be checked afresh.
    pub recheck: Vec<usize>,
}

impl ApprovedContract {
    /// Admit and generate the complete target, then apply the narrow reuse rule.
    /// Only new laws are supported in this profile. Entity refinements, new writers
    /// and other composition shapes conservatively require a full re-check.
    /// No caller-provided closure or eligibility assertion is trusted.
    /// # Errors
    /// Same structural, protected identity and resource errors as `challenge`.
    pub fn challenge_under_specialisation(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<PropertyTypeName>,
        base_contract: &Self,
        base_source: &DefinitionSource,
        prior: &[PriorEvidence<'_>],
    ) -> Result<SpecialisationChallenge, ChallengeError> {
        let challenge = self.challenge(source, registry)?;
        let goal_count = goals(&challenge).len();
        let mut result = SpecialisationChallenge {
            recheck: (0..goal_count).collect(),
            reused: Vec::new(),
            challenge,
        };
        // Bound input work and never use a failed reconstruction to skip work.
        if prior.iter().fold(0_usize, |total, evidence| {
            total
                .saturating_add(evidence.record.challenge_json.len())
                .saturating_add(evidence.record.result_json.len())
                .saturating_add(evidence.pins.bounds_json.len())
        }) > 8_388_608
            || prior.len() > 8_192
            || !base_source.base.is_empty()
            || source.base_sources != base_source.receipt_sources
            || source
                .artifacts
                .iter()
                .any(|a| a.kind() != ArtifactKind::Law)
            || self.semantics != base_contract.semantics
            || self.assumptions != base_contract.assumptions
            || self.assurance != base_contract.assurance
        {
            return Ok(result);
        }
        let Ok(base) = base_contract.challenge(base_source, registry) else {
            return Ok(result);
        };
        if base.body()["writerStatements"] != result.challenge.body()["writerStatements"] {
            return Ok(result);
        }
        for evidence in prior {
            if base.evidence_identity(evidence.pins, evidence.result_sha256, evidence.record)
                != EvidenceIdentity::Current
            {
                continue;
            }
            let Some(old) = usize::try_from(evidence.pins.goal_index)
                .ok()
                .and_then(|index| goals(&base).get(index))
            else {
                continue;
            };
            let kind = old["goal"]["kind"].as_str().unwrap_or("");
            if !matches!(kind, "initialization" | "preservation") {
                continue;
            }
            let Some(law_index) = old["law"].as_u64().and_then(|i| usize::try_from(i).ok()) else {
                continue;
            };
            let Some(law_identity) = base.body()["lawStatements"]
                .as_array()
                .and_then(|a| a.get(law_index))
            else {
                continue;
            };
            let safety = base_source
                .artifacts
                .iter()
                .find(|a| Some(a.artifact_id.as_str()) == law_identity["artifactId"].as_str())
                .and_then(|a| {
                    serde_json::to_value(a.payload.to_json_value())
                        .ok()
                        .and_then(|v| serde_json::from_value::<LawIR>(v).ok())
                })
                .is_some_and(|law| {
                    matches!(law.modality, LawModality::Always {} | LawModality::Never {})
                });
            if !safety {
                continue;
            }
            let Some(target_law) = result.challenge.body()["lawStatements"]
                .as_array()
                .and_then(|laws| laws.iter().position(|law| law == law_identity))
            else {
                continue;
            };
            let mut target = old.clone();
            target["law"] = target_law.into();
            target["statement"]["law"] = target_law.into();
            let Some(index) = goals(&result.challenge)
                .iter()
                .position(|goal| goal == &target)
            else {
                continue;
            };
            if result.reused.iter().any(|r| r.goal_index == index) {
                continue;
            }
            let bytes = norn_canonical::serialize_json_verbatim(evidence.record)
                .map_err(|_| ChallengeError::Closure)?;
            let base_evidence_sha256 = exact_commitment(b"specialisation-evidence", &[&bytes])?;
            let extension_sha256 = source.exact_digest.clone();
            let index_bytes = index.to_string();
            let sha256 = exact_commitment(
                b"specialisation-reuse",
                &[
                    RULE.as_bytes(),
                    base_evidence_sha256.as_bytes(),
                    extension_sha256.as_bytes(),
                    result.challenge.sha256().as_bytes(),
                    index_bytes.as_bytes(),
                ],
            )?;
            result.reused.push(ReusedEvidence {
                goal_index: index,
                base_evidence_sha256,
                extension_sha256,
                sha256,
            });
        }
        result
            .recheck
            .retain(|index| !result.reused.iter().any(|r| r.goal_index == *index));
        Ok(result)
    }
}

fn goals(challenge: &Challenge) -> &[Value] {
    challenge.body()["obligations"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}
