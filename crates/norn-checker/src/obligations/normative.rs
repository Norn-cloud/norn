//! LAW-NORMATIVE-CONTEXT-1: host-selected enactment is never candidate JSON.
use norn_ir::{ArtifactKind, CompositionPackRoot, PackInstanceRef};
use serde_json::{Value, json};

use super::{ChallengeError, RetainedLayers, exact_commitment};

/// Immutable host authority selection. No deserializer and no execution grant.
#[derive(Debug, Clone)]
pub struct RetainedNormativeContext {
    retained: String,
    receipt: Value,
    digest: String,
}

impl RetainedNormativeContext {
    /// Select authority only for one unambiguous, exactly pinned retained instance.
    /// The host must authorize and retain this selection outside candidate control.
    /// # Errors
    /// Refuses missing/ambiguous/stale instances, malformed identities or oversized input.
    pub fn select(
        retained: &RetainedLayers,
        enacting: &PackInstanceRef,
        authority_id: &str,
        authority_version: &str,
        rank: u32,
        enactment: u64,
    ) -> Result<Self, ChallengeError> {
        if [authority_id, authority_version]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 256)
        {
            return Err(ChallengeError::InvalidSubmission);
        }
        let mut matches = 0usize;
        for artifact in retained
            .artifacts()
            .iter()
            .filter(|a| a.kind() == ArtifactKind::BusinessPack)
        {
            let Ok(pack) = serde_json::from_value::<CompositionPackRoot>(
                serde_json::to_value(artifact.payload.to_json_value())
                    .map_err(|_error| ChallengeError::Closure)?,
            ) else {
                continue;
            };
            for instance in pack.instances.iter().flatten() {
                if instance.instance_id == enacting.instance_id {
                    if instance.pack != enacting.pack {
                        return Err(ChallengeError::InvalidSubmission);
                    }
                    matches = matches.saturating_add(1);
                }
            }
        }
        let target = retained
            .artifacts()
            .iter()
            .find(|a| {
                a.artifact_id == enacting.pack.artifact_id
                    && a.version == enacting.pack.version
                    && a.kind() == ArtifactKind::BusinessPack
            })
            .ok_or(ChallengeError::InvalidSubmission)?;
        if matches != 1
            || crate::ArtifactRef::of(target)
                .map_err(|_error| ChallengeError::Closure)?
                .sha256
                != enacting.pack.sha256
        {
            return Err(ChallengeError::InvalidSubmission);
        }
        let receipt = json!({"format":"norn.retained-normative-context/1",
            "retainedLayers":retained.exact_digest(), "enactingInstance":enacting,
            "authority":{"id":authority_id,"version":authority_version,"rank":rank,
                "enactmentOrdinal":enactment.to_string()}});
        let bytes = norn_canonical::serialize_json_verbatim(&receipt)
            .map_err(|_error| ChallengeError::LossyInput)?;
        let digest = exact_commitment(b"retained-normative-context/1", &[&bytes])?;
        Ok(Self {
            retained: retained.exact_digest().into(),
            receipt,
            digest,
        })
    }

    /// Exact host-selection receipt, distinct from ordinary structural admission.
    #[must_use]
    pub const fn receipt(&self) -> &Value {
        &self.receipt
    }

    /// Exact identity includes every retained authority/enactment input.
    #[must_use]
    pub fn exact_digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn matches(&self, retained: &RetainedLayers) -> bool {
        self.retained == retained.exact_digest()
    }
}
