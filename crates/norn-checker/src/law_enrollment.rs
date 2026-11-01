//! Receipt-bound origins for laws retained from installed composition packs.
//! This is provenance, not evidence that a law or mapping has been proved.

use std::collections::BTreeMap;

use norn_ir::{ArtifactEnvelope, ArtifactKind, BusinessPackRoot, LawIR};
use serde_json::{Value, json};

use crate::receipt::{ArtifactRef, ReceiptError};

/// Exact origin of an inherited law; the digest pins the artifact version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InheritedFrom {
    /// Installed composition pack identity.
    pub pack_id: String,
    /// Installed composition pack version.
    pub pack_version: String,
    /// Artifact envelope identity.
    pub artifact_id: String,
    /// Semantic envelope digest under the checker's existing Norn1 profile.
    pub artifact_digest: String,
    /// Stable law member identity inside the artifact.
    pub law_id: String,
}

/// One inherited law in a checker receipt's enrollment projection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LawEnrollment {
    /// The retained pack, artifact digest and stable member identity.
    pub inherited_from: InheritedFrom,
}

impl LawEnrollment {
    /// Versioned origin fields consumed by enrollment and receipt readers.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let origin = &self.inherited_from;
        json!({"inherited_from": {
            "pack_id": origin.pack_id, "pack_version": origin.pack_version,
            "artifact_id": origin.artifact_id, "artifact_digest": origin.artifact_digest,
            "law_id": origin.law_id,
        }})
    }

    /// Human-readable provenance, with no proof-level promotion.
    #[must_use]
    pub fn description(&self) -> String {
        let origin = &self.inherited_from;
        format!(
            "inherited from law {} of pack {} version {}",
            origin.law_id, origin.pack_id, origin.pack_version
        )
    }
}

pub(crate) fn inherited_laws(
    base: &[ArtifactEnvelope],
) -> Result<Vec<LawEnrollment>, ReceiptError> {
    let invalid = |detail: String| ReceiptError::InvalidStoredReceipt { detail };
    if !base
        .iter()
        .any(|artifact| artifact.kind() == ArtifactKind::BusinessPack)
    {
        return Ok(Vec::new());
    }
    let mut laws = BTreeMap::new();
    for law in base
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Law)
    {
        let definition: LawIR = serde_json::to_value(law.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|error| invalid(format!("invalid retained law: {error}")))?;
        let identity = (&law.artifact_id, &law.version);
        if laws
            .insert(identity, (definition.id, ArtifactRef::of(law)?.sha256))
            .is_some()
        {
            return Err(invalid(format!(
                "retained law is ambiguous: {}",
                law.artifact_id
            )));
        }
    }
    let mut enrollment = Vec::new();
    for artifact in base
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::BusinessPack)
    {
        let pack: BusinessPackRoot = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|error| invalid(format!("invalid retained pack: {error}")))?;
        let BusinessPackRoot::Composition(pack) = pack else {
            continue;
        };
        for member in &pack.artifacts {
            if member.kind != norn_ir::business_pack::PackArtifactKind::Law {
                continue;
            }
            let Some((law_id, digest)) = laws.get(&(&member.artifact_id, &member.version)) else {
                // An unresolved reference supplies no retained law to enroll.
                // It must not block an unrelated admission or imply an origin.
                continue;
            };
            enrollment.push(LawEnrollment {
                inherited_from: InheritedFrom {
                    pack_id: pack.pack_id.clone(),
                    pack_version: pack.pack_version.clone(),
                    artifact_id: member.artifact_id.clone(),
                    artifact_digest: digest.clone(),
                    law_id: law_id.clone(),
                },
            });
        }
    }
    enrollment.sort();
    enrollment.dedup();
    Ok(enrollment)
}
