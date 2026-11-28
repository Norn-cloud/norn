//! Scoped source-dispute counting for `HOST-READ-2` / `OBS-LINK-DISPUTE-1`
//! combined-view cuts, kept separate from [`crate::mapping_conflict`] so
//! each module stays within the repository non-test LOC cap.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{DataQualityFinding, DataQualityFindingStatus, RuntimeValue};
use norn_kernel::Observed;

use crate::engine::{SqlEngine, params};
use crate::mapping_bundle::MappingBundle;
use crate::mapping_conflict::{EventDispute, MappingConflict, SourceDisputeIndex};
use crate::system::INBOX;
use crate::{CellError, StoreReader};

impl SourceDisputeIndex {
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
            serde_json::from_str(&encoded).map_err(|_err| CellError::MappingBundleMismatch)?;
        let bundle: MappingBundle = norn_ir::runtime::decode(&observed.input)
            .map_err(|_err| CellError::MappingBundleMismatch)?;
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

    pub(crate) fn verify_committed_event_bundles(
        &self,
        event_keys: &[String; 5],
    ) -> Result<(), CellError> {
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
            self.verify_committed_bundle_event(&observation_id)?;
        }
        Ok(())
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
        let mut matched = false;
        let mut checked = BTreeSet::new();
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
            if checked.insert(observation_id.clone()) {
                self.verify_committed_bundle_event(&observation_id)?;
            }
            matched = true;
        }
        Ok(matched)
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
            let verified_keys =
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)?
                    .map(|key| norn_ir::canonical::nfc(&key));
            self.verify_committed_event_bundles(&verified_keys)?;
            let expected = self.mapping_conflict(&conflict.incoming)?;
            if expected.as_ref() != Some(&conflict) {
                continue;
            }
            index.dispute(verified_keys, position, stored_id, &conflict.finding);
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
            if let Err(err) = self.recheck_identity_evidence(&guard, &stored_id) {
                if crate::mapping_page::is_store_failure(&err) {
                    return Err(err);
                }
                continue;
            }
            let verified_keys =
                crate::mapping_identity::keys(&guard.bundle.raw, &guard.bundle.input)?;
            self.verify_committed_event_bundles(
                &verified_keys
                    .clone()
                    .map(|key| norn_ir::canonical::nfc(&key)),
            )?;
            index.dispute(verified_keys, position, stored_id, &guard.finding);
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
        let res = crate::link_claim_frontier::DISPUTE_RESOLUTION;
        let unresolved = format!(
            "NOT EXISTS (SELECT 1 FROM \"{res}\" r WHERE r.\"dispute_id\" = {INBOX}.observation_id)"
        );
        let conflict_rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM {INBOX} \
                 WHERE observation_id >= 'mapping-conflict:' AND observation_id < 'mapping-conflict;' \
                   AND {unresolved} \
                 ORDER BY position"
            ),
            params![],
        )?;
        let identity_rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM {INBOX} \
                 WHERE observation_id >= 'mapping-identity:' AND observation_id < 'mapping-identity;' \
                   AND {unresolved} \
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
