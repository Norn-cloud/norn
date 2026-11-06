//! FEED-CURSOR-1 / OBS-EVIDENCE-1: preserve immutable source-event conflicts.
use crate::{
    CellError, CellStore, StoreReader,
    engine::{SqlEngine, params},
    mapping_bundle::MappingBundle,
    system::INBOX,
};
use norn_ir::{
    DataQualityFinding, DataQualityFindingStatus, DataQualityLineage, DataQualityOutcome,
    DataQualityRuleRef, DataQualitySeverity, DataQualitySubject, RuntimeValue,
};
use norn_kernel::{ObservationId, Observed};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Quarantined capture evidence; this never supplies canonical write authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingConflict {
    /// Immutable first capture, retained even after its inbox entry is consumed.
    pub accepted_bundle_id: String,
    /// Complete changed evidence, including pins and mapping diagnostics.
    pub incoming: MappingBundle,
    /// Replay-stable boundary finding; resolution needs explicit review.
    pub finding: DataQualityFinding,
}

fn field<'a>(bundle: &'a MappingBundle, name: &str) -> Result<&'a str, CellError> {
    bundle
        .raw
        .get(name)
        .and_then(RuntimeValue::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or(CellError::MappingBundleMismatch)
}

fn content(bundle: &MappingBundle) -> Result<String, CellError> {
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.source.content.v1\0",
        &(
            field(bundle, "kind")?,
            field(bundle, "source_version")?,
            field(bundle, "payload_digest")?,
            bundle.raw.get("redaction").filter(|v| !v.is_null()),
        ),
    )?)
}

impl<E: SqlEngine> StoreReader<'_, E> {
    fn source_event_bundles(
        &self,
        incoming: &MappingBundle,
    ) -> Result<Vec<MappingBundle>, CellError> {
        self.source_event_mapping_bundles(
            &incoming.input.connector_instance,
            &incoming.input.partition,
            field(incoming, "source")?,
            field(incoming, "source_id")?,
            field(incoming, "event_id")?,
        )
    }

    /// Find a conflict against the first captured source event, including consumed captures.
    pub(crate) fn mapping_conflict(
        &self,
        incoming: &MappingBundle,
    ) -> Result<Option<MappingConflict>, CellError> {
        let new = content(incoming)?;
        let Some(accepted) = self.source_event_bundles(incoming)?.into_iter().next() else {
            return Ok(None);
        };
        let accepted_id = format!("mapping-bundle:{}", accepted.digest);
        let old = content(&accepted)?;
        if old == new {
            return Ok(None);
        }
        let evidence = norn_ir::canonical::domain_hash_hex(
            b"norn.source.conflict.v1\0",
            &(
                &incoming.input.connector_instance,
                &incoming.input.partition,
                field(incoming, "source")?,
                field(incoming, "source_id")?,
                field(incoming, "event_id")?,
                old,
                new,
            ),
        )?;
        let subjects = accepted
            .records
            .iter()
            .chain(&incoming.records)
            .map(|r| DataQualitySubject::new(r.object_type.clone(), &r.key))
            .collect();
        let observation_ids = vec![
            accepted_id.clone(),
            format!("mapping-bundle:{}", incoming.digest),
        ];
        let finding = DataQualityFinding::v2(
            DataQualityRuleRef::Builtin {
                id: "core.boundary.import_finding".into(),
                version: "1".into(),
            },
            subjects,
            DataQualitySeverity::Error,
            "conflicting immutable source event content",
            DataQualityOutcome::NotSatisfied,
            accepted
                .records
                .iter()
                .chain(&incoming.records)
                .map(|r| DataQualityLineage::new(r.provenance.clone(), observation_ids.clone()))
                .collect(),
            incoming.input.recorded_at.clone(),
            incoming.input.recorded_at.clone(),
            DataQualityFindingStatus::Open,
        )?
        .with_evidence_digest(evidence)?;
        Ok(Some(MappingConflict {
            accepted_bundle_id: accepted_id,
            incoming: incoming.clone(),
            finding,
        }))
    }

    /// OBS-EVIDENCE-1: disputed source identities quarantine every affected live subject.
    pub(crate) fn source_conflict_findings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.indexed_source_findings(object_type, key, false)
    }

    pub(crate) fn verified_source_conflicts(&self) -> Result<Vec<MappingConflict>, CellError> {
        let rows = self.conn.query(&format!("SELECT observation_id, observed FROM {INBOX} WHERE observation_id LIKE 'mapping-conflict:%' ORDER BY position"), params![])?;
        let mut conflicts = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let stored_id: String = row.get(0)?;
            let encoded: String = row.get(1)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
            let conflict: MappingConflict =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_mapping_conflict(&conflict)?;
            if observed.id.0 != stored_id
                || stored_id != format!("mapping-conflict:{}", conflict.finding.id)
            {
                return Err(CellError::MappingBundleMismatch);
            }
            conflicts.push(conflict);
        }
        Ok(conflicts)
    }

    /// Recompute boundary conflict evidence before disclosure under the caller's source policy.
    ///
    /// # Errors
    /// Corrupt evidence, missing retained pins or disagreement refuses bundle mismatch.
    pub fn recheck_mapping_conflict(&self, conflict: &MappingConflict) -> Result<(), CellError> {
        self.recheck_mapping_bundle(&conflict.incoming)?;
        if self.mapping_conflict(&conflict.incoming)?.as_ref() != Some(conflict) {
            return Err(CellError::MappingBundleMismatch);
        }
        Ok(())
    }
}

