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
    let kind = field(bundle, "kind")?;
    let source_version = field(bundle, "source_version")?;
    let payload_digest = field(bundle, "payload_digest")?;
    let redaction = bundle.raw.get("redaction").filter(|v| !v.is_null());
    // Canonicalize omitted and explicit default wire values so repeating the
    // default (`full_snapshot` / `lexicographical`) never causes a spurious
    // conflict while differing effective interpretations always conflict.
    let feed_mode = bundle
        .raw
        .get("feed_mode")
        .filter(|v| !v.is_null() && v.as_str() != Some(norn_ir::FeedMode::FullSnapshot.as_str()));
    let version_kind = bundle.raw.get("version_kind").filter(|v| {
        !v.is_null() && v.as_str() != Some(norn_ir::VersionKind::Lexicographical.as_str())
    });
    let canonical_version = version_kind
        .and_then(RuntimeValue::as_str)
        .and_then(norn_ir::VersionKind::parse_wire)
        .and_then(|vk| norn_ir::SourceVersion::parse(vk, source_version).ok())
        .map(|sv| sv.canonical_id());
    let source_version = canonical_version.as_deref().unwrap_or(source_version);
    if feed_mode.is_none() && version_kind.is_none() {
        return Ok(norn_ir::canonical::domain_hash_hex(
            b"norn.source.content.v1\0",
            &(kind, source_version, payload_digest, redaction),
        )?);
    }
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.source.content.v1\0",
        &(
            kind,
            source_version,
            payload_digest,
            redaction,
            feed_mode,
            version_kind,
        ),
    )?)
}

/// Result of comparing an incoming bundle against previously captured bundles
/// for the same `(connector_instance, partition, source, source_id, event_id)`.
pub(crate) enum SourceEventCapture {
    /// No conflicting prior bundle and no equivalent capture under the same
    /// definitions/mapping/derivation; capture as a fresh bundle.
    Fresh,
    /// Equivalent replay (`content` matches under the same definitions, mapping,
    /// dataset, and derivation); reuse the existing bundle ID without inserting
    /// a new inbox row that could overwrite newer state.
    Replay(String),
    /// Conflicting content for the same immutable source event (`FEED-CONFLICT`).
    Conflict(Box<MappingConflict>),
}

