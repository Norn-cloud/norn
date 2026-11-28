//! Scoped source-dispute counting for `HOST-READ-2` / `OBS-LINK-DISPUTE-1`
//! combined-view cuts, kept separate from [`crate::mapping_conflict`] so
//! each module stays within the repository non-test LOC cap.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{DataQualityFinding, DataQualityFindingStatus, RuntimeValue};
use norn_kernel::Observed;

use crate::engine::{SqlEngine, params};
use crate::mapping_bundle::MappingBundle;
use crate::mapping_conflict::{
    DisputeVerifyCache, EventDispute, MappingConflict, SourceDisputeIndex, SourceEventCapture,
    classify_source_event_with,
};
use crate::system::INBOX;
use crate::{CellError, StoreReader};

impl SourceDisputeIndex {
    pub(crate) fn has_subject(&self, roots: Option<&[String]>, key: &str) -> bool {
        match roots {
            Some(roots) => roots.iter().any(|root| {
                self.subjects
                    .get(root.as_str())
                    .is_some_and(|by_key| by_key.contains_key(key))
            }),
            None => self.roots_by_key.contains_key(key),
        }
    }

    fn subject_findings(&self, root: &str, key: &str) -> Option<&[(bool, DataQualityFinding)]> {
        self.subjects
            .get(root)
            .and_then(|by_key| by_key.get(key))
            .map(Vec::as_slice)
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
            for (kind, finding) in self.subject_findings(root, key).into_iter().flatten() {
                if *kind == identity
                    && seen.insert(finding.id.as_str())
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
    pub(crate) fn index_source_finding_with(
        &self,
        index: &mut SourceDisputeIndex,
        related: &[MappingBundle],
        incoming: &MappingBundle,
        identity: bool,
        finding: &DataQualityFinding,
        retained_roots: &mut BTreeMap<(u64, String), String>,
    ) -> Result<(), CellError> {
        let mut subjects = BTreeSet::new();
        for bundle in related.iter().chain(std::iter::once(incoming)) {
            for record in &bundle.records {
                let root = self
                    .cached_retained_root(
                        bundle.definitions_version,
                        record.object_type.as_str(),
                        retained_roots,
                    )?
                    .to_owned();
                subjects.insert((root, record.key.clone()));
            }
        }
        for (root, key) in subjects {
            index
                .roots_by_key
                .entry(key.clone())
                .or_default()
                .insert(root.clone());
            index
                .subjects
                .entry(root)
                .or_default()
                .entry(key)
                .or_default()
                .push((identity, finding.clone()));
        }
        Ok(())
    }

    /// D102: identities of `root` with any source or identity finding, the
    /// candidates an analytical answer checks for exclusion.
    pub(crate) fn source_finding_keys(&self, root: &str) -> Result<BTreeSet<String>, CellError> {
        self.with_source_dispute_index(|index| {
            index
                .subjects
                .get(root)
                .map_or_default(|by_key| by_key.keys().cloned().collect())
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
                for (kind, finding) in index.subject_findings(root, key).into_iter().flatten() {
                    if *kind == identity && seen.insert(finding.id.as_str()) {
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
            if index.roots_by_key.is_empty() {
                return Ok(BTreeMap::new());
            }
            let mut pins = BTreeMap::new();
            let mut hash_for = |key: &str, roots: &BTreeSet<String>| -> Result<(), CellError> {
                let mut findings = Vec::with_capacity(roots.len());
                for root in roots {
                    if let Some(entries) = index
                        .subjects
                        .get(root.as_str())
                        .and_then(|by_key| by_key.get(key))
                    {
                        findings.push((root, entries));
                    }
                }
                if !findings.is_empty() {
                    pins.insert(key.to_owned(), crate::codec::hash_json(&findings)?);
                }
                Ok(())
            };
            if members.len() <= index.roots_by_key.len() {
                for key in members {
                    if let Some(roots) = index.roots_by_key.get(key.as_str()) {
                        hash_for(key, roots)?;
                    }
                }
            } else {
                for (key, roots) in &index.roots_by_key {
                    if members.contains(key.as_str()) {
                        hash_for(key, roots)?;
                    }
                }
            }
            Ok(pins)
        })?
    }

    pub(crate) fn indexed_source_dispute(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<bool, CellError> {
        let roots = self.source_dispute_roots(world, object_type, key)?;
        self.with_source_dispute_index(|index| index.has_subject(roots.as_deref(), key))
    }

    fn subject_matches_readable(
        &self,
        version: u64,
        object_type: &str,
        key: &str,
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
        retained_roots: &mut BTreeMap<(u64, String), String>,
    ) -> bool {
        let Some(allowed_roots) = readable_map.get(key) else {
            return false;
        };
        let Some(roots) = allowed_roots else {
            return true;
        };
        if let Ok(root) = self.cached_retained_root(version, object_type, retained_roots) {
            return roots.contains(root);
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
        retained_roots: &mut BTreeMap<(u64, String), String>,
    ) -> bool {
        finding.subjects.iter().any(|s| {
            self.subject_matches_readable(
                version,
                s.object_type.as_str(),
                &s.key,
                readable_map,
                retained_roots,
            )
        }) || records.iter().any(|r| {
            self.subject_matches_readable(
                version,
                r.object_type.as_str(),
                &r.key,
                readable_map,
                retained_roots,
            )
        })
    }

    fn verify_committed_bundle_event_cached(
        &self,
        observation_id: &str,
        verified_bundles: &mut BTreeSet<String>,
    ) -> Result<(), CellError> {
        if verified_bundles.contains(observation_id) {
            return Ok(());
        }
        let encoded: String = self
            .conn
            .query_optional(
                &format!("SELECT observed FROM {INBOX} WHERE observation_id = ?1"),
                params![observation_id],
                |row| row.get(0),
            )?
            .ok_or(CellError::MappingBundleMismatch)?;
        let observed: Observed<RuntimeValue> =
            serde_json::from_str(&encoded).map_err(|_err| CellError::MappingBundleMismatch)?;
        let bundle: MappingBundle = norn_ir::runtime::decode(&observed.input)
            .map_err(|_err| CellError::MappingBundleMismatch)?;
        if observed.id.0 != observation_id
            || observation_id != format!("mapping-bundle:{}", bundle.digest)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        self.recheck_mapping_bundle(&bundle)?;
        verified_bundles.insert(observation_id.to_owned());
        Ok(())
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

    pub(crate) fn verify_committed_event_bundles(
        &self,
        event_keys: &[String; 5],
    ) -> Result<(), CellError> {
        let mut cache = DisputeVerifyCache::default();
        self.verify_committed_event_bundles_cached(event_keys, &mut cache)
    }

    fn verify_committed_event_bundles_cached(
        &self,
        event_keys: &[String; 5],
        cache: &mut DisputeVerifyCache,
    ) -> Result<(), CellError> {
        if cache.verified_events.contains(event_keys) {
            return Ok(());
        }
        let [c, p, s, sid, eid] = event_keys;
        let [connector, partition, source, source_id, event_id]: [String; 5] =
            serde_json::from_slice(&norn_ir::canonical::canonical_json_of(&(
                c, p, s, sid, eid,
            ))?)?;
        let table = crate::system::CLAIM_ENDPOINT;
        let rows = self.conn.query(
            &format!(
                "SELECT DISTINCT \"observation_id\" FROM \"{table}\" \
                 WHERE \"side\" = 'event:' || json_array(?1, ?2, ?3, ?4, ?5)"
            ),
            params![&connector, &partition, &source, &source_id, &event_id],
        )?;
        for values in rows {
            let observation_id: String = crate::engine::SqlRow(values).get(0)?;
            self.verify_committed_bundle_event_cached(
                &observation_id,
                &mut cache.verified_bundles,
            )?;
        }
        cache.verified_events.insert(event_keys.clone());
        Ok(())
    }

    fn event_has_committed_readable_bundle(
        &self,
        event_keys: &[String; 5],
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
        verified_bundles: &mut BTreeSet<String>,
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
        let mut matched = false;
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let subject_json: String = row.get(0)?;
            let observation_id: String = row.get(1)?;
            if subject_json.is_empty() {
                continue;
            }
            let [object_type, subject_key]: [String; 2] = serde_json::from_str(&subject_json)
                .map_err(|_err| CellError::MappingBundleMismatch)?;
            if !self.subject_matches_readable_active(&object_type, &subject_key, readable_map) {
                continue;
            }
            self.verify_committed_bundle_event_cached(&observation_id, verified_bundles)?;
            matched = true;
        }
        Ok(matched)
    }

    fn event_has_unconsumed_readable_bundle(
        &self,
        event_keys: &[String; 5],
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
        retained_roots: &mut BTreeMap<(u64, String), String>,
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
                    retained_roots,
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
        cache: &mut DisputeVerifyCache,
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
                &mut cache.retained_roots,
            );
            let event_keys =
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)
                    .ok()
                    .map(|keys| keys.map(|k| norn_ir::canonical::nfc(&k)));
            let relevant = direct
                || match event_keys.as_ref() {
                    Some(keys) => {
                        self.event_has_committed_readable_bundle(
                            keys,
                            readable_map,
                            &mut cache.verified_bundles,
                        )? || self.event_has_unconsumed_readable_bundle(
                            keys,
                            readable_map,
                            &mut cache.retained_roots,
                        )?
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
            let verified_keys = event_keys.ok_or(CellError::MappingBundleMismatch)?;
            self.verify_committed_event_bundles_cached(&verified_keys, cache)?;
            let existing =
                self.cached_source_event_bundles(&conflict.incoming, &mut cache.event_bundles)?;
            let expected = match classify_source_event_with(&conflict.incoming, existing)? {
                SourceEventCapture::Conflict(expected) => Some(*expected),
                SourceEventCapture::Fresh | SourceEventCapture::Replay(_) => None,
            };
            if expected.as_ref() != Some(&conflict) {
                continue;
            }
            index.dispute(verified_keys, position, stored_id, &conflict.finding);
            self.index_source_finding_with(
                index,
                existing,
                &conflict.incoming,
                false,
                &conflict.finding,
                &mut cache.retained_roots,
            )?;
        }
        Ok(())
    }

    fn collect_scoped_identity_evidence(
        &self,
        rows: Vec<Vec<crate::engine::SqlValue>>,
        readable_map: &BTreeMap<&str, Option<BTreeSet<String>>>,
        index: &mut SourceDisputeIndex,
        cache: &mut DisputeVerifyCache,
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
                &mut cache.retained_roots,
            );
            let event_keys = crate::mapping_identity::keys(&guard.bundle.raw, &guard.bundle.input)
                .ok()
                .map(|keys| keys.map(|k| norn_ir::canonical::nfc(&k)));
            let relevant = direct
                || match event_keys.as_ref() {
                    Some(keys) => {
                        self.event_has_committed_readable_bundle(
                            keys,
                            readable_map,
                            &mut cache.verified_bundles,
                        )? || self.event_has_unconsumed_readable_bundle(
                            keys,
                            readable_map,
                            &mut cache.retained_roots,
                        )?
                    }
                    None => false,
                };
            if !relevant {
                continue;
            }
            if let Err(err) = self.recheck_identity_evidence(&guard, &stored_id) {
                if crate::mapping_page::is_store_failure(&err) {
                    return Err(err);
                }
                continue;
            }
            let verified_keys =
                crate::mapping_identity::keys(&guard.bundle.raw, &guard.bundle.input)?;
            let nfc_keys = event_keys.unwrap_or_else(|| {
                verified_keys
                    .clone()
                    .map(|key| norn_ir::canonical::nfc(&key))
            });
            self.verify_committed_event_bundles_cached(&nfc_keys, cache)?;
            index.dispute(verified_keys, position, stored_id, &guard.finding);
            let related =
                self.cached_source_event_bundles(&guard.bundle, &mut cache.event_bundles)?;
            self.index_source_finding_with(
                index,
                related,
                &guard.bundle,
                true,
                &guard.finding,
                &mut cache.retained_roots,
            )?;
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
        // Case-insensitive range bounds (`:` = 0x3A, `;` = 0x3B) use `_norn_inbox/id_nocase`
        // with bound parameters so no literal `;` appears in SQL (`WriteRevision::read_only`).
        let conflict_rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM {INBOX} \
                 WHERE observation_id COLLATE NOCASE >= ?1 \
                   AND observation_id COLLATE NOCASE < ?2 \
                 ORDER BY position"
            ),
            params!["mapping-conflict:", "mapping-conflict;"],
        )?;
        let identity_rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM {INBOX} \
                 WHERE observation_id COLLATE NOCASE >= ?1 \
                   AND observation_id COLLATE NOCASE < ?2 \
                 ORDER BY position"
            ),
            params!["mapping-identity:", "mapping-identity;"],
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
        let mut cache = DisputeVerifyCache::default();
        self.collect_scoped_source_conflicts(
            conflict_rows,
            &readable_map,
            &mut scoped_index,
            &mut cache,
        )?;
        self.collect_scoped_identity_evidence(
            identity_rows,
            &readable_map,
            &mut scoped_index,
            &mut cache,
        )?;
        if scoped_index.subjects.is_empty() {
            return Ok(0);
        }
        let mut open = 0_usize;
        for (ty, key, roots) in &readable_roots {
            if !scoped_index.has_subject(roots.as_deref(), key) {
                continue;
            }
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

/// Cached landed version candidates `(observation_id, parsed_version, cached_connection_match)`
/// per `(dataset, connector_instance, partition, source, source_id)`.
/// Connection attribution is checked lazily only when a candidate actually supersedes `q_ver`,
/// preserving exact error-surfacing semantics for equal, older, or incomparable landed rows.
pub(crate) type SupersededVersionCache = BTreeMap<
    (String, String, String, String, String),
    Vec<(String, norn_ir::SourceVersion, Option<bool>)>,
>;

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

pub(crate) fn bundle_superseded_by_newer<E: SqlEngine>(
    conn: &E,
    mapping_id: &str,
    connection_id: Option<&str>,
    bundle: &MappingBundle,
    frontier: Option<i64>,
    cache: &mut SupersededVersionCache,
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
    let key = (
        bundle.input.dataset.clone(),
        bundle.input.connector_instance.clone(),
        bundle.input.partition.clone(),
        source.to_owned(),
        source_id.to_owned(),
    );
    let versions = match cache.entry(key) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::btree_map::Entry::Vacant(entry) => {
            let consumed_pred = match frontier {
                Some(f) => {
                    format!(
                        "(\"consumed_commit_seq\" IS NOT NULL AND \"consumed_commit_seq\" <= {f})"
                    )
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
            let mut parsed = Vec::new();
            for row_vals in landed {
                let row = crate::engine::SqlRow(row_vals);
                let obs_id: String = row.get(0)?;
                let Ok(Some(ver)) = row.get::<Option<String>>(1) else {
                    continue;
                };
                let kind_wire = row.get::<Option<String>>(2).ok().flatten();
                if let Some(lv) = parse_version_parts(&ver, kind_wire.as_deref()) {
                    parsed.push((obs_id, lv, None));
                }
            }
            entry.insert(parsed)
        }
    };
    for (obs_id, lv, cached_match) in versions.iter_mut() {
        if lv.try_cmp(&q_ver) != Ok(std::cmp::Ordering::Greater) {
            continue;
        }
        let matched = if let Some(matched) = *cached_match {
            matched
        } else {
            let matched = landed_matches_connection(conn, obs_id, connection_id, frontier)?;
            *cached_match = Some(matched);
            matched
        };
        if matched {
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
