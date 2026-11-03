//! OBS-EVIDENCE-1: durable finding identity and explicit authoritative population filtering.
use crate::{
    CellError, CommitBatch, ObjectRecord, StoreReader,
    engine::{SqlEngine, params},
    read::to_i64,
    system::LEDGER,
};
use norn_ir::{DataQualityFinding, DataQualityFindingStatus, RuntimeValue};
use norn_kernel::{FactKey, TypeCatalog};
use std::collections::BTreeSet;

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
    /// OBS-EVIDENCE-1: quarantine is any open integrity finding under its retained model.
    ///
    /// # Errors
    /// Missing/corrupt historical definitions or unsupported recovery evaluation.
    pub fn observation_is_quarantined(
        &self,
        world: &str,
        record: &ObjectRecord,
    ) -> Result<bool, CellError> {
        if self.source_dispute_at(world, &record.object_type, &record.id)? {
            return Ok(true);
        }
        Ok(self
            .review_findings(world, &record.object_type, &record.id)?
            .iter()
            .any(|(finding, quarantines)| {
                *quarantines && finding.status == DataQualityFindingStatus::Open
            }))
    }

    /// Authoritative projection selection excludes quarantined observations.
    /// Raw `object` reads preserve source truth; callers still enforce visibility/scope.
    ///
    /// # Errors
    /// Raw read failure or unresolved/unsupported governing-law evaluation.
    pub fn authoritative_object(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<Option<ObjectRecord>, CellError> {
        if world != crate::MAIN_WORLD {
            self.source_dispute_at(world, object_type, key)?;
        }
        let Some(record) = self.object(world, object_type, key)? else {
            return Ok(None);
        };
        Ok((!self.observation_is_quarantined(world, &record)?).then_some(record))
    }

    /// OBS-EVIDENCE-1: filter one bounded raw page for authoritative aggregates.
    /// The raw cursor advances even when every candidate is quarantined.
    ///
    /// # Errors
    /// Raw read failure or incomplete governing-law evaluation.
    pub fn authoritative_page(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<AuthoritativePage, CellError> {
        let raw = self.list_after(world, object_type, after, limit)?;
        let mut page = AuthoritativePage {
            records: Vec::new(),
            quarantine_count: 0,
            scanned_through: raw.last().map(|record| record.id.clone()),
        };
        for record in raw {
            if self.observation_is_quarantined(world, &record)? {
                page.quarantine_count = page
                    .quarantine_count
                    .checked_add(1)
                    .ok_or(CellError::OutOfRange("quarantine count"))?;
            } else {
                page.records.push(record);
            }
        }
        Ok(page)
    }

    /// Quality-review projection folds recovery over immutable finding and correction history.
    /// The original persisted finding remains unchanged.
    ///
    /// # Errors
    /// Failed proof lookup or unsupported laws.
    pub fn observation_findings(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        if world != crate::MAIN_WORLD {
            self.source_dispute_at(world, object_type, key)?;
        }
        let mut findings: Vec<_> = self
            .review_findings(world, object_type, key)?
            .into_iter()
            .map(|(finding, _)| finding)
            .collect();
        if world == crate::MAIN_WORLD {
            findings.extend(self.source_conflict_findings(object_type, key)?);
            findings.extend(self.identity_findings(object_type, key)?);
        }
        Ok(findings)
    }

    /// DATA-QUARANTINE: resolve real exclusion pins at the selected knowledge frontier.
    /// The caller must supply a receipt-authenticated record at that frontier.
    pub(crate) fn question_findings(
        &self,
        world: &str,
        record: &ObjectRecord,
        frontier: Option<u64>,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        let mut findings: Vec<_> = self
            .review_findings_at(
                world,
                &record.object_type,
                &record.id,
                Some(record),
                frontier.unwrap_or(u64::MAX),
            )?
            .into_iter()
            .filter_map(|(finding, quarantines)| {
                (quarantines && finding.status == DataQualityFindingStatus::Open).then_some(finding)
            })
            .collect();
        if self.source_dispute_at(world, &record.object_type, &record.id)? {
            if frontier.is_some() {
                return Err(CellError::ComputeRefused(
                    "core.world.source_dispute_unqualified",
                ));
            }
            findings.extend(self.source_conflict_findings(&record.object_type, &record.id)?);
            findings.extend(self.identity_findings(&record.object_type, &record.id)?);
        }
        findings.retain(|f| f.status == DataQualityFindingStatus::Open);
        findings.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(findings)
    }

    fn review_findings(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<(DataQualityFinding, bool)>, CellError> {
        let current = self.object(world, object_type, key)?;
        self.review_findings_at(world, object_type, key, current.as_ref(), u64::MAX)
    }

    // DATA-QUARANTINE / ASOF-KNOWLEDGE-1: recovery uses the selected incarnation.
    fn review_findings_at(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        current: Option<&ObjectRecord>,
        frontier: u64,
    ) -> Result<Vec<(DataQualityFinding, bool)>, CellError> {
        if let Some(record) = current
            && self
                .object_at_version(world, object_type, key, record.version)?
                .as_ref()
                != Some(record)
        {
            return Err(CellError::UnprovenCorrection);
        }
        let mut seen = BTreeSet::new();
        let mut findings = Vec::new();
        let mut after = 0i64;
        loop {
            let sequences = self.conn.query(
                &format!("SELECT seq FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding' AND json_extract(event, '$.fact.subjects[0].objectType') = ?2 AND json_extract(event, '$.fact.subjects[0].key') = ?3 AND seq > ?4 AND commit_seq <= ?5 ORDER BY seq LIMIT 64"),
                params![world, object_type, key, after, to_i64(frontier.min(i64::MAX as u64), "finding frontier")?],
            )?;
            if sequences.is_empty() {
                break;
            }
            for values in sequences {
                let seq: i64 = crate::engine::SqlRow(values).get(0)?;
                after = seq;
                if let Some((event, definitions_version)) =
                    self.verified_finding_at(crate::read::to_u64(seq, "finding sequence")?)?
                {
                    let mut finding: DataQualityFinding = norn_ir::runtime::decode(
                        event
                            .get("fact")
                            .ok_or(CellError::OutOfRange("finding fact"))?,
                    )
                    .map_err(CellError::Encoding)?;
                    if seen.insert(finding.id.clone()) {
                        let model = self.retained_definition_model(definitions_version)?;
                        let catalog = TypeCatalog::new(model.object_types())
                            .with_interfaces(model.interfaces())
                            .with_property_types(model.property_types())
                            .with_governing_laws(model.law_artifacts());
                        let class = catalog.classify_data_quality_rule(&finding.rule_ref)?;
                        let quarantines = class
                            == norn_kernel::DataQualityRuleClassification::Integrity
                            || (class == norn_kernel::DataQualityRuleClassification::Builtin
                                && finding.severity == norn_ir::DataQualitySeverity::Error);
                        if let Some(record) = current
                            && !norn_kernel::snapshot_observe::finding_rule_is_violated(
                                &catalog,
                                &FactKey::new(object_type, key),
                                &record.values,
                                &finding,
                            )
                            .map_err(|error| CellError::QualityEvaluation(Box::new(error)))?
                        {
                            finding.status = if class
                                == norn_kernel::DataQualityRuleClassification::Unverified
                            {
                                DataQualityFindingStatus::Superseded
                            } else {
                                DataQualityFindingStatus::Resolved
                            };
                        }
                        findings.push((finding, quarantines));
                    }
                }
            }
        }
        Ok(findings)
    }

    fn verified_finding_at(&self, seq: u64) -> Result<Option<(RuntimeValue, u64)>, CellError> {
        let Some(entry) = self
            .ledger_page(seq.saturating_sub(1), 1)?
            .into_iter()
            .find(|entry| entry.seq == seq)
        else {
            return Ok(None);
        };
        let receipt = self
            .receipt_at(entry.commit_seq)?
            .ok_or(CellError::OutOfRange("finding receipt"))?;
        if receipt.body.definitions_version == 0 {
            return Err(CellError::UnpinnedFinding);
        }
        let Some(event) = self.verified_history_event(&entry)? else {
            return Ok(None);
        };
        Ok(Some((event, receipt.body.definitions_version)))
    }
}

/// Normalize findings inside the same capture transaction, before hashing and append.
/// The retry cutoff preserves the exact first request's normalized bytes.
pub(crate) fn deduplicate<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &mut CommitBatch,
    before_commit: u64,
) -> Result<(), CellError> {
    let events = std::mem::take(&mut batch.events);
    let mut seen = BTreeSet::new();
    for event in events {
        let finding = (event.get("op").and_then(RuntimeValue::as_str)
            == Some("data_quality_finding"))
        .then(|| {
            event
                .get("fact")
                .and_then(|fact| norn_ir::runtime::decode::<DataQualityFinding>(fact).ok())
        })
        .flatten();
        let mut known = false;
        if let Some(finding) = finding {
            if !seen.insert(finding.id.clone()) {
                continue;
            }
            let mut after = 0i64;
            loop {
                let rows = reader.conn.query(
                    &format!("SELECT seq FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding' AND json_extract(event, '$.fact.id') = ?2 AND commit_seq < ?3 AND seq > ?4 ORDER BY seq LIMIT 64"),
                    params![&batch.world, &finding.id, to_i64(before_commit, "finding cutoff")?, after],
                )?;
                if rows.is_empty() {
                    break;
                }
                for values in rows {
                    after = crate::engine::SqlRow(values).get(0)?;
                    if reader
                        .verified_finding_at(crate::read::to_u64(after, "finding sequence")?)?
                        .is_some()
                    {
                        known = true;
                        break;
                    }
                }
                if known {
                    break;
                }
            }
        }
        if !known {
            batch.events.push(event);
        }
    }
    Ok(())
}