/// FEED-CURSOR-1: comparison and evidence capture share the caller's write transaction.
pub(crate) fn capture<E: SqlEngine>(
    store: &CellStore<E>,
    raw: &Observed<RuntimeValue>,
    bundle: &MappingBundle,
) -> Result<(String, bool), CellError> {
    let (id, input) = if let Some(conflict) = store.read().mapping_conflict(bundle)? {
        (
            format!("mapping-conflict:{}", conflict.finding.id),
            norn_ir::runtime::encode(&conflict).map_err(CellError::Encoding)?,
        )
    } else {
        (
            format!("mapping-bundle:{}", bundle.digest),
            norn_ir::runtime::encode(bundle).map_err(CellError::Encoding)?,
        )
    };
    let observed = Observed {
        id: ObservationId(id.clone()),
        provenance: raw.provenance.clone(),
        input,
    };
    let inserted = crate::store::inject_observation(
        &store.conn,
        &id,
        &norn_ir::runtime::encode(&observed).map_err(CellError::Encoding)?,
    )?;
    Ok((id, inserted))
}

/// OBS-DISPUTE-LOOKUP-1: membership comes only from rechecked complete evidence.
/// The index is disposable and valid only at an unchanged SQLite connection snapshot.
#[derive(Debug, Default)]
pub(crate) struct SourceDisputeIndex {
    revision: Option<(u64, u64)>,
    initialized: bool,
    subjects: BTreeMap<(String, String), Vec<(bool, DataQualityFinding)>>,
    identities: BTreeSet<[String; 5]>,
    conflicts: BTreeSet<[String; 5]>,
    roots_by_key: BTreeMap<String, BTreeSet<String>>,
    pub(crate) claim_endpoints: BTreeMap<String, BTreeMap<(u64, String), u64>>,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    fn build_source_dispute_index(&self) -> Result<SourceDisputeIndex, CellError> {
        for _attempt in 0..3 {
            let revision = self.conn.read_cache_revision()?;
            let collected = self.collect_source_dispute_index();
            if self.conn.read_cache_revision()? != revision {
                continue;
            }
            let mut index = collected?;
            index.revision = revision;
            index.initialized = true;
            return Ok(index);
        }
        Err(CellError::SourceEvidenceChanged)
    }

