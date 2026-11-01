//! OBS-EVIDENCE-1: durable finding identity and explicit authoritative population filtering.
use std::collections::BTreeSet;
use norn_ir::{DataQualityFinding, DataQualityFindingStatus, RuntimeValue};
use norn_kernel::{FactKey, TypeCatalog};
use crate::{CellError, CommitBatch, ObjectRecord, StoreReader, engine::{SqlEngine, params}, read::to_i64, system::LEDGER};

/// One authoritative page with explicit excluded-row accounting.
#[derive(Debug)]
pub struct AuthoritativePage {
    /// Eligible source records, subject to the caller's access policy.
    pub records: Vec<ObjectRecord>,
    /// Quarantined candidates excluded from this raw page.
    pub quarantine_count: usize,
    /// Last scanned raw identity; use this cursor even for an empty eligible page.
    pub scanned_through: Option<String>,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// CP2 error-severity direct laws declare quarantine; raw observations remain readable.
    /// A corrected observation recovers when these same admitted predicates pass.
    ///
    /// # Errors
    /// Unsupported law scope/semantics or exhausted evaluation fuel.
    pub fn observation_is_quarantined(&self, record: &ObjectRecord) -> Result<bool, CellError> {
        let catalog = self.quality_catalog();
        let mut remaining = norn_kernel::invariant_evaluation_budget();
        let key = FactKey::new(&record.object_type, &record.id);
        let invariants = norn_kernel::evaluate_invariants(&catalog, &key, &record.values, &mut remaining)
            .map_err(|error| CellError::DefinitionReceiptInvalid(format!("quality evaluation: {error:?}")))?;
        norn_kernel::governing_laws::evaluate(&catalog, &FactKey::new(&record.object_type, &record.id), &record.values, &mut remaining)
            .map(|failures| failures.iter().any(|failure| failure.refuses_native) || invariants.iter().any(|failure| failure.classification == norn_ir::ConstraintClass::Integrity))
            .map_err(|error| CellError::DefinitionReceiptInvalid(error.to_string()))
    }

    /// Authoritative projection selection excludes quarantined observations.
    /// Raw `object` reads preserve source truth; callers still enforce visibility/scope.
    ///
    /// # Errors
    /// Raw read failure or unresolved/unsupported governing-law evaluation.
    pub fn authoritative_object(&self, world: &str, object_type: &str, key: &str) -> Result<Option<ObjectRecord>, CellError> {
        let Some(record) = self.object(world, object_type, key)? else { return Ok(None); };
        Ok((!self.observation_is_quarantined(&record)?).then_some(record))
    }

    /// OBS-EVIDENCE-1: filter one bounded raw page for authoritative aggregates.
    /// The raw cursor advances even when every candidate is quarantined.
    ///
    /// # Errors
    /// Raw read failure or incomplete governing-law evaluation.
    pub fn authoritative_page(&self, world: &str, object_type: &str, after: Option<&str>, limit: usize) -> Result<AuthoritativePage, CellError> {
        let raw = self.list_after(world, object_type, after, limit)?;
        let mut page = AuthoritativePage {records: Vec::new(), quarantine_count: 0, scanned_through: raw.last().map(|record| record.id.clone())};
        for record in raw {
            if self.observation_is_quarantined(&record)? { page.quarantine_count += 1; }
            else { page.records.push(record); }
        }
        Ok(page)
    }

    /// Quality-review projection folds recovery over immutable finding and correction history.
    /// The original persisted finding remains unchanged.
    ///
    /// # Errors
    /// More than 64 historical candidates, failed proof lookup or unsupported laws.
    pub fn observation_findings(&self, world: &str, object_type: &str, key: &str) -> Result<Vec<DataQualityFinding>, CellError> {
        let sequences = self.conn.query(
            &format!("SELECT seq FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding' AND json_extract(event, '$.fact.subjects[0].objectType') = ?2 AND json_extract(event, '$.fact.subjects[0].key') = ?3 ORDER BY seq LIMIT 65"),
            params![world, object_type, key],
        )?;
        if sequences.len() > 64 { return Err(CellError::OutOfRange("finding review candidates")); }
        let current = self.object(world, object_type, key)?;
        if let Some(record) = &current { self.observation_is_quarantined(record)?; }
        let catalog = self.quality_catalog();
        let mut seen = BTreeSet::new();
        let mut findings = Vec::new();
        for values in sequences {
            let seq: i64 = crate::engine::SqlRow(values).get(0)?;
            if let Some(event) = self.verified_finding_at(crate::read::to_u64(seq, "finding sequence")?)? {
                let mut finding: DataQualityFinding = norn_ir::runtime::decode(event.get("fact").ok_or(CellError::OutOfRange("finding fact"))?).map_err(CellError::Encoding)?;
                if seen.insert(finding.id.clone()) {
                    if let Some(record) = &current {
                        let probe = finding.clone().with_evidence_digest(format!("sha256:{}", crate::codec::hash_json(&record.values)?))?;
                        if !norn_kernel::snapshot_observe::verify_finding_values(&catalog, &FactKey::new(object_type, key), &record.values, &probe)
                            .map_err(|error| CellError::DefinitionReceiptInvalid(error.to_string()))? {
                            finding.status = if catalog.classify_data_quality_rule(&finding.rule_ref)? == norn_kernel::DataQualityRuleClassification::Unverified {
                                DataQualityFindingStatus::Superseded
                            } else { DataQualityFindingStatus::Resolved };
                        }
                    }
                    findings.push(finding);
                }
            }
        }
        Ok(findings)
    }

    fn quality_catalog(&self) -> TypeCatalog<'_> {
        TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces())
            .with_property_types(self.model().property_types())
            .with_governing_laws(self.model().law_artifacts())
    }

    fn verified_finding_at(&self, seq: u64) -> Result<Option<RuntimeValue>, CellError> {
        let Some(entry) = self.ledger_page(seq.saturating_sub(1), 1)?.into_iter().find(|entry| entry.seq == seq) else { return Ok(None); };
        self.verified_history_event(&entry)
    }
}

/// Normalize findings inside the same capture transaction, before hashing and append.
/// The retry cutoff preserves the exact first request's normalized bytes.
pub(crate) fn deduplicate<E: SqlEngine>(reader: &StoreReader<'_, E>, batch: &mut CommitBatch, before_commit: u64) -> Result<(), CellError> {
    let events = std::mem::take(&mut batch.events);
    let mut seen = BTreeSet::new();
    for event in events {
        let finding = (event.get("op").and_then(RuntimeValue::as_str) == Some("data_quality_finding"))
            .then(|| event.get("fact").and_then(|fact| norn_ir::runtime::decode::<DataQualityFinding>(fact).ok())).flatten();
        let mut known = false;
        if let Some(finding) = finding {
            if !seen.insert(finding.id.clone()) { continue; }
            let rows = reader.conn.query(
                &format!("SELECT seq FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding' AND json_extract(event, '$.fact.id') = ?2 AND commit_seq < ?3 ORDER BY seq LIMIT 65"),
                params![&batch.world, finding.id, to_i64(before_commit, "finding cutoff")?],
            )?;
            if rows.len() > 64 { return Err(CellError::OutOfRange("finding dedup candidates")); }
            for values in rows {
                let seq: i64 = crate::engine::SqlRow(values).get(0)?;
                if reader.verified_finding_at(crate::read::to_u64(seq, "finding sequence")?)?.is_some() { known = true; break; }
            }
        }
        if !known { batch.events.push(event); }
    }
    Ok(())
}
