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

    pub(crate) fn has_subject(&self, roots: Option<&[String]>, key: &str) -> bool {
        match roots {
            Some(roots) => roots
                .iter()
                .any(|root| self.subjects.contains_key(&(root.clone(), key.to_owned()))),
            None => self.roots_by_key.contains_key(key),
        }
    }

    pub(crate) fn event_dispute(
        &self,
        bundle: &MappingBundle,
    ) -> Result<Option<EventDispute>, CellError> {
        let keys = crate::mapping_identity::keys(&bundle.raw, &bundle.input)?
            .map(|key| norn_ir::canonical::nfc(&key));
        Ok(self.events.get(&keys).cloned())
    }

    fn open_findings_for(&self, roots: Option<&[String]>, key: &str, identity: bool) -> usize {
        let mut seen = BTreeSet::new();
        let mut count = 0_usize;
        for root in roots.into_iter().flatten().chain(
            self.roots_by_key
                .get(key)
                .into_iter()
                .flatten()
                .filter(|_root| roots.is_none()),
        ) {
            for (kind, finding) in self
                .subjects
                .get(&(root.clone(), key.to_owned()))
                .into_iter()
                .flatten()
            {
                if *kind == identity
                    && seen.insert(finding.id.clone())
                    && matches!(finding.status, DataQualityFindingStatus::Open)
                {
                    count = count.saturating_add(1);
                }
            }
        }
        count
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

    fn subject_matches_readable(
        &self,
        version: u64,
        object_type: &str,
        key: &str,
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
    ) -> bool {
        let Some(allowed_roots) = readable_map.get(key) else {
            return false;
        };
        let Some(roots) = allowed_roots else {
            return true;
        };
        if let Ok(root) = self.retained_mapping_root(version, object_type) {
            return roots.contains(&root);
        }
        if let Ok(root) = self.root_type(object_type) {
            return roots.contains(root);
        }
        true
    }

    fn dispute_touches_readable(
        &self,
        version: u64,
        finding: &DataQualityFinding,
        records: &[norn_ir::mapping::MappedRecord],
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
    ) -> bool {
        finding.subjects.iter().any(|s| {
            self.subject_matches_readable(version, s.object_type.as_str(), &s.key, readable_map)
        }) || records.iter().any(|r| {
            self.subject_matches_readable(version, r.object_type.as_str(), &r.key, readable_map)
        })
    }

    fn verify_committed_bundle_event(&self, observation_id: &str) -> Result<(), CellError> {
        let encoded: String = self
            .conn
            .query_optional(
                &format!("SELECT observed FROM {INBOX} WHERE observation_id = ?1"),
                params![observation_id],
                |row| row.get(0),
            )?
            .ok_or(CellError::MappingBundleMismatch)?;
        let observed: Observed<RuntimeValue> =
            serde_json::from_str(&encoded).map_err(|_| CellError::MappingBundleMismatch)?;
        let bundle: MappingBundle = norn_ir::runtime::decode(&observed.input)
            .map_err(|_| CellError::MappingBundleMismatch)?;
        if observed.id.0 != observation_id
            || observation_id != format!("mapping-bundle:{}", bundle.digest)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        self.recheck_mapping_bundle(&bundle)
    }

    fn subject_matches_readable_active(
        &self,
        object_type: &str,
        subject_key: &str,
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
    ) -> bool {
        let Some(allowed_roots) = readable_map.get(subject_key) else {
            return false;
        };
        let Some(roots) = allowed_roots else {
            return true;
        };
        match self.root_type(object_type) {
            Ok(root) => roots.contains(root),
            Err(_) => true,
        }
    }

    fn event_has_committed_readable_bundle(
        &self,
        event_keys: &[String; 5],
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
    ) -> Result<bool, CellError> {
        let [c, p, s, sid, eid] = event_keys;
        let [connector, partition, source, source_id, event_id]: [String; 5] =
            serde_json::from_slice(&norn_ir::canonical::canonical_json_of(&(
                c, p, s, sid, eid,
            ))?)?;
        let table = crate::system::CLAIM_ENDPOINT;
        let rows = self.conn.query(
            &format!(
                "SELECT \"key\", \"observation_id\" FROM \"{table}\" \
                 WHERE \"side\" = 'event:' || json_array(?1, ?2, ?3, ?4, ?5)"
            ),
            params![&connector, &partition, &source, &source_id, &event_id],
        )?;
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let subject_json: String = row.get(0)?;
            let observation_id: String = row.get(1)?;
            let [object_type, subject_key]: [String; 2] = serde_json::from_str(&subject_json)
                .map_err(|_| CellError::MappingBundleMismatch)?;
            if !self.subject_matches_readable_active(&object_type, &subject_key, readable_map) {
                continue;
            }
            self.verify_committed_bundle_event(&observation_id)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn event_has_unconsumed_readable_bundle(
        &self,
        event_keys: &[String; 5],
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
    ) -> Result<bool, CellError> {
        let [c, p, s, sid, eid] = event_keys;
        let [connector, partition, source, source_id, event_id]: [String; 5] =
            serde_json::from_slice(&norn_ir::canonical::canonical_json_of(&(
                c, p, s, sid, eid,
            ))?)?;
        let rows = self.conn.query(
            &format!(
                "SELECT observation_id, observed FROM {INBOX} \
                 WHERE observation_id LIKE 'mapping-bundle:%' \
                 AND consumed_commit_seq IS NULL \
                 AND json_extract(observed, '$.input.input.connectorInstance') = ?1 \
                 AND json_extract(observed, '$.input.input.partition') = ?2 \
                 AND json_extract(observed, '$.input.raw.source') = ?3 \
                 AND json_extract(observed, '$.input.raw.source_id') = ?4 \
                 AND json_extract(observed, '$.input.raw.event_id') = ?5 \
                 ORDER BY position"
            ),
            params![&connector, &partition, &source, &source_id, &event_id],
        )?;
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let stored_id: String = row.get(0)?;
            let encoded: String = row.get(1)?;
            let Ok(observed) = serde_json::from_str::<Observed<RuntimeValue>>(&encoded) else {
                continue;
            };
            let Ok(bundle) = norn_ir::runtime::decode::<MappingBundle>(&observed.input) else {
                continue;
            };
            let touches = bundle.records.iter().any(|r| {
                self.subject_matches_readable(
                    bundle.definitions_version,
                    r.object_type.as_str(),
                    &r.key,
                    readable_map,
                )
            });
            if !touches
                || observed.id.0 != stored_id
                || stored_id != format!("mapping-bundle:{}", bundle.digest)
            {
                continue;
            }
            if let Err(err) = self.recheck_mapping_bundle(&bundle) {
                if crate::mapping_page::is_store_failure(&err) {
                    return Err(err);
                }
                continue;
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn collect_scoped_source_conflicts(
        &self,
        rows: Vec<Vec<crate::engine::SqlValue>>,
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
        index: &mut SourceDisputeIndex,
    ) -> Result<(), CellError> {
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let position = crate::read::to_u64(row.get(0)?, "inbox position")?;
            let stored_id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let Ok(observed) = serde_json::from_str::<Observed<RuntimeValue>>(&encoded) else {
                continue;
            };
            let Ok(conflict) = norn_ir::runtime::decode::<MappingConflict>(&observed.input) else {
                continue;
            };
            if observed.id.0 != stored_id
                || stored_id != format!("mapping-conflict:{}", conflict.finding.id)
            {
                continue;
            }
            let direct = self.dispute_touches_readable(
                conflict.incoming.definitions_version,
                &conflict.finding,
                &conflict.incoming.records,
                readable_map,
            );
            let event_keys =
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)
                    .ok()
                    .map(|keys| keys.map(|k| norn_ir::canonical::nfc(&k)));
            let relevant = direct
                || match event_keys.as_ref() {
                    Some(keys) => {
                        self.event_has_committed_readable_bundle(keys, readable_map)?
                            || self.event_has_unconsumed_readable_bundle(keys, readable_map)?
                    }
                    None => false,
                };
            if !relevant {
                continue;
            }
            if let Err(err) = self.recheck_mapping_bundle(&conflict.incoming) {
                if crate::mapping_page::is_store_failure(&err) {
                    return Err(err);
                }
                continue;
            }
            let expected = self.mapping_conflict(&conflict.incoming)?;
            if expected.as_ref() != Some(&conflict) {
                continue;
            }
            index.dispute(
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)?
                    .map(|key| norn_ir::canonical::nfc(&key)),
                position,
                stored_id,
                &conflict.finding,
            );
            self.index_source_finding(index, &conflict.incoming, false, &conflict.finding)?;
        }
        Ok(())
    }

    fn collect_scoped_identity_evidence(
        &self,
        rows: Vec<Vec<crate::engine::SqlValue>>,
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
        index: &mut SourceDisputeIndex,
    ) -> Result<(), CellError> {
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let position = crate::read::to_u64(row.get(0)?, "inbox position")?;
            let stored_id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let Ok(observed) = serde_json::from_str::<Observed<RuntimeValue>>(&encoded) else {
                continue;
            };
            let Ok(guard) = norn_ir::runtime::decode::<crate::mapping_identity::IdentityEvidence>(
                &observed.input,
            ) else {
                continue;
            };
            if observed.id.0 != stored_id
                || stored_id != format!("mapping-identity:{}", guard.finding.id)
            {
                continue;
            }
            let direct = self.dispute_touches_readable(
                guard.bundle.definitions_version,
                &guard.finding,
                &guard.bundle.records,
                readable_map,
            );
            let event_keys = crate::mapping_identity::keys(&guard.bundle.raw, &guard.bundle.input)
                .ok()
                .map(|keys| keys.map(|k| norn_ir::canonical::nfc(&k)));
            let relevant = direct
                || match event_keys.as_ref() {
                    Some(keys) => {
                        self.event_has_committed_readable_bundle(keys, readable_map)?
                            || self.event_has_unconsumed_readable_bundle(keys, readable_map)?
                    }
                    None => false,
                };
            if !relevant {
                continue;
            }
            if let Err(err) = self.recheck_identity_evidence(&guard) {
                if crate::mapping_page::is_store_failure(&err) {
                    return Err(err);
                }
                continue;
            }
            index.dispute(
                crate::mapping_identity::keys(&guard.bundle.raw, &guard.bundle.input)?,
                position,
                stored_id,
                &guard.finding,
            );
            self.index_source_finding(index, &guard.bundle, true, &guard.finding)?;
        }
        Ok(())
    }

    fn held_link_endpoints_readable(
        &self,
        scope: &str,
        finding: &DataQualityFinding,
        evidence_access: crate::combined_view::RowAccessDecider<'_>,
    ) -> Result<bool, CellError> {
        if finding.subjects.len() < 2 {
            return Ok(false);
        }
        for subject in &finding.subjects {
            let Some(record) = self.object(
                crate::MAIN_WORLD,
                subject.object_type.as_str(),
                &subject.key,
            )?
            else {
                return Ok(false);
            };
            if record.scope != scope {
                return Err(CellError::ComputeRefused("core.compute.stale_input"));
            }
            if !matches!(
                evidence_access(&record)?,
                crate::compute::transform::RowAccess::Read
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Count open source-conflict, identity-dispute, and held-link findings strictly
    /// for `readable` subjects (`HOST-READ-2` / `OBS-LINK-DISPUTE-1`), returning
    /// immediately via indexed prefix ranges on clean cells, expanding strictly per
    /// candidate dispute event via durable subject-to-bundle bindings (covering no-op
    /// consumed bundles and failing closed on corrupted committed bundles without
    /// touching unreadable page siblings or full history), and requiring both endpoints
    /// to be readable before counting any held-link finding.
    pub(crate) fn scoped_open_source_dispute_findings(
        &self,
        scope: &str,
        readable: &[(String, String)],
        evidence_access: crate::combined_view::RowAccessDecider<'_>,
    ) -> Result<usize, CellError> {
        // Range bounds (`:` = 0x3A, `;` = 0x3B) use the `UNIQUE(observation_id)` index.
        let conflict_rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM {INBOX} \
                 WHERE observation_id >= 'mapping-conflict:' AND observation_id < 'mapping-conflict;' \
                 ORDER BY position"
            ),
            params![],
        )?;
        let identity_rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM {INBOX} \
                 WHERE observation_id >= 'mapping-identity:' AND observation_id < 'mapping-identity;' \
                 ORDER BY position"
            ),
            params![],
        )?;
        if conflict_rows.is_empty() && identity_rows.is_empty() {
            return Ok(0);
        }
        let mut readable_roots = Vec::with_capacity(readable.len());
        let mut readable_map: BTreeMap<&str, Option<BTreeSet<String>>> = BTreeMap::new();
        for (ty, key) in readable {
            let roots = self.source_dispute_roots(crate::MAIN_WORLD, ty, key)?;
            match (&roots, readable_map.entry(key.as_str())) {
                (Some(rs), std::collections::btree_map::Entry::Vacant(slot)) => {
                    slot.insert(Some(rs.iter().cloned().collect()));
                }
                (Some(rs), std::collections::btree_map::Entry::Occupied(mut slot)) => {
                    if let Some(existing) = slot.get_mut() {
                        existing.extend(rs.iter().cloned());
                    }
                }
                (None, std::collections::btree_map::Entry::Vacant(slot)) => {
                    slot.insert(None);
                }
                (None, std::collections::btree_map::Entry::Occupied(mut slot)) => {
                    slot.insert(None);
                }
            }
            readable_roots.push((ty.as_str(), key.as_str(), roots));
        }
        let mut scoped_index = SourceDisputeIndex::default();
        self.collect_scoped_source_conflicts(conflict_rows, &readable_map, &mut scoped_index)?;
        self.collect_scoped_identity_evidence(identity_rows, &readable_map, &mut scoped_index)?;
        if scoped_index.subjects.is_empty() {
            return Ok(0);
        }
        let mut open = 0_usize;
        for (ty, key, roots) in &readable_roots {
            open =
                open.saturating_add(scoped_index.open_findings_for(roots.as_deref(), key, false));
            open = open.saturating_add(scoped_index.open_findings_for(roots.as_deref(), key, true));
            for finding in self.held_link_findings_in_index(ty, key, &scoped_index)? {
                if matches!(finding.status, DataQualityFindingStatus::Open)
                    && self.held_link_endpoints_readable(scope, &finding, evidence_access)?
                {
                    open = open.saturating_add(1);
                }
            }
        }
        Ok(open)
    }
}
