//! OBS-EVIDENCE-1: durable finding identity and explicit authoritative population filtering.
use crate::{
    CellError, CommitBatch, ObjectRecord, StoreReader,
    engine::{SqlEngine, SqlValue, params},
    read::to_i64,
    system::LEDGER,
};
use norn_ir::{DataQualityFinding, DataQualityFindingStatus, ObjectTypeName, RuntimeValue};
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
    /// QUERY-ENGINE-1: receipt subjects are invalidation hints, never finding authority.
    /// The shared record read rechecks findings using the same first-subject lookup.
    pub(crate) fn question_finding_keys(
        &self,
        receipt: &crate::Receipt,
    ) -> Result<BTreeSet<String>, CellError> {
        let mut keys = BTreeSet::new();
        if receipt.body.event_count == 0 {
            return Ok(keys);
        }
        let rows = self.conn.query(
            &format!("SELECT json_extract(event, '$.fact.subjects[0].key') FROM {LEDGER} WHERE commit_seq = ?1 AND world = ?2 AND json_extract(event, '$.op') = 'data_quality_finding' ORDER BY seq"),
            params![to_i64(receipt.body.commit_seq, "finding receipt")?, &receipt.body.world],
        )?;
        for mut row in rows {
            if let Some(SqlValue::Text(key)) = row.pop() {
                keys.insert(key);
            }
        }
        Ok(keys)
    }

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
            findings.extend(self.held_link_findings(object_type, key)?);
        }
        Ok(findings)
    }

    /// DATA-QUARANTINE: resolve live exclusion pins for `record`.
    pub(crate) fn question_findings(
        &self,
        world: &str,
        record: &ObjectRecord,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.question_findings_inner(world, record, None)
    }

    /// DATA-QUARANTINE / ASOF-KNOWLEDGE-1: resolve exclusion pins at a specific
    /// knowledge frontier. The caller must supply a receipt-authenticated
    /// record at `frontier`; linked governing-law recovery fails closed rather
    /// than reading live links.
    pub(crate) fn question_findings_at_cutoff(
        &self,
        world: &str,
        record: &ObjectRecord,
        frontier: u64,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.question_findings_inner(world, record, Some(frontier))
    }

    fn question_findings_inner(
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
                frontier,
            )?
            .into_iter()
            .filter_map(|(finding, quarantines)| {
                (quarantines && finding.status == DataQualityFindingStatus::Open).then_some(finding)
            })
            .collect();
        self.source_dispute_at(world, &record.object_type, &record.id)?;
        if world == crate::MAIN_WORLD {
            let mut source = self.source_conflict_findings(&record.object_type, &record.id)?;
            source.extend(self.identity_findings(&record.object_type, &record.id)?);
            if frontier.is_some() && !source.is_empty() {
                return Err(CellError::ComputeRefused(
                    "core.world.source_dispute_unqualified",
                ));
            }
            findings.extend(source);
        }
        findings.retain(|f| f.status == DataQualityFindingStatus::Open);
        findings.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(findings)
    }

    /// Whether the authenticated snapshot of `current`'s version, replayed
    /// through every intervening authenticated definitions install to the
    /// active schema, produces the exact row `current` as read today.
    ///
    /// Replays `materialize_historical_values` step by step from the emitting
    /// receipt's `definitions_version` through the active definitions version
    /// (the same authenticated projection `verify_modified_before` uses), so
    /// every column has one exact expected value: no union of historical and
    /// active defaults, and no blanket null exemption for newly added columns.
    fn proves(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        current: &ObjectRecord,
    ) -> Result<bool, CellError> {
        let evidence = self.fact_version_evidence(world, object_type, key, current.version)?;
        let snapshot = evidence.snapshot();
        if (
            &snapshot.object_type,
            &snapshot.key,
            &snapshot.scope,
            snapshot.version,
        ) != (
            &current.object_type,
            &current.id,
            &current.scope,
            current.version,
        ) {
            return Ok(false);
        }
        let Some(mut materialized) = snapshot.values.clone() else {
            return Ok(false);
        };
        let first = evidence.receipt().body.definitions_version;
        let last = self.active_definitions_version()?.unwrap_or(0);
        if first > last {
            return Ok(false);
        }
        if first == last {
            if first != 0
                && self
                    .installed_definitions(Some(first))?
                    .is_none_or(|installed| installed.model_hash != snapshot.model_hash)
            {
                return Ok(false);
            }
            return Ok(
                snapshot.model_hash == self.model().model_hash() && materialized == current.values
            );
        }
        if first != 0 && self.retained_definition_model(first)?.model_hash() != snapshot.model_hash
        {
            return Ok(false);
        }
        let object_type_name = ObjectTypeName(snapshot.object_type.clone());
        let mut previous_hash = snapshot.model_hash.clone();
        for version in first.saturating_add(1)..=last {
            let model = self.retained_definition_model(version)?;
            if version == last && model.model_hash() != self.model().model_hash() {
                return Ok(false);
            }
            if model.model_hash() == previous_hash {
                continue;
            }
            materialized = crate::history::materialize_historical_values(
                &model,
                &object_type_name,
                &materialized,
            )?;
            model.model_hash().clone_into(&mut previous_hash);
        }
        Ok(materialized == current.values)
    }

    fn review_findings(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<(DataQualityFinding, bool)>, CellError> {
        self.check_history("observation quality findings")?;
        let current = self.object(world, object_type, key)?;
        if let Some(record) = current.as_ref()
            && !self.proves(world, object_type, key, record)?
        {
            return Err(CellError::UnprovenCorrection);
        }
        self.review_findings_at(world, object_type, key, current.as_ref(), None)
    }

    // DATA-QUARANTINE / ASOF-KNOWLEDGE-1: recovery uses the selected incarnation.
    fn review_findings_at(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        current: Option<&ObjectRecord>,
        frontier: Option<u64>,
    ) -> Result<Vec<(DataQualityFinding, bool)>, CellError> {
        // S8 (#1551 round 5): scanning `_norn_ledger` for findings requires
        // verified history so deleted covered events cannot un-quarantine records.
        self.check_history("observation quality findings")?;
        let mut seen = BTreeSet::new();
        let mut findings = Vec::new();
        let mut after = 0i64;
        loop {
            let sequences = self.conn.query(
                &format!("SELECT seq FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding' AND json_extract(event, '$.fact.subjects[0].objectType') = ?2 AND json_extract(event, '$.fact.subjects[0].key') = ?3 AND seq > ?4 AND (?5 IS NULL OR commit_seq <= ?5) ORDER BY seq LIMIT 64"),
                params![world, object_type, key, after, frontier.map(|f| to_i64(f, "finding frontier")).transpose()?],
            )?;
            if sequences.is_empty() {
                break;
            }
            if let Some(record) = current
                && !self.proves(world, object_type, key, record)?
            {
                return Err(CellError::UnprovenCorrection);
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
                        if let Some(record) = current {
                            // Scope governing-law recovery to the finding's own rule so a
                            // pure-property finding on a mixed-law entity never touches an
                            // unrelated linked law at a historical frontier.
                            let scoped_laws = scoped_finding_laws(model.law_artifacts(), &finding);
                            let eval_catalog = TypeCatalog::new(model.object_types())
                                .with_interfaces(model.interfaces())
                                .with_property_types(model.property_types())
                                .with_governing_laws(&scoped_laws);
                            if !self.finding_is_violated(
                                &eval_catalog,
                                world,
                                object_type,
                                key,
                                record,
                                &finding,
                                frontier,
                            )? {
                                finding.status = if class
                                    == norn_kernel::DataQualityRuleClassification::Unverified
                                {
                                    DataQualityFindingStatus::Superseded
                                } else {
                                    DataQualityFindingStatus::Resolved
                                };
                            }
                        }
                        findings.push((finding, quarantines));
                    }
                }
            }
        }
        Ok(findings)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "keeps finding recovery dispatch separate from ledger scan"
    )]
    fn finding_is_violated(
        &self,
        catalog: &TypeCatalog<'_>,
        world: &str,
        object_type: &str,
        key: &str,
        record: &ObjectRecord,
        finding: &DataQualityFinding,
        frontier: Option<u64>,
    ) -> Result<bool, CellError> {
        let fact_key = FactKey::new(object_type, key);
        match frontier {
            None => norn_kernel::snapshot_observe::finding_rule_is_violated(
                catalog,
                &fact_key,
                &record.values,
                finding,
                &mut crate::link_laws::CurrentLinks {
                    reader: self,
                    world,
                    subject: FactKey::new(record.object_type.as_str(), key),
                },
            )
            .map_err(|error| CellError::QualityEvaluation(Box::new(error))),
            Some(cutoff_frontier) => {
                let mut links = crate::link_laws::FailClosedAsOfLinks::new(cutoff_frontier);
                match norn_kernel::snapshot_observe::finding_rule_is_violated(
                    catalog,
                    &fact_key,
                    &record.values,
                    finding,
                    &mut links,
                ) {
                    Ok(violated) => Ok(violated),
                    Err(_error) if links.attempted() => Err(CellError::ComputeRefused(
                        norn_checker::metric::compute::UNSUPPORTED_ENGINE,
                    )),
                    Err(error) => Err(CellError::QualityEvaluation(Box::new(error))),
                }
            }
        }
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

/// Filter retained governing-law artifacts to the single law referenced by
/// `finding`, or none when `finding` references a built-in or entity invariant.
fn scoped_finding_laws(
    laws: &[norn_ir::ArtifactEnvelope],
    finding: &DataQualityFinding,
) -> Vec<norn_ir::ArtifactEnvelope> {
    match &finding.rule_ref {
        norn_ir::DataQualityRuleRef::Law {
            artifact_id,
            law_id,
            ..
        } => laws
            .iter()
            .filter(|law| {
                law.artifact_id == *artifact_id
                    || law.payload.get("id").and_then(serde_json::Value::as_str)
                        == Some(law_id.as_str())
            })
            .cloned()
            .collect(),
        norn_ir::DataQualityRuleRef::Invariant { .. }
        | norn_ir::DataQualityRuleRef::Builtin { .. } => Vec::new(),
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