    fn collect_source_dispute_index(&self) -> Result<SourceDisputeIndex, CellError> {
        let mut index = SourceDisputeIndex::default();
        for conflict in self.verified_source_conflicts()? {
            index.conflicts.insert(
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)?
                    .map(|key| norn_ir::canonical::nfc(&key)),
            );
            self.index_source_finding(&mut index, &conflict.incoming, false, &conflict.finding)?;
        }
        for guard in self.identity_evidence()? {
            index.identities.insert(crate::mapping_identity::keys(
                &guard.bundle.raw,
                &guard.bundle.input,
            )?);
            self.index_source_finding(&mut index, &guard.bundle, true, &guard.finding)?;
        }
        let rows = self.conn.query(&format!("SELECT observation_id, consumed_commit_seq, octet_length(observed), json_extract(record.value, '$.key'), record.value FROM {INBOX}, json_each(observed, '$.input.records') AS record WHERE observation_id LIKE 'mapping-bundle:%' AND consumed_commit_seq IS NOT NULL AND (json_array_length(json_extract(record.value, '$.links')) > 0 OR json_array_length(json_extract(record.value, '$.clearedLinks')) > 0)"), params![])?;
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let id: String = row.get(0)?;
            let seq = crate::read::to_u64(row.get(1)?, "claim sequence")?;
            let bytes = crate::read::to_u64(row.get(2)?, "claim bytes")?;
            let record: serde_json::Value = serde_json::from_str(&row.get::<String>(4)?)?;
            let mut endpoints = BTreeSet::new();
            if record
                .get("clearedLinks")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|links| !links.is_empty())
            {
                endpoints.insert(row.get::<String>(3)?);
            }
            for link in record
                .get("links")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                for field in ["key", "targetKey"] {
                    if let Some(key) = link.get(field).and_then(serde_json::Value::as_str) {
                        endpoints.insert(key.to_owned());
                    }
                }
            }
            for key in endpoints {
                index
                    .claim_endpoints
                    .entry(key)
                    .or_default()
                    .insert((seq, id.clone()), bytes);
            }
        }
        Ok(index)
    }

    fn index_source_finding(
        &self,
        index: &mut SourceDisputeIndex,
        incoming: &MappingBundle,
        identity: bool,
        finding: &DataQualityFinding,
    ) -> Result<(), CellError> {
        let related = self.source_event_bundles(incoming)?;
        let mut subjects = BTreeSet::new();
        for bundle in related.iter().chain(std::iter::once(incoming)) {
            for record in &bundle.records {
                subjects.insert((
                    self.retained_mapping_root(
                        bundle.definitions_version,
                        record.object_type.as_str(),
                    )?,
                    record.key.clone(),
                ));
            }
        }
        for subject in subjects {
            index
                .roots_by_key
                .entry(subject.1.clone())
                .or_default()
                .insert(subject.0.clone());
            index
                .subjects
                .entry(subject)
                .or_default()
                .push((identity, finding.clone()));
        }
        Ok(())
    }

    pub(crate) fn with_source_dispute_index<T>(
        &self,
        read: impl FnOnce(&SourceDisputeIndex) -> T,
    ) -> Result<T, CellError> {
        let Some(cache) = self.source_disputes else {
            return Ok(read(&self.build_source_dispute_index()?));
        };
        let revision = self.conn.read_cache_revision()?;
        if revision.is_none() && !self.source_disputes_transaction {
            return Ok(read(&self.build_source_dispute_index()?));
        }
        if !cache.borrow().initialized || cache.borrow().revision != revision {
            let rebuilt = self.build_source_dispute_index()?;
            *cache.borrow_mut() = rebuilt;
        }
        Ok(read(&cache.borrow()))
    }

    pub(crate) fn indexed_source_findings(
        &self,
        object_type: &str,
        key: &str,
        identity: bool,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        let roots = self.source_dispute_roots(crate::MAIN_WORLD, object_type, key)?;
        self.with_source_dispute_index(|index| {
            let mut seen = BTreeSet::new();
            let mut findings = Vec::new();
            for root in roots.as_ref().into_iter().flatten().chain(
                index
                    .roots_by_key
                    .get(key)
                    .into_iter()
                    .flatten()
                    .filter(|_root| roots.is_none()),
            ) {
                for (kind, finding) in index
                    .subjects
                    .get(&(root.clone(), key.to_owned()))
                    .into_iter()
                    .flatten()
                {
                    if *kind == identity && seen.insert(finding.id.clone()) {
                        findings.push(finding.clone());
                    }
                }
            }
            findings
        })
    }

    pub(crate) fn indexed_source_dispute(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<bool, CellError> {
        let roots = self.source_dispute_roots(world, object_type, key)?;
        self.with_source_dispute_index(|index| match &roots {
            Some(roots) => roots
                .iter()
                .any(|root| index.subjects.contains_key(&(root.clone(), key.to_owned()))),
            None => index.roots_by_key.contains_key(key),
        })
    }

    /// OBS-EVIDENCE-1: a dispute covers the accepted event as well as its candidate.
    pub(crate) fn mapping_event_is_disputed(
        &self,
        bundle: &MappingBundle,
    ) -> Result<bool, CellError> {
        let keys = crate::mapping_identity::keys(&bundle.raw, &bundle.input)?
            .map(|key| norn_ir::canonical::nfc(&key));
        self.with_source_dispute_index(|index| {
            index.identities.contains(&keys) || index.conflicts.contains(&keys)
        })
    }
}
