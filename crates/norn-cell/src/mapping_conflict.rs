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
            .chain(
                accepted
                    .deletes
                    .iter()
                    .chain(&incoming.deletes)
                    .map(|d| DataQualitySubject::new(d.object_type.clone(), &d.key)),
            )
            .collect();
        let observation_ids = vec![
            accepted_id.clone(),
            format!("mapping-bundle:{}", incoming.digest),
        ];
        let lineage = accepted
            .records
            .iter()
            .chain(&incoming.records)
            .map(|r| &r.provenance)
            .chain(
                accepted
                    .deletes
                    .iter()
                    .chain(&incoming.deletes)
                    .map(|d| &d.provenance),
            )
            .map(|p| DataQualityLineage::new(p.clone(), observation_ids.clone()))
            .collect();
        let finding = DataQualityFinding::v2(
            DataQualityRuleRef::Builtin {
                id: "core.boundary.import_finding".into(),
                version: "1".into(),
            },
            subjects,
            DataQualitySeverity::Error,
            "conflicting immutable source event content",
            DataQualityOutcome::NotSatisfied,
            lineage,
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

    /// Every unresolved (pending) conflict with its inbox position, in capture order.
    pub(crate) fn verified_source_conflicts(
        &self,
    ) -> Result<Vec<(u64, MappingConflict)>, CellError> {
        self.load_source_conflicts(true)
            .map(|rows| rows.into_iter().map(|(pos, c, _)| (pos, c)).collect())
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
    pub(crate) events: BTreeMap<[String; 5], EventDispute>,
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
            for delete in &bundle.deletes {
                subjects.insert((
                    self.retained_mapping_root(
                        bundle.definitions_version,
                        delete.object_type.as_str(),
                    )?,
                    delete.key.clone(),
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
        self.event_dispute_for_keys(&keys, None)
    }

    /// Datasets of `mapping_id` (optionally scoped to `connection_id`) that
    /// still hold unresolved quarantined inbox rows (`mapping-bundle:` without
    /// a strictly newer landed correction under the declared
    /// [`norn_ir::VersionKind`] ordering, or any unconsumed `mapping-conflict:`
    /// / `mapping-identity:`).
    ///
    /// # Errors
    /// Store query errors.
    pub fn unresolved_quarantined_datasets(
        &self,
        mapping_id: &str,
        connection_id: Option<&str>,
    ) -> Result<BTreeSet<String>, CellError> {
        self.unresolved_quarantined_datasets_with(mapping_id, connection_id, None, None)
    }

    /// Live-question variant of [`Self::unresolved_quarantined_datasets`]:
    /// returns `(live_quarantined, partial_attributable)`. `live_quarantined`
    /// includes every live unconsumed quarantine immediately as a dataset veto
    /// (crediting superseding corrections only once captured before `head`),
    /// while `partial_attributable` evaluates the exact committed frontier
    /// `head` so an uncommitted quarantine on an unrelated dataset can never
    /// authorize a `partial` report in live when pinned `:trace` would reject it.
    pub(crate) fn unresolved_quarantined_datasets_live(
        &self,
        mapping_id: &str,
        connection_id: Option<&str>,
        as_of_ms: u64,
    ) -> Result<(BTreeSet<String>, bool), CellError> {
        let head: i64 = self
            .conn
            .query_optional(
                "SELECT \"commit_seq\" FROM \"_norn_clock\" WHERE \"id\" = 1",
                params![],
                |row| row.get(0),
            )?
            .unwrap_or(0);
        let live =
            self.unresolved_quarantined_datasets_with(mapping_id, connection_id, None, Some(head))?;
        let head_u64 = u64::try_from(head).map_err(|_err| CellError::OutOfRange("frontier"))?;
        let committed = self.unresolved_quarantined_datasets_with(
            mapping_id,
            connection_id,
            Some((head_u64, as_of_ms)),
            Some(head),
        )?;
        Ok((live, !committed.is_empty()))
    }

    /// Like [`Self::unresolved_quarantined_datasets`], evaluated either at live
    /// state (`at == None`) or as of `(frontier, as_of_ms)` for pinned
    /// historical traces, using durable capture-commit attribution in
    /// `_norn_source_evidence` so same-millisecond post-answer captures cannot
    /// invalidate an earlier exact answer.
    ///
    /// # Errors
    /// Store query errors.
    pub(crate) fn unresolved_quarantined_datasets_at(
        &self,
        mapping_id: &str,
        connection_id: Option<&str>,
        at: Option<(u64, u64)>,
    ) -> Result<BTreeSet<String>, CellError> {
        let frontier_i64 = at
            .map(|(f, _)| i64::try_from(f).map_err(|_err| CellError::OutOfRange("frontier")))
            .transpose()?;
        self.unresolved_quarantined_datasets_with(mapping_id, connection_id, at, frontier_i64)
    }

    fn unresolved_quarantined_datasets_with(
        &self,
        mapping_id: &str,
        connection_id: Option<&str>,
        at: Option<(u64, u64)>,
        supersede_frontier: Option<i64>,
    ) -> Result<BTreeSet<String>, CellError> {
        let mut datasets = BTreeSet::new();
        let frontier_i64 = at
            .map(|(f, _)| i64::try_from(f).map_err(|_err| CellError::OutOfRange("frontier")))
            .transpose()?;
        let scope_at = match (frontier_i64, at) {
            (Some(f), Some((_, ms))) => Some((f, ms, max_consumed_inbox_position(self.conn, f)?)),
            _ => None,
        };
        let unconsumed_pred = match frontier_i64 {
            Some(f) => {
                format!("(\"consumed_commit_seq\" IS NULL OR \"consumed_commit_seq\" > {f})")
            }
            None => "\"consumed_commit_seq\" IS NULL".to_owned(),
        };
        let disputes_sql = format!(
            "SELECT \"position\", \"observation_id\", \"observed\" FROM \"{INBOX}\" \
             WHERE {unconsumed_pred} \
               AND (\"observation_id\" LIKE 'mapping-conflict:%' OR \"observation_id\" LIKE 'mapping-identity:%')"
        );
        for values in self.conn.query(&disputes_sql, params![])? {
            let row = crate::engine::SqlRow(values);
            let pos: i64 = row.get(0)?;
            let id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let Some(bundle) = decode_evidence_bundle(&id, &encoded) else {
                continue;
            };
            if bundle.input.mapping != mapping_id {
                continue;
            }
            if evidence_matches_scope(self.conn, &id, pos, &bundle, connection_id, scope_at)? {
                datasets.insert(bundle.input.dataset);
            }
        }
        let pending_sql = format!(
            "SELECT \"position\", \"observation_id\", \"observed\" FROM \"{INBOX}\" \
             WHERE {unconsumed_pred} \
               AND \"observation_id\" LIKE 'mapping-bundle:%' \
               AND json_extract(\"observed\", '$.input.input.mapping') = ?1"
        );
        for values in self.conn.query(&pending_sql, params![mapping_id])? {
            let row = crate::engine::SqlRow(values);
            let pos: i64 = row.get(0)?;
            let id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let Some(bundle) = decode_evidence_bundle(&id, &encoded) else {
                continue;
            };
            if datasets.contains(&bundle.input.dataset)
                || !evidence_matches_scope(self.conn, &id, pos, &bundle, connection_id, scope_at)?
            {
                continue;
            }
            if !bundle_superseded_by_newer(
                self.conn,
                mapping_id,
                connection_id,
                &bundle,
                supersede_frontier,
            )? {
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
                let check = crate::mapping_process::LegacyReplayCheck {
                    mapping: Some(mapping_id),
                    input: None,
                    active_binding: Some(expected_binding),
                    delivered: None,
                };
                match crate::mapping_process::legacy_source_replay_allowed(
                    self.conn,
                    &record.records,
                    check,
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

fn max_consumed_inbox_position<E: SqlEngine>(
    conn: &E,
    frontier: i64,
) -> Result<Option<i64>, CellError> {
    let row: Option<Option<i64>> = conn.query_optional(
        &format!(
            "SELECT MAX(\"position\") FROM \"{INBOX}\" \
             WHERE \"consumed_commit_seq\" IS NOT NULL AND \"consumed_commit_seq\" <= ?1"
        ),
        params![frontier],
        |row| row.get(0),
    )?;
    Ok(row.flatten())
}

fn evidence_matches_scope<E: SqlEngine>(
    conn: &E,
    observation_id: &str,
    position: i64,
    bundle: &MappingBundle,
    connection_id: Option<&str>,
    at: Option<(i64, u64, Option<i64>)>,
) -> Result<bool, CellError> {
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let rows = conn.query(
        &format!(
            "SELECT \"connection_id\", \"captured_commit_seq\" FROM \"{table}\" \
             WHERE \"observation_id\" = ?1"
        ),
        params![observation_id],
    )?;
    if rows.is_empty() {
        return Ok(match at {
            Some((_, _, Some(max_pos))) => position <= max_pos,
            Some((_, as_of_ms, None)) => recorded_at_le(bundle, as_of_ms),
            None => true,
        });
    }
    for values in rows {
        let row = crate::engine::SqlRow(values);
        let cid: String = row.get(0)?;
        let cap_seq: i64 = row.get(1)?;
        let conn_ok = connection_id.is_none_or(|target| cid == target || cid.is_empty());
        let frontier_ok = if cap_seq < 0 {
            match at {
                Some((_, _, Some(max_pos))) => position <= max_pos,
                Some((_, as_of_ms, None)) => recorded_at_le(bundle, as_of_ms),
                None => true,
            }
        } else {
            at.is_none_or(|(frontier, _, _)| cap_seq < frontier)
        };
        if conn_ok && frontier_ok {
            return Ok(true);
        }
    }
    Ok(false)
}

fn landed_matches_connection<E: SqlEngine>(
    conn: &E,
    observation_id: &str,
    connection_id: Option<&str>,
    frontier: Option<i64>,
) -> Result<bool, CellError> {
    let Some(target) = connection_id else {
        return Ok(true);
    };
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let rows = conn.query(
        &format!(
            "SELECT \"connection_id\", \"captured_commit_seq\" FROM \"{table}\" \
             WHERE \"observation_id\" = ?1"
        ),
        params![observation_id],
    )?;
    if rows.is_empty() {
        return Ok(false);
    }
    for values in rows {
        let row = crate::engine::SqlRow(values);
        let cid: String = row.get(0)?;
        let cap_seq: i64 = row.get(1)?;
        if cap_seq >= 0 && (cid == target || cid.is_empty()) && frontier.is_none_or(|f| cap_seq < f)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn recorded_at_le(bundle: &MappingBundle, as_of_ms: u64) -> bool {
    crate::compute::freshness::parse_rfc3339_ms(&bundle.input.recorded_at.0)
        .is_none_or(|ts| ts <= as_of_ms)
}

pub(crate) fn decode_evidence_with_identity(
    id: &str,
    encoded: &str,
) -> Option<(MappingBundle, Option<[Vec<u8>; 5]>)> {
    let observed = serde_json::from_str::<Observed<RuntimeValue>>(encoded).ok()?;
    if id.starts_with("mapping-bundle:") {
        norn_ir::runtime::decode::<MappingBundle>(&observed.input)
            .ok()
            .map(|b| (b, None))
    } else if id.starts_with("mapping-conflict:") {
        norn_ir::runtime::decode::<MappingConflict>(&observed.input)
            .ok()
            .map(|c| (c.incoming, None))
    } else if id.starts_with("mapping-identity:") {
        norn_ir::runtime::decode::<crate::mapping_identity::IdentityEvidence>(&observed.input)
            .ok()
            .map(|i| (i.bundle, Some(i.identity)))
    } else {
        None
    }
}

fn decode_evidence_bundle(id: &str, encoded: &str) -> Option<MappingBundle> {
    decode_evidence_with_identity(id, encoded).map(|(bundle, _)| bundle)
}

fn bundle_superseded_by_newer<E: SqlEngine>(
    conn: &E,
    mapping_id: &str,
    connection_id: Option<&str>,
    bundle: &MappingBundle,
    frontier: Option<i64>,
) -> Result<bool, CellError> {
    let Some(q_ver) = parse_raw_source_version(&bundle.raw) else {
        return Ok(false);
    };
    let (Some(source), Some(source_id)) = (
        bundle.raw.get("source").and_then(RuntimeValue::as_str),
        bundle.raw.get("source_id").and_then(RuntimeValue::as_str),
    ) else {
        return Ok(false);
    };
    let consumed_pred = match frontier {
        Some(f) => {
            format!("(\"consumed_commit_seq\" IS NOT NULL AND \"consumed_commit_seq\" <= {f})")
        }
        None => "\"consumed_commit_seq\" IS NOT NULL".to_owned(),
    };
    let landed_sql = format!(
        "SELECT \"observation_id\", \
                json_extract(\"observed\", '$.input.raw.source_version'), \
                json_extract(\"observed\", '$.input.raw.version_kind') \
         FROM \"{INBOX}\" \
         WHERE {consumed_pred} \
           AND \"observation_id\" LIKE 'mapping-bundle:%' \
           AND json_extract(\"observed\", '$.input.input.mapping') = ?1 \
           AND json_extract(\"observed\", '$.input.input.dataset') = ?2 \
           AND json_extract(\"observed\", '$.input.input.connectorInstance') = ?3 \
           AND json_extract(\"observed\", '$.input.input.partition') = ?4 \
           AND json_extract(\"observed\", '$.input.raw.source') = ?5 \
           AND json_extract(\"observed\", '$.input.raw.source_id') = ?6"
    );
    let landed = conn.query(
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
    for row_vals in landed {
        let row = crate::engine::SqlRow(row_vals);
        let obs_id: String = row.get(0)?;
        let Ok(Some(ver)) = row.get::<Option<String>>(1) else {
            continue;
        };
        let kind_wire = row.get::<Option<String>>(2).ok().flatten();
        if parse_version_parts(&ver, kind_wire.as_deref())
            .is_some_and(|lv| lv.try_cmp(&q_ver) == Ok(std::cmp::Ordering::Greater))
            && landed_matches_connection(conn, &obs_id, connection_id, frontier)?
        {
            return Ok(true);
        }
    }
    Ok(false)
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

pub(crate) fn verify_mapping_binding_in_tx<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    expected_binding: Option<(&str, &str)>,
    lease: Option<(&str, &str, u64)>,
) -> Result<(), CellError> {
    let Some((mapping_id, expected)) = expected_binding else {
        return Ok(());
    };
    let current = reader
        .active_mapping_binding(mapping_id)?
        .unwrap_or_else(|| mapping_id.to_owned());
    if current != expected {
        let (connection_id, lease_id, generation) = lease.map_or_else(
            || (String::new(), String::new(), 0),
            |(c, l, g)| (c.to_owned(), l.to_owned(), g),
        );
        return Err(CellError::SourceLeaseFenced {
            connection_id,
            lease_id,
            generation,
        });
    }
    Ok(())
}

pub(crate) fn verify_source_fence_in_tx<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    fence: crate::source_cursor::SourceReportFence<'_>,
) -> Result<(), CellError> {
    verify_mapping_binding_in_tx(
        reader,
        fence.expected_binding,
        fence.lease.map(|(c, l, g, _)| (c, l, g)),
    )?;
    if let Some((connection_id, lease_id, generation, now_ms)) = fence.lease {
        let active = reader.source_lease(connection_id)?;
        if !active.is_some_and(|current| {
            current.lease_id == lease_id
                && current.generation == generation
                && now_ms < current.expires_at_ms
        }) {
            return Err(CellError::SourceLeaseFenced {
                connection_id: connection_id.to_owned(),
                lease_id: lease_id.to_owned(),
                generation,
            });
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "native"))]
#[allow(
    clippy::expect_used,
    reason = "unit tests for source evidence preservation"
)]
mod tests {
    use super::*;
    use crate::mapping_bundle::MappingInput;
    use norn_checker::mapping::Derivation;
    use norn_ir::provenance::UtcTimestamp;
    use norn_ir::{ArtifactEnvelope, InterfaceDef, ObjectTypeDef, PropertyTypeDef};
    use norn_kernel::Provenance;
    use std::path::PathBuf;

    const MAPPING_ID: &str = "erpnext.three.mapping";
    const DATASET: &str = "Warehouse Lot Balance";

    fn pack_artifacts() -> Vec<ArtifactEnvelope> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut artifacts = Vec::new();
        for dir in ["core", "packs/canon", "packs/erpnext"] {
            for file in std::fs::read_dir(root.join(format!("registry/{dir}/artifacts")))
                .expect("artifacts")
            {
                let artifact: ArtifactEnvelope = serde_json::from_str(
                    &std::fs::read_to_string(file.expect("file").path()).expect("read"),
                )
                .expect("artifact");
                if ![
                    "commerce.SalesOrder.entity",
                    "core.Order.interface",
                    "core.Entitlement.entity",
                    "identity.BusinessUnit.entity",
                    "identity.Directory.interface",
                    "location.Region.entity",
                    "location.Space.entity",
                    "party.Brand.entity",
                    "party.Organization.entity",
                ]
                .contains(&artifact.artifact_id.as_str())
                {
                    artifacts.push(artifact);
                }
            }
        }
        artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        artifacts
    }

    fn open_installed_store(
        artifacts: &[ArtifactEnvelope],
    ) -> CellStore<crate::engine::NativeEngine> {
        let (mut types, mut interfaces, mut properties) = (
            Vec::<ObjectTypeDef>::new(),
            Vec::<InterfaceDef>::new(),
            Vec::<PropertyTypeDef>::new(),
        );
        for artifact in artifacts {
            let encoded = serde_json::to_value(artifact).expect("artifact");
            let payload = encoded.get("payload").expect("payload");
            if let Some(value) = payload.get("objectType") {
                types.push(serde_json::from_value(value.clone()).expect("object"));
            }
            if let Some(value) = payload.get("interface") {
                interfaces.push(serde_json::from_value(value.clone()).expect("interface"));
            }
            if let Some(value) = payload.get("propertyTypes") {
                properties.extend(
                    serde_json::from_value::<Vec<PropertyTypeDef>>(value.clone())
                        .expect("properties"),
                );
            }
        }
        let model =
            crate::Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
                .expect("model")
                .with_governing_laws(artifacts)
                .expect("laws");
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store.bind("scope", "org_demo/dev").expect("scope");
        store
            .install_definitions(
                model,
                crate::InstalledDefinitionInput {
                    canonical_set: String::from_utf8(
                        norn_ir::canonical::canonical_json_of(artifacts).expect("set"),
                    )
                    .expect("utf8"),
                    registry: vec![],
                    receipt_sha256: norn_ir::canonical::domain_hash_hex(
                        b"norn.test.receipt\0",
                        &"source-evidence-replay",
                    )
                    .expect("receipt digest"),
                    admission_receipt: None,
                    readmission: None,
                    diff: serde_json::json!({}),
                    installed_at: 1_793_702_400_000,
                    layout: None,
                },
                None,
                "org_demo/dev",
            )
            .expect("install");
        store
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "json! literals in test fixture helper"
    )]
    fn derive_holding_obs(
        store: &CellStore<crate::engine::NativeEngine>,
        source_version: &str,
        event_id: &str,
        recorded_at: &str,
        quantity_json: serde_json::Value,
        derivation: Derivation,
        reprocesses: Option<String>,
    ) -> Observed<RuntimeValue> {
        let payload = RuntimeValue::from_json(serde_json::json!({
            "name": "SYN-ERPNEXT-holding-north",
            "custom_norn_ref": "demo/holding/owner/north/gold",
            "resource_code": "GOLD",
            "quantity": quantity_json,
            "unit": "g",
            "location": "north",
            "item_ref": "demo/item/gold",
            "owner_ref": "demo/party/owner",
            "custodian_ref": "demo/party/custodian",
            "stock_cost": 9000,
            "cost_currency": "USD",
            "valuation_method": "moving_average",
            "modified": recorded_at,
        }))
        .expect("payload");
        let digest = norn_ir::canonical::HashProfile::Norn1
            .hash_json(serde_json::to_string(&payload).expect("json"))
            .expect("digest");
        let raw = norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => "synthetic-erpnext",
            "source_id" => "SYN-ERPNEXT-holding-north",
            "event_id" => event_id,
            "source_version" => source_version,
            "pipeline_version" => "synthetic/1",
            "kind" => "upsert",
            "extracted_at" => 1_793_702_400_000_u64,
            "payload" => payload,
            "payload_digest" => digest,
        };
        let input = MappingInput {
            mapping: MAPPING_ID.into(),
            dataset: DATASET.into(),
            connector_instance: "demo".into(),
            partition: "snapshot".into(),
            recorded_at: UtcTimestamp(recorded_at.into()),
        };
        let bundle = store
            .read()
            .derived_mapping_bundle(&raw, &input, 1, derivation, reprocesses)
            .expect("derive bundle");
        Observed {
            id: ObservationId(format!("mapping-bundle:{}", bundle.digest)),
            provenance: Provenance {
                source: "synthetic-erpnext".into(),
                source_id: "SYN-ERPNEXT-holding-north".into(),
                observed_at: norn_kernel::LogicalTime::from_millis(100),
                mapping_version: "1.0.0".into(),
            },
            input: norn_ir::runtime::encode(&bundle).expect("encode bundle"),
        }
    }

    fn insert_inbox_row(
        conn: &crate::engine::NativeEngine,
        position: i64,
        obs_id: &str,
        observed_json: &str,
        consumed_seq: Option<i64>,
    ) {
        conn.execute(
            &format!(
                "INSERT INTO \"{INBOX}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") \
                 VALUES (?1, ?2, ?3, ?4)"
            ),
            params![position, obs_id, observed_json, consumed_seq],
        )
        .expect("insert inbox");
    }

    fn insert_evidence_row(
        conn: &crate::engine::NativeEngine,
        obs_id: &str,
        connection_id: &str,
        captured_seq: i64,
    ) {
        conn.execute(
            &format!(
                "INSERT INTO \"{}\" (\"observation_id\", \"connection_id\", \"captured_commit_seq\") \
                 VALUES (?1, ?2, ?3)",
                crate::source_cursor::SOURCE_EVIDENCE
            ),
            params![obs_id, connection_id, captured_seq],
        )
        .expect("insert evidence");
    }

    fn replay_observation_preimage(
        replayed: &CellStore<crate::engine::NativeEngine>,
        commit_seq: u64,
        preimage: &RuntimeValue,
    ) -> (usize, usize) {
        let decoded = crate::export_journal::decode_observation_input(preimage).expect("decode");
        match decoded {
            crate::export_journal::ConsumedInput::Observation {
                position,
                observation_id,
                observed,
                members,
                source_evidence,
            } => {
                assert!(crate::export_journal::members_valid(
                    &replayed.read(),
                    &observation_id,
                    &observed,
                    &members
                ));
                let mut pre_existing = BTreeSet::new();
                let obs_json = crate::codec::canonical_json(&observed).expect("obs json");
                if replayed
                    .read()
                    .inbox_observation_exists(&observation_id)
                    .expect("exists")
                {
                    pre_existing.insert(observation_id.as_str());
                } else {
                    insert_inbox_row(
                        &replayed.conn,
                        i64::try_from(position).expect("pos"),
                        &observation_id,
                        &obs_json,
                        None,
                    );
                }
                for member in &members {
                    if replayed
                        .read()
                        .inbox_observation_exists(&member.observation_id)
                        .expect("m exists")
                    {
                        pre_existing.insert(member.observation_id.as_str());
                    } else {
                        let m_json =
                            crate::codec::canonical_json(&member.observed).expect("m json");
                        insert_inbox_row(
                            &replayed.conn,
                            i64::try_from(member.position).expect("m pos"),
                            &member.observation_id,
                            &m_json,
                            None,
                        );
                    }
                }
                assert!(
                    crate::export_journal::restore_source_evidence(
                        &replayed.conn,
                        commit_seq,
                        &observation_id,
                        &members,
                        &source_evidence,
                        &pre_existing,
                    )
                    .expect("restore evidence")
                );
                (members.len(), source_evidence.len())
            }
            crate::export_journal::ConsumedInput::Result { .. }
            | crate::export_journal::ConsumedInput::Timer { .. } => (usize::MAX, usize::MAX),
        }
    }

    #[test]
    fn unattributed_quarantine_evidence_persists_after_later_reprocess() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        let q_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-q",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::V1,
            None,
        );
        let q_json = serde_json::to_string(&q_obs).expect("q json");
        insert_inbox_row(&store.conn, 1, "other:1", "{}", Some(1));
        insert_inbox_row(&store.conn, 2, &q_obs.id.0, &q_json, Some(3));
        insert_evidence_row(&store.conn, &q_obs.id.0, "", 1);
        let at_frontier_2 = store
            .read()
            .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.a"), Some((2, 100)))
            .expect("quarantined at 2");
        assert!(at_frontier_2.contains(DATASET));
    }

    #[test]
    fn unattributed_correction_resolves_quarantine_and_survives_export_replay() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        let q_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-q2",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::V1,
            None,
        );
        let c_obs = derive_holding_obs(
            &store,
            "2026-01-02",
            "ev-c",
            "2026-01-02T00:00:00Z",
            serde_json::json!(200),
            Derivation::CURRENT,
            None,
        );
        let q_json = serde_json::to_string(&q_obs).expect("q json");
        let c_json = serde_json::to_string(&c_obs).expect("c json");
        insert_inbox_row(&store.conn, 1, &q_obs.id.0, &q_json, None);
        insert_inbox_row(&store.conn, 2, &c_obs.id.0, &c_json, None);
        insert_evidence_row(&store.conn, &q_obs.id.0, "conn.a", 1);
        insert_evidence_row(&store.conn, &c_obs.id.0, "", 1);
        let consumed = crate::Consumption::Observation {
            observation_id: c_obs.id.0.clone(),
        };
        let pre = crate::export_journal::consumed_input(&store.read(), Some(&consumed))
            .expect("consumed_input")
            .expect("preimage");
        store
            .conn
            .execute(
                &format!(
                    "UPDATE \"{INBOX}\" SET \"consumed_commit_seq\" = 2 WHERE \"observation_id\" = ?1"
                ),
                params![c_obs.id.0.as_str()],
            )
            .expect("consume c");
        let (p2, ev2) =
            crate::export_journal::read_late_source_evidence(&store.conn, 2).expect("late c2");
        assert_eq!(p2.len(), 1);
        let replayed = open_installed_store(&artifacts);
        assert_eq!(replay_observation_preimage(&replayed, 2, &pre), (0, 1));
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 2, &p2, &ev2)
            .expect("restore p2");
        replayed
            .conn
            .execute(
                &format!(
                    "UPDATE \"{INBOX}\" SET \"consumed_commit_seq\" = 2 WHERE \"observation_id\" = ?1"
                ),
                params![c_obs.id.0.as_str()],
            )
            .expect("consume c on replayed");
        let live = store
            .read()
            .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.a"), None)
            .expect("live quarantined");
        let replayed_live = replayed
            .read()
            .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.a"), None)
            .expect("replayed quarantined");
        assert!(live.is_empty() && replayed_live.is_empty());
    }

    #[test]
    fn reprocess_export_replay_preserves_earlier_frontier_quarantine() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        let q_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-rq",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::V1,
            None,
        );
        let r_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-rq",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::CURRENT,
            Some(q_obs.id.0.clone()),
        );
        let q_json = serde_json::to_string(&q_obs).expect("q json");
        let r_json = serde_json::to_string(&r_obs).expect("r json");
        insert_inbox_row(&store.conn, 1, &q_obs.id.0, &q_json, None);
        insert_evidence_row(&store.conn, &q_obs.id.0, "conn.a", 1);
        let (p2, ev2) =
            crate::export_journal::read_late_source_evidence(&store.conn, 2).expect("late c2");
        insert_inbox_row(&store.conn, 2, &r_obs.id.0, &r_json, None);
        insert_evidence_row(&store.conn, &r_obs.id.0, "", 3);
        let consumed = crate::Consumption::Observation {
            observation_id: r_obs.id.0.clone(),
        };
        let pre = crate::export_journal::consumed_input(&store.read(), Some(&consumed))
            .expect("consumed_input")
            .expect("preimage");
        let replayed = open_installed_store(&artifacts);
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 2, &p2, &ev2)
            .expect("restore c2 quarantine");
        assert_eq!(replay_observation_preimage(&replayed, 4, &pre), (1, 2));
        let round_trip = crate::export_journal::consumed_input(&replayed.read(), Some(&consumed))
            .expect("replayed consumed_input")
            .expect("replayed preimage");
        assert_eq!(round_trip, pre);
        for conn in [&store.conn, &replayed.conn] {
            conn.execute(
                &format!(
                    "UPDATE \"{INBOX}\" SET \"consumed_commit_seq\" = 4 \
                     WHERE \"observation_id\" = ?1 OR \"observation_id\" = ?2"
                ),
                params![r_obs.id.0.as_str(), q_obs.id.0.as_str()],
            )
            .expect("consume r + q");
        }
        let live_f2 = store
            .read()
            .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.a"), Some((2, 100)))
            .expect("live f2");
        let replayed_f2 = replayed
            .read()
            .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.a"), Some((2, 100)))
            .expect("replayed f2");
        assert!(live_f2.contains(DATASET) && replayed_f2 == live_f2);
        assert!(
            replayed
                .read()
                .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.a"), Some((4, 100)))
                .expect("replayed f4")
                .is_empty()
        );
    }

    #[test]
    fn late_sibling_attribution_and_commit_refused_pending_survive_empty_replay() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        // Commit-refused bundle has empty diagnostics yet remains pending as a freshness veto.
        let q_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-lq",
            "2026-01-01T00:00:00Z",
            serde_json::json!(200),
            Derivation::CURRENT,
            None,
        );
        let c_obs = derive_holding_obs(
            &store,
            "2026-01-02",
            "ev-lc",
            "2026-01-02T00:00:00Z",
            serde_json::json!(200),
            Derivation::CURRENT,
            None,
        );
        let q_json = serde_json::to_string(&q_obs).expect("q json");
        let c_json = serde_json::to_string(&c_obs).expect("c json");
        insert_inbox_row(&store.conn, 1, &q_obs.id.0, &q_json, None);
        insert_inbox_row(&store.conn, 2, &c_obs.id.0, &c_json, None);
        insert_evidence_row(&store.conn, &q_obs.id.0, "conn.b", 1);
        insert_evidence_row(&store.conn, &c_obs.id.0, "conn.a", 1);
        let c_consumed = crate::Consumption::Observation {
            observation_id: c_obs.id.0.clone(),
        };
        let c_pre = crate::export_journal::consumed_input(&store.read(), Some(&c_consumed))
            .expect("c pre")
            .expect("pre");
        store
            .conn
            .execute(
                &format!(
                    "UPDATE \"{INBOX}\" SET \"consumed_commit_seq\" = 2 WHERE \"observation_id\" = ?1"
                ),
                params![c_obs.id.0.as_str()],
            )
            .expect("consume c");
        let (p2, ev2) =
            crate::export_journal::read_late_source_evidence(&store.conn, 2).expect("late c2");
        assert_eq!(p2.len(), 1);
        let replayed = open_installed_store(&artifacts);
        assert_eq!(replay_observation_preimage(&replayed, 2, &c_pre), (0, 1));
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 2, &p2, &ev2)
            .expect("restore c2 pending veto");
        replayed
            .conn
            .execute(
                &format!(
                    "UPDATE \"{INBOX}\" SET \"consumed_commit_seq\" = 2 WHERE \"observation_id\" = ?1"
                ),
                params![c_obs.id.0.as_str()],
            )
            .expect("consume c replayed");
        assert!(
            replayed
                .read()
                .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.b"), Some((2, 100)))
                .expect("replayed f2 before c3")
                .contains(DATASET)
        );
        insert_evidence_row(&store.conn, &c_obs.id.0, "conn.b", 2);
        let (p3, ev3) =
            crate::export_journal::read_late_source_evidence(&store.conn, 3).expect("late c3");
        assert!(p3.is_empty() && ev3.len() == 1);
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 3, &p3, &ev3)
            .expect("restore c3 late evidence");
        assert!(
            replayed
                .read()
                .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.b"), Some((2, 100)))
                .expect("pinned f2")
                .contains(DATASET)
        );
        assert!(
            replayed
                .read()
                .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.b"), Some((3, 100)))
                .expect("pinned f3")
                .is_empty()
        );
    }

    #[test]
    fn legacy_pending_quarantine_promotion_survives_empty_replay() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        let q_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-leg",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::V1,
            None,
        );
        let q_json = serde_json::to_string(&q_obs).expect("q json");
        insert_inbox_row(&store.conn, 1, &q_obs.id.0, &q_json, None);
        crate::source_cursor::record_source_evidence_in_tx(
            &store.conn,
            &q_obs.id.0,
            "conn.b",
            false,
        )
        .expect("promote legacy");
        let (p2, ev2) =
            crate::export_journal::read_late_source_evidence(&store.conn, 2).expect("late c2");
        assert_eq!(p2.len(), 1);
        assert_eq!(ev2.len(), 2);
        let replayed = open_installed_store(&artifacts);
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 2, &p2, &ev2)
            .expect("empty replay accepts promoted legacy quarantine");
        assert_eq!(
            crate::export_journal::read_late_source_evidence(&replayed.conn, 2).expect("rt c2"),
            (p2, ev2)
        );
        assert!(
            replayed
                .read()
                .unresolved_quarantined_datasets_at(MAPPING_ID, Some("conn.b"), Some((2, 100)))
                .expect("replayed f2")
                .contains(DATASET)
        );
    }

    #[test]
    fn replay_rejects_tampered_members_and_retroactive_attribution() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        let q_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-badq",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::V1,
            None,
        );
        let r_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-badq",
            "2026-01-01T00:00:00Z",
            serde_json::json!("200.50"),
            Derivation::CURRENT,
            Some(q_obs.id.0.clone()),
        );
        let mut tampered_q = q_obs.clone();
        if let RuntimeValue::Object(ref mut input_obj) = tampered_q.input
            && let Some(RuntimeValue::Object(raw_obj)) = input_obj.get_mut("raw")
        {
            raw_obj.insert("source_version".into(), RuntimeValue::from("tampered"));
        }
        let r_val = norn_ir::runtime::encode(&r_obs).expect("r val");
        let forged_member = crate::PendingObservation {
            position: 1,
            observation_id: q_obs.id.0.clone(),
            observed: norn_ir::runtime::encode(&tampered_q).expect("tampered val"),
        };
        assert!(!crate::export_journal::members_valid(
            &store.read(),
            &r_obs.id.0,
            &r_val,
            &[forged_member]
        ));
        let q_json = serde_json::to_string(&q_obs).expect("q json");
        insert_inbox_row(&store.conn, 1, &q_obs.id.0, &q_json, None);
        insert_evidence_row(&store.conn, &q_obs.id.0, "conn.a", 1);
        let pre_existing = BTreeSet::from([q_obs.id.0.as_str()]);
        let q_member = crate::PendingObservation {
            position: 1,
            observation_id: q_obs.id.0.clone(),
            observed: norn_ir::runtime::encode(&q_obs).expect("q val"),
        };
        let retroactive = [
            crate::export::SourceEvidenceEntry {
                observation_id: q_obs.id.0.clone(),
                connection_id: "conn.a".into(),
                captured_commit_seq: 1,
            },
            crate::export::SourceEvidenceEntry {
                observation_id: q_obs.id.0.clone(),
                connection_id: "conn.x".into(),
                captured_commit_seq: 2,
            },
        ];
        assert!(
            !crate::export_journal::restore_source_evidence(
                &store.conn,
                5,
                &r_obs.id.0,
                &[q_member],
                &retroactive,
                &pre_existing,
            )
            .expect("retroactive check")
        );
        let forged_legacy = [
            crate::export::SourceEvidenceEntry {
                observation_id: q_obs.id.0.clone(),
                connection_id: String::new(),
                captured_commit_seq: -1,
            },
            crate::export::SourceEvidenceEntry {
                observation_id: q_obs.id.0,
                connection_id: "conn.b".into(),
                captured_commit_seq: 2,
            },
        ];
        assert!(
            crate::export_journal::restore_late_source_evidence(
                &store.read(),
                3,
                &[],
                &forged_legacy
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_pending_bundle_conflict_dependency_survives_empty_replay() {
        let artifacts = pack_artifacts();
        let store = open_installed_store(&artifacts);
        // Legacy pending bundle has no _norn_source_evidence row at all.
        let legacy_obs = derive_holding_obs(
            &store,
            "2026-01-01",
            "ev-dep",
            "2026-01-01T00:00:00Z",
            serde_json::json!(200),
            Derivation::CURRENT,
            None,
        );
        let legacy_json = serde_json::to_string(&legacy_obs).expect("legacy json");
        insert_inbox_row(&store.conn, 1, &legacy_obs.id.0, &legacy_json, None);
        // Conflicting event with the same event_id but changed source_version creates mapping-conflict:.
        let incoming_obs = derive_holding_obs(
            &store,
            "2026-01-02",
            "ev-dep",
            "2026-01-01T00:00:00Z",
            serde_json::json!(250),
            Derivation::CURRENT,
            None,
        );
        let incoming_bundle: MappingBundle =
            norn_ir::runtime::decode(&incoming_obs.input).expect("decode incoming");
        let (conflict_id, inserted) =
            capture(&store, &incoming_obs, &incoming_bundle).expect("capture conflict");
        assert!(inserted && conflict_id.starts_with("mapping-conflict:"));
        crate::source_cursor::record_source_evidence_in_tx(
            &store.conn,
            &conflict_id,
            "conn.b",
            true,
        )
        .expect("record conflict evidence");
        let (p2, ev2) =
            crate::export_journal::read_late_source_evidence(&store.conn, 2).expect("late c2");
        assert_eq!(p2.len(), 2);
        assert_eq!(ev2.len(), 1);
        let replayed = open_installed_store(&artifacts);
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 2, &p2, &ev2)
            .expect("empty replay restores legacy dependency before conflict recheck");
        assert_eq!(
            crate::export_journal::read_late_source_evidence(&replayed.conn, 2).expect("rt c2"),
            (p2, ev2)
        );
        // Later named promotion of legacy_obs at clock=2 does not duplicate legacy_obs in pending_observations.
        insert_evidence_row(&store.conn, &legacy_obs.id.0, "", -1);
        insert_evidence_row(&store.conn, &legacy_obs.id.0, "conn.b", 2);
        let (p3, ev3) =
            crate::export_journal::read_late_source_evidence(&store.conn, 3).expect("late c3");
        assert!(p3.is_empty() && ev3.len() == 2);
        crate::export_journal::restore_late_source_evidence(&replayed.read(), 3, &p3, &ev3)
            .expect("c3 promotes already-restored legacy dependency");
        assert_eq!(
            crate::export_journal::read_late_source_evidence(&replayed.conn, 3).expect("rt c3"),
            (p3, ev3)
        );
        // An unattributed pending bundle with no referencing conflict is still rejected.
        let unreferenced = open_installed_store(&artifacts);
        let orphan_observed = norn_ir::runtime::encode(&legacy_obs).expect("legacy val");
        let orphan = crate::PendingObservation {
            position: 1,
            observation_id: legacy_obs.id.0,
            observed: orphan_observed,
        };
        assert!(
            crate::export_journal::restore_late_source_evidence(
                &unreferenced.read(),
                2,
                &[orphan],
                &[]
            )
            .is_err()
        );
    }
}
