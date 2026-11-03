//! OBS-EVIDENCE-1: NFC storage buckets never establish opaque source identity.
use crate::{
    CellError, CellStore, StoreReader,
    engine::{SqlEngine, params},
    mapping_bundle::{MappingBundle, MappingInput},
    system::INBOX,
};
use norn_ir::{
    DataQualityFinding, DataQualityFindingStatus, DataQualityLineage, DataQualityOutcome,
    DataQualityRuleRef, DataQualitySeverity, DataQualitySubject, RuntimeValue,
};
use norn_kernel::{ObservationId, Observed};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IdentityEvidence {
    pub(crate) bundle: MappingBundle,
    // Numeric bytes survive the inbox text profile without changing the spelling proof.
    identity: [Vec<u8>; 5],
    pub(crate) finding: DataQualityFinding,
}

pub(crate) fn keys(raw: &RuntimeValue, input: &MappingInput) -> Result<[String; 5], CellError> {
    let text = |name| {
        raw.get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
            .ok_or(CellError::MappingBundleMismatch)
    };
    Ok([
        input.connector_instance.clone(),
        input.partition.clone(),
        text("source")?,
        text("source_id")?,
        text("event_id")?,
    ])
}

fn finding(
    bundle: &MappingBundle,
    identity: &[Vec<u8>; 5],
) -> Result<DataQualityFinding, CellError> {
    let evidence = norn_ir::canonical::domain_hash_hex(
        b"norn.source.identity_guard.v1\0",
        &(
            identity,
            bundle.definitions_version,
            &bundle.definitions_digest,
            &bundle.artifact_pins,
            bundle.raw.get("kind"),
            bundle.raw.get("source_version"),
            bundle.raw.get("payload_digest"),
            bundle.raw.get("redaction").filter(|v| !v.is_null()),
        ),
    )?;
    let mut result = DataQualityFinding::v2(
        DataQualityRuleRef::Builtin {
            id: "core.boundary.import_finding".into(),
            version: "1".into(),
        },
        bundle
            .records
            .iter()
            .map(|r| DataQualitySubject::new(r.object_type.clone(), &r.key))
            .collect(),
        DataQualitySeverity::Error,
        "identity normalized; possible collision",
        DataQualityOutcome::NotSatisfied,
        bundle
            .records
            .iter()
            .map(|r| DataQualityLineage::new(r.provenance.clone(), Vec::new()))
            .collect(),
        bundle.input.recorded_at.clone(),
        bundle.input.recorded_at.clone(),
        DataQualityFindingStatus::Open,
    )?
    .with_evidence_digest(evidence)?;
    for lineage in &mut result.provenance {
        lineage.observation_ids = vec![format!("mapping-identity:{}", result.id)];
    }
    Ok(result)
}

/// Capture non-NFC spelling before the inbox serializes text; refuse promotion, never merge.
pub(crate) fn capture<E: SqlEngine>(
    store: &CellStore<E>,
    raw: &Observed<RuntimeValue>,
    input: &MappingInput,
    version: u64,
) -> Result<Option<(String, bool)>, CellError> {
    let original = keys(&raw.input, input)?;
    let canonical = original.clone().map(|s| norn_ir::canonical::nfc(&s));
    if original == canonical {
        return Ok(None);
    }
    let mut mapped_raw = raw.input.clone();
    let object = mapped_raw
        .as_object_mut()
        .ok_or(CellError::MappingBundleMismatch)?;
    for (name, value) in ["source", "source_id", "event_id"]
        .into_iter()
        .zip(canonical.iter().skip(2))
    {
        object.insert(name.into(), RuntimeValue::from(value.clone()));
    }
    let mut mapped_input = input.clone();
    mapped_input.connector_instance.clone_from(&canonical[0]);
    mapped_input.partition.clone_from(&canonical[1]);
    let bundle = store
        .read()
        .mapping_bundle(&mapped_raw, &mapped_input, version)?;
    let identity = original.map(String::into_bytes);
    let finding = finding(&bundle, &identity)?;
    let id = format!("mapping-identity:{}", finding.id);
    let evidence = IdentityEvidence {
        bundle,
        identity,
        finding,
    };
    let observed = Observed {
        id: ObservationId(id.clone()),
        provenance: raw.provenance.clone(),
        input: norn_ir::runtime::encode(&evidence).map_err(CellError::Encoding)?,
    };
    let inserted = crate::store::inject_observation(
        &store.conn,
        &id,
        &norn_ir::runtime::encode(&observed).map_err(CellError::Encoding)?,
    )?;
    Ok(Some((id, inserted)))
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// OBS-EVIDENCE-1: governed reads cannot consume disputed source subjects.
    pub(crate) fn refuse_disputed_subject(
        &self,
        key: &norn_kernel::FactKey,
    ) -> Result<(), CellError> {
        if self.source_dispute_at(crate::MAIN_WORLD, key.object_type.as_str(), &key.key)? {
            return Err(CellError::SourceIdentityDisputed);
        }
        Ok(())
    }

    /// OBS-EVIDENCE-1: non-main reads cannot omit an unqualified source-evidence overlay.
    pub(crate) fn source_dispute_at(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<bool, CellError> {
        let disputed = self.indexed_source_dispute(world, object_type, key)?;
        if disputed && world != crate::MAIN_WORLD {
            return Err(CellError::SourceDisputeWorldUnqualified);
        }
        Ok(disputed)
    }

    pub(crate) fn identity_evidence(&self) -> Result<Vec<IdentityEvidence>, CellError> {
        let rows = self.conn.query(&format!("SELECT observation_id, observed FROM {INBOX} WHERE observation_id LIKE 'mapping-identity:%' ORDER BY position"), params![])?;
        let mut evidence = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let stored_id: String = row.get(0)?;
            let encoded: String = row.get(1)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
            let guard: IdentityEvidence =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_mapping_bundle(&guard.bundle)?;
            let text: Vec<_> = guard
                .identity
                .iter()
                .map(|b| {
                    std::str::from_utf8(b)
                        .map(str::to_owned)
                        .map_err(|_invalid_utf8| CellError::MappingBundleMismatch)
                })
                .collect::<Result<_, _>>()?;
            let normalized: Vec<_> = text.iter().map(|s| norn_ir::canonical::nfc(s)).collect();
            if text == normalized
                || normalized.as_slice() != keys(&guard.bundle.raw, &guard.bundle.input)?
                || guard.finding != finding(&guard.bundle, &guard.identity)?
                || observed.id.0 != stored_id
                || stored_id != format!("mapping-identity:{}", guard.finding.id)
            {
                return Err(CellError::MappingBundleMismatch);
            }
            evidence.push(guard);
        }
        Ok(evidence)
    }

    /// OBS-EVIDENCE-1: an ambiguous NFC bucket grants no convergence authority.
    pub(crate) fn identity_is_disputed(&self, bundle: &MappingBundle) -> Result<bool, CellError> {
        let key = keys(&bundle.raw, &bundle.input)?.map(|s| norn_ir::canonical::nfc(&s));
        self.indexed_identity_dispute(&key)
    }

    pub(crate) fn identity_findings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.indexed_source_findings(object_type, key, true)
    }
}