fn same_capture_pin(existing: &MappingBundle, incoming: &MappingBundle) -> bool {
    existing.definitions_version == incoming.definitions_version
        && existing.definitions_digest == incoming.definitions_digest
        && existing.input.mapping == incoming.input.mapping
        && existing.input.dataset == incoming.input.dataset
        && existing.derivation == incoming.derivation
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

    pub(crate) fn classify_source_event(
        &self,
        incoming: &MappingBundle,
    ) -> Result<SourceEventCapture, CellError> {
        let new = content(incoming)?;
        let existing = self.source_event_bundles(incoming)?;
        let Some(accepted) = existing.first() else {
            return Ok(SourceEventCapture::Fresh);
        };
        let accepted_id = format!("mapping-bundle:{}", accepted.digest);
        let old = content(accepted)?;
        if old == new {
            if incoming.reprocesses.is_none()
                && let Some(matched) = existing.iter().find(|b| same_capture_pin(b, incoming))
            {
                return Ok(SourceEventCapture::Replay(format!(
                    "mapping-bundle:{}",
                    matched.digest
                )));
            }
            return Ok(SourceEventCapture::Fresh);
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
        Ok(SourceEventCapture::Conflict(Box::new(MappingConflict {
            accepted_bundle_id: accepted_id,
            incoming: incoming.clone(),
            finding,
        })))
    }

    /// Find a conflict against the first captured source event, including consumed captures.
    pub(crate) fn mapping_conflict(
        &self,
        incoming: &MappingBundle,
    ) -> Result<Option<MappingConflict>, CellError> {
        match self.classify_source_event(incoming)? {
            SourceEventCapture::Conflict(conflict) => Ok(Some(*conflict)),
            SourceEventCapture::Fresh | SourceEventCapture::Replay(_) => Ok(None),
        }
    }

    /// OBS-EVIDENCE-1: disputed source identities quarantine every affected live subject.
    pub(crate) fn source_conflict_findings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.indexed_source_findings(object_type, key, false)
    }

    /// Every captured conflict with its inbox position, in capture order.
    pub(crate) fn verified_source_conflicts(
        &self,
    ) -> Result<Vec<(u64, MappingConflict)>, CellError> {
        let rows = self.conn.query(&format!("SELECT position, observation_id, observed FROM {INBOX} WHERE observation_id LIKE 'mapping-conflict:%' ORDER BY position"), params![])?;
        let mut conflicts = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let position = crate::read::to_u64(row.get(0)?, "inbox position")?;
            let stored_id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
            let conflict: MappingConflict =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_mapping_conflict(&conflict)?;
            if observed.id.0 != stored_id
                || stored_id != format!("mapping-conflict:{}", conflict.finding.id)
            {
                return Err(CellError::MappingBundleMismatch);
            }
            conflicts.push((position, conflict));
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
    let (id, input) = match store.read().classify_source_event(bundle)? {
        SourceEventCapture::Replay(existing_id) => return Ok((existing_id, false)),
        SourceEventCapture::Conflict(conflict) => (
            format!("mapping-conflict:{}", conflict.finding.id),
            norn_ir::runtime::encode(conflict.as_ref()).map_err(CellError::Encoding)?,
        ),
        SourceEventCapture::Fresh => (
            format!("mapping-bundle:{}", bundle.digest),
            norn_ir::runtime::encode(bundle).map_err(CellError::Encoding)?,
        ),
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
    /// Disputed source events (NFC keys), conflicts and identity disputes alike.
    events: BTreeMap<[String; 5], EventDispute>,
    roots_by_key: BTreeMap<String, BTreeSet<String>>,
}

/// OBS-LINK-DISPUTE-1: the dispute rows covering one source event.
#[derive(Debug, Clone)]
pub(crate) struct EventDispute {
    /// The inbox position of the earliest row, whose `first_seen` dates a
    /// held-link finding. It orders evidence only: the freeze itself reads
    /// whether a row exists when the commit runs.
    pub(crate) position: u64,
    /// The dispute rows' inbox ids (`mapping-conflict:` / `mapping-identity:`).
    pub(crate) rows: BTreeSet<String>,
    /// When the earliest row's finding was first seen.
    pub(crate) first_seen: norn_ir::provenance::UtcTimestamp,
}

impl SourceDisputeIndex {
    fn dispute(
        &mut self,
        keys: [String; 5],
        position: u64,
        row: String,
        finding: &DataQualityFinding,
    ) {
        let entry = self.events.entry(keys).or_insert_with(|| EventDispute {
            position,
            rows: BTreeSet::new(),
            first_seen: finding.first_seen.clone(),
        });
        if position < entry.position {
            entry.position = position;
            entry.first_seen = finding.first_seen.clone();
        }
        entry.rows.insert(row);
    }
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
        for (position, conflict) in self.verified_source_conflicts()? {
            index.dispute(
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)?
                    .map(|key| norn_ir::canonical::nfc(&key)),
                position,
                format!("mapping-conflict:{}", conflict.finding.id),
                &conflict.finding,
            );
            self.index_source_finding(&mut index, &conflict.incoming, false, &conflict.finding)?;
        }
        for (position, guard) in self.identity_evidence()? {
            index.dispute(
                crate::mapping_identity::keys(&guard.bundle.raw, &guard.bundle.input)?,
                position,
                format!("mapping-identity:{}", guard.finding.id),
                &guard.finding,
            );
            self.index_source_finding(&mut index, &guard.bundle, true, &guard.finding)?;
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

    /// D102: identities of `root` with any source or identity finding, the
    /// candidates an analytical answer checks for exclusion.
    pub(crate) fn source_finding_keys(&self, root: &str) -> Result<BTreeSet<String>, CellError> {
        self.with_source_dispute_index(|index| {
            index
                .subjects
                .keys()
                .filter(|(kind, _)| kind == root)
                .map(|(_, key)| key.clone())
                .collect()
        })
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

    /// QUERY-ENGINE-1: bounded member cache tokens from the rechecked source index.
    /// Tokens include roots and complete finding contents; they grant no authority.
    pub(crate) fn question_source_pins(
        &self,
        members: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, String>, CellError> {
        if members.is_empty() {
            return Ok(BTreeMap::new());
        }
        self.with_source_dispute_index(|index| {
            let mut grouped = BTreeMap::<&str, Vec<_>>::new();
            for ((root, key), findings) in &index.subjects {
                if members.contains(key) {
                    grouped
                        .entry(key.as_str())
                        .or_default()
                        .push((root, findings));
                }
            }
            grouped
                .into_iter()
                .map(|(key, findings)| Ok((key.to_owned(), crate::codec::hash_json(&findings)?)))
                .collect()
        })?
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
        self.with_source_dispute_index(|index| index.events.contains_key(&keys))
    }

    /// The dispute rows covering `bundle`'s source event, if any.
    pub(crate) fn mapping_event_dispute(
        &self,
        bundle: &MappingBundle,
    ) -> Result<Option<EventDispute>, CellError> {
        let keys = crate::mapping_identity::keys(&bundle.raw, &bundle.input)?
            .map(|key| norn_ir::canonical::nfc(&key));
        self.with_source_dispute_index(|index| index.events.get(&keys).cloned())
    }

    /// Datasets of `mapping_id` that still hold unresolved quarantined inbox
    /// rows (`mapping-bundle:` without a strictly newer landed correction under
    /// the declared [`norn_ir::VersionKind`] ordering, or any unconsumed
    /// `mapping-conflict:` / `mapping-identity:`).
    ///
    /// # Errors
    /// Store query errors.
    pub fn unresolved_quarantined_datasets(
        &self,
        mapping_id: &str,
    ) -> Result<BTreeSet<String>, CellError> {
        let mut datasets = BTreeSet::new();
        let disputes_sql = format!(
            "SELECT DISTINCT json_extract(\"observed\", '$.input.incoming.input.dataset') \
             FROM \"{INBOX}\" \
             WHERE \"consumed_commit_seq\" IS NULL \
               AND \"observation_id\" LIKE 'mapping-conflict:%' \
               AND json_extract(\"observed\", '$.input.incoming.input.mapping') = ?1 \
             UNION \
             SELECT DISTINCT json_extract(\"observed\", '$.input.bundle.input.dataset') \
             FROM \"{INBOX}\" \
             WHERE \"consumed_commit_seq\" IS NULL \
               AND \"observation_id\" LIKE 'mapping-identity:%' \
               AND json_extract(\"observed\", '$.input.bundle.input.mapping') = ?1"
        );
        for values in self.conn.query(&disputes_sql, params![mapping_id])? {
            if let Some(ds) = crate::engine::SqlRow(values).get::<Option<String>>(0)?
                && !ds.is_empty()
            {
                datasets.insert(ds);
            }
        }
        let pending_sql = format!(
            "SELECT \"observed\" FROM \"{INBOX}\" \
             WHERE \"consumed_commit_seq\" IS NULL \
               AND \"observation_id\" LIKE 'mapping-bundle:%' \
               AND json_extract(\"observed\", '$.input.input.mapping') = ?1"
        );
        let landed_sql = format!(
            "SELECT json_extract(\"observed\", '$.input.raw.source_version'), \
                    json_extract(\"observed\", '$.input.raw.version_kind') \
             FROM \"{INBOX}\" \
             WHERE \"consumed_commit_seq\" IS NOT NULL \
               AND \"observation_id\" LIKE 'mapping-bundle:%' \
               AND json_extract(\"observed\", '$.input.input.mapping') = ?1 \
               AND json_extract(\"observed\", '$.input.input.dataset') = ?2 \
               AND json_extract(\"observed\", '$.input.input.connectorInstance') = ?3 \
               AND json_extract(\"observed\", '$.input.input.partition') = ?4 \
               AND json_extract(\"observed\", '$.input.raw.source') = ?5 \
               AND json_extract(\"observed\", '$.input.raw.source_id') = ?6"
        );
        for values in self.conn.query(&pending_sql, params![mapping_id])? {
            let encoded: String = crate::engine::SqlRow(values).get(0)?;
            let Ok(observed) = serde_json::from_str::<Observed<RuntimeValue>>(&encoded) else {
                continue;
            };
            let Ok(bundle) = norn_ir::runtime::decode::<MappingBundle>(&observed.input) else {
                continue;
            };
            if datasets.contains(&bundle.input.dataset) {
                continue;
            }
            let Some(q_ver) = parse_raw_source_version(&bundle.raw) else {
                datasets.insert(bundle.input.dataset);
                continue;
            };
            let (Some(source), Some(source_id)) = (
                bundle.raw.get("source").and_then(RuntimeValue::as_str),
                bundle.raw.get("source_id").and_then(RuntimeValue::as_str),
            ) else {
                datasets.insert(bundle.input.dataset);
                continue;
            };
            let landed = self.conn.query(
                &landed_sql,
                params![
                    mapping_id,
                    bundle.input.dataset.as_str(),
                    bundle.input.connector_instance.as_str(),
                    bundle.input.partition.as_str(),
                    source,
                    source_id,
                ],
            )?;
            let superseded = landed.into_iter().any(|row_vals| {
                let row = crate::engine::SqlRow(row_vals);
                let Ok(Some(ver)) = row.get::<Option<String>>(0) else {
                    return false;
                };
                let kind_wire = row.get::<Option<String>>(1).ok().flatten();
                parse_version_parts(&ver, kind_wire.as_deref())
                    .is_some_and(|lv| lv.try_cmp(&q_ver) == Ok(std::cmp::Ordering::Greater))
            });
            if !superseded {
                datasets.insert(bundle.input.dataset);
            }
        }
        Ok(datasets)
    }

    /// Whether legacy (unbound) durable cursors or `source.Connection.watermark`
    /// for `source_id` carry direct proof of belonging to `expected_binding`
    /// (`<id>@<version>#<sha256>`).
    ///
    /// # Errors
    /// Store or definition decoding errors.
    pub fn legacy_cursors_match_mapping_binding(
        &self,
        world: &str,
        source_id: &str,
        mapping_id: &str,
        expected_binding: &str,
    ) -> Result<bool, CellError> {
        let cursors = self.source_cursors(source_id)?;
        if !cursors.is_empty() {
            let mut fallback = BTreeMap::new();
            for (dataset, record) in &cursors {
                match legacy_source_replay_allowed(
                    self.conn,
                    &record.records,
                    Some(mapping_id),
                    Some(expected_binding),
                )? {
                    true if !record.records.is_empty() => {}
                    false if !record.records.is_empty() => return Ok(false),
                    _ => {
                        let Some(canonical) =
                            crate::compute::freshness::validate_cursor_json(&record.cursor_json)
                        else {
                            return Ok(false);
                        };
                        fallback.insert(dataset.clone(), canonical);
                    }
                }
            }
            if fallback.is_empty() {
                return Ok(true);
            }
            return self.connection_history_proves_cursors(
                world,
                source_id,
                (mapping_id, expected_binding),
                &fallback,
            );
        }
        if !self.model().has_type("source.Connection") {
            return Ok(false);
        }
        let Some(conn) = self.object(world, "source.Connection", source_id)? else {
            return Ok(false);
        };
        let conn_cursors = crate::compute::freshness::strict_connection_cursors(&conn);
        if conn_cursors.is_empty() {
            return Ok(false);
        }
        self.connection_history_proves_cursors(
            world,
            source_id,
            (mapping_id, expected_binding),
            &conn_cursors,
        )
    }
}

fn parse_version_parts(version: &str, kind_wire: Option<&str>) -> Option<norn_ir::SourceVersion> {
    let kind = match kind_wire {
        Some(wire) => norn_ir::VersionKind::parse_wire(wire)?,
        None => norn_ir::VersionKind::Lexicographical,
    };
    norn_ir::SourceVersion::parse(kind, version).ok()
}

fn parse_raw_source_version(raw: &RuntimeValue) -> Option<norn_ir::SourceVersion> {
    let ver = raw.get("source_version").and_then(RuntimeValue::as_str)?;
    let kind_wire = match raw.get("version_kind").filter(|v| !v.is_null()) {
        Some(v) => Some(v.as_str()?),
        None => None,
    };
    parse_version_parts(ver, kind_wire)
}

#[derive(Serialize)]
struct SourceRowDigest<'a> {
    observation_id: &'a str,
    source: &'a str,
    source_id: &'a str,
    mapping_version: &'a str,
    input_source: Option<&'a RuntimeValue>,
    input_source_id: Option<&'a RuntimeValue>,
    event_id: Option<&'a RuntimeValue>,
    source_version: Option<&'a RuntimeValue>,
    pipeline_version: Option<&'a RuntimeValue>,
    kind: Option<&'a RuntimeValue>,
    payload_digest: Option<&'a RuntimeValue>,
    payload: Option<&'a RuntimeValue>,
}

fn source_row_digests(observations: &[Observed<RuntimeValue>]) -> Vec<SourceRowDigest<'_>> {
    observations
        .iter()
        .map(|raw| SourceRowDigest {
            observation_id: raw.id.0.as_str(),
            source: raw.provenance.source.as_str(),
            source_id: raw.provenance.source_id.as_str(),
            mapping_version: raw.provenance.mapping_version.as_str(),
            input_source: raw.input.get("source"),
            input_source_id: raw.input.get("source_id"),
            event_id: raw.input.get("event_id"),
            source_version: raw.input.get("source_version"),
            pipeline_version: raw.input.get("pipeline_version"),
            kind: raw.input.get("kind"),
            payload_digest: raw.input.get("payload_digest"),
            payload: raw.input.get("payload"),
        })
        .collect()
}

pub(crate) fn source_page_digest_v3(
    advance: &crate::source_cursor::CursorAdvanceSpec,
    mapping: Option<&str>,
    observations: &[Observed<RuntimeValue>],
) -> Result<String, CellError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        connection_id: &'a str,
        dataset: &'a str,
        mapping: Option<&'a str>,
        rows: Vec<SourceRowDigest<'a>>,
        expected_previous_cursor: Option<&'a str>,
        next_cursor: Option<&'a str>,
    }
    let input = DigestInput {
        connection_id: &advance.connection_id,
        dataset: &advance.dataset,
        mapping,
        rows: source_row_digests(observations),
        expected_previous_cursor: advance.expected_previous_cursor.as_deref(),
        next_cursor: advance.next_cursor.as_deref(),
    };
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.source.page.v3\0",
        &input,
    )?)
}

