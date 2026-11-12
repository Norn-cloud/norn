//! OBS-EVIDENCE-1: resolve source lineage only from independently committed bundle inputs.
use crate::{
    CellError, ObjectRecord, StoreReader,
    engine::{SqlEngine, params},
    mapping_bundle::MappingBundle,
    reactive::{BudgetedRead, DecodeBudget},
};
use norn_ir::{FactRef, InstanceProvenance, PropertyId, RuntimeValue, SourceRecordRef};
use serde::Serialize;
use std::collections::BTreeMap;

/// Exact admitted mapping identity, including declaration digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingPin {
    /// Mapping artifact ID.
    pub artifact_id: String,
    /// Admitted artifact version.
    pub version: String,
    /// Canonical artifact digest.
    pub sha256: String,
}

/// Source evidence for one exact canonical instance version; grants no authority.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSourceProvenance {
    /// Content address of the recomputed bundle. Derived from the mapped canonical
    /// values, so a trace withholds it (`None`) unless the caller may read values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_digest: Option<String>,
    /// Semantic source-record identity emitted by the admitted mapping.
    pub source_record_ref: SourceRecordRef,
    /// Original source document identity in the sealed envelope.
    pub document_ref: SourceRecordRef,
    /// Digest of the source document payload. Derived from the payload's values, so
    /// a trace withholds it (`None`) unless the caller may read values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_digest: Option<String>,
    /// Exact mapping declaration that produced the canonical instance.
    pub mapping_pin: MappingPin,
    /// Complete field-level source/rule provenance emitted by that mapping.
    pub field_provenance: BTreeMap<PropertyId, InstanceProvenance>,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Resolve an immutable fact's consumed bundle under current host disclosure policy.
    /// The host must authorize source-provenance disclosure separately from numeric
    /// membership or canonical-value visibility. Call within its stable read snapshot.
    /// `None` means withheld content or an instance without direct mapped-source lineage.
    ///
    /// # Errors
    /// Exhausted budget, unproven fact/receipt/input, changed pins or mapping output.
    pub fn instance_source_provenance(
        &self,
        world: &str,
        fact: &FactRef,
        budget: &mut DecodeBudget,
        reveal_source: impl FnOnce(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<Option<InstanceSourceProvenance>, CellError> {
        let invalid = || CellError::MappingBundleMismatch;
        // An exhausted budget is a bound, not corrupt evidence: the route serves
        // it as 413 `core.compute.bound_exceeded`, never a partial trace.
        let exhausted = || CellError::ComputeRefused("core.compute.bound_exceeded");
        let instance = match self.object_at_version_with_budget(
            world,
            fact.object_type.as_str(),
            &fact.key,
            fact.version,
            budget,
        )? {
            BudgetedRead::Found(instance) => instance,
            BudgetedRead::Exhausted => return Err(exhausted()),
            BudgetedRead::Missing => return Err(invalid()),
        };
        if !reveal_source(&instance)? {
            return Ok(None);
        }
        if instance.object_type == norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE {
            return Ok(None);
        }
        let receipt = self
            .receipt_for_fact_version_with_budget(
                world,
                fact.object_type.as_str(),
                &fact.key,
                fact.version,
                budget,
            )?
            .ok_or_else(invalid)?;
        let Some(crate::Consumption::Observation { observation_id }) = &receipt.body.consumed
        else {
            return Ok(None);
        };
        let length: Option<i64> = self.conn.query_optional(
            &format!(
                "SELECT ({} + octet_length(hash)) FROM _norn_export_journal WHERE commit_seq = ?1",
                crate::payload_storage::EXPORT_BYTES,
            ),
            params![crate::read::to_i64(
                receipt.body.commit_seq,
                "source commit"
            )?],
            |row| row.get(0),
        )?;
        if !budget.charge_rows(1)
            || !budget.charge(crate::read::to_u64(
                length.ok_or_else(invalid)?,
                "source input bytes",
            )?)
        {
            return Err(exhausted());
        }
        let record = crate::export_journal::read_record(self.conn, receipt.body.commit_seq)?
            .ok_or_else(invalid)?;
        if record.receipt != receipt
            || crate::codec::hash_json(&record.batch)? != receipt.body.request_hash
            || record.batch.consumes != receipt.body.consumed
        {
            return Err(invalid());
        }
        let input = record.consumed_input.as_ref().ok_or_else(invalid)?;
        if input.get("observationId").and_then(RuntimeValue::as_str) != Some(observation_id) {
            return Err(invalid());
        }
        let mut observed: norn_kernel::Observed<RuntimeValue> =
            norn_ir::runtime::decode(input.get("observed").ok_or_else(invalid)?)
                .map_err(CellError::Encoding)?;
        if observed.id.0 != *observation_id {
            return Err(invalid());
        }
        if observation_id.starts_with(crate::mapping_page::PAGE_PREFIX) {
            // OBS-PAGE-1: the member whose bundle maps this object, among the
            // members the page commit consumed (retained in its input).
            let members: Vec<crate::PendingObservation> =
                norn_ir::runtime::decode(input.get("members").ok_or_else(invalid)?)
                    .map_err(CellError::Encoding)?;
            let mut found = None;
            for member in members {
                let candidate: norn_kernel::Observed<RuntimeValue> =
                    norn_ir::runtime::decode(&member.observed).map_err(CellError::Encoding)?;
                let maps = candidate
                    .input
                    .get("records")
                    .and_then(RuntimeValue::as_array)
                    .is_some_and(|records| {
                        records.iter().any(|r| {
                            r.get("key").and_then(RuntimeValue::as_str) == Some(instance.id.as_str())
                                && r.get("objectType").and_then(RuntimeValue::as_str)
                                    == Some(instance.object_type.as_str())
                        })
                    });
                if maps {
                    if candidate.id.0 != member.observation_id {
                        return Err(invalid());
                    }
                    found = Some(candidate);
                    break;
                }
            }
            observed = found.ok_or_else(invalid)?;
        }
        if observed.input.get("artifactPins").is_none() {
            return Ok(None);
        }
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        if observed.id.0 != format!("mapping-bundle:{}", bundle.digest)
            || receipt.body.definitions_version != bundle.definitions_version
        {
            return Err(invalid());
        }
        self.recheck_mapping_bundle(&bundle)?;
        source_from_bundle(&bundle, &instance).map(Some)
    }
}

fn source_from_bundle(
    bundle: &MappingBundle,
    instance: &ObjectRecord,
) -> Result<InstanceSourceProvenance, CellError> {
    let invalid = || CellError::MappingBundleMismatch;
    let mapped = bundle
        .records
        .iter()
        .find(|r| r.object_type.as_str() == instance.object_type.as_str() && r.key == instance.id)
        .ok_or_else(invalid)?;
    let invocation = mapped.invocations.first().ok_or_else(invalid)?;
    if invocation
        .input
        .iter()
        .filter(|(p, _)| p.as_str() != norn_checker::mapping::OBJECT_ID_FIELD)
        .any(|(property, value)| instance.values.get(property) != Some(value))
    {
        return Err(invalid());
    }
    let (artifact_id, version, sha256) = bundle
        .artifact_pins
        .iter()
        .find(|(id, _, _)| *id == bundle.input.mapping)
        .ok_or_else(invalid)?
        .clone();
    let text = |field: &str| {
        bundle
            .raw
            .get(field)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
            .ok_or_else(invalid)
    };
    Ok(InstanceSourceProvenance {
        bundle_digest: Some(bundle.digest.clone()),
        source_record_ref: mapped.provenance.source.clone(),
        document_ref: SourceRecordRef::new(text("source")?, text("source_id")?),
        document_digest: Some(text("payload_digest")?),
        mapping_pin: MappingPin {
            artifact_id,
            version,
            sha256,
        },
        field_provenance: mapped.property_provenance.clone(),
    })
}