pub(crate) fn source_page_digest_v2(
    advance: &crate::source_cursor::CursorAdvanceSpec,
    observations: &[Observed<RuntimeValue>],
) -> Result<String, CellError> {
    #[derive(Serialize)]
    struct DigestInputV2<'a> {
        connection_id: &'a str,
        dataset: &'a str,
        rows: Vec<SourceRowDigest<'a>>,
        expected_previous_cursor: Option<&'a str>,
        next_cursor: Option<&'a str>,
    }
    let input = DigestInputV2 {
        connection_id: &advance.connection_id,
        dataset: &advance.dataset,
        rows: source_row_digests(observations),
        expected_previous_cursor: advance.expected_previous_cursor.as_deref(),
        next_cursor: advance.next_cursor.as_deref(),
    };
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.source.page.v2\0",
        &input,
    )?)
}

pub(crate) fn legacy_source_replay_allowed<E: SqlEngine>(
    conn: &E,
    records: &[String],
    mapping: Option<&str>,
    active_binding: Option<&str>,
) -> Result<bool, CellError> {
    let mut saw_evidence = false;
    for id in records {
        let encoded: Option<String> = conn.query_optional(
            &format!("SELECT \"observed\" FROM \"{INBOX}\" WHERE \"observation_id\" = ?1"),
            params![id.as_str()],
            |row| row.get(0),
        )?;
        let Some(encoded) = encoded else {
            return Ok(false);
        };
        let Ok(observed) = serde_json::from_str::<Observed<RuntimeValue>>(&encoded) else {
            return Ok(false);
        };
        let bundle = if id.starts_with("mapping-bundle:") {
            let Ok(bundle) = norn_ir::runtime::decode::<MappingBundle>(&observed.input) else {
                return Ok(false);
            };
            bundle
        } else if id.starts_with("mapping-conflict:") {
            let Ok(conflict) = norn_ir::runtime::decode::<MappingConflict>(&observed.input) else {
                return Ok(false);
            };
            conflict.incoming
        } else if id.starts_with("mapping-identity:") {
            let Ok(identity) = norn_ir::runtime::decode::<crate::mapping_identity::IdentityEvidence>(
                &observed.input,
            ) else {
                return Ok(false);
            };
            identity.bundle
        } else {
            return Ok(false);
        };
        saw_evidence = true;
        if mapping.is_some_and(|m| bundle.input.mapping != m) {
            return Ok(false);
        }
        if let Some(pin) = active_binding
            && !bundle
                .artifact_pins
                .iter()
                .any(|(art_id, ver, sha)| format!("{art_id}@{ver}#{sha}") == pin)
        {
            return Ok(false);
        }
    }
    Ok(saw_evidence || (records.is_empty() && mapping.is_none()))
}
