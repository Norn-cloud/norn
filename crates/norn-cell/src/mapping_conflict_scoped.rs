//! Scoped source-dispute counting for `HOST-READ-2` / `OBS-LINK-DISPUTE-1`
//! combined-view cuts, kept separate from [`crate::mapping_conflict`] so
//! each module stays within the repository non-test LOC cap.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{DataQualityFinding, DataQualityFindingStatus, RuntimeValue};
use norn_kernel::Observed;

use crate::engine::{SqlEngine, params};
use crate::mapping_bundle::MappingBundle;
use crate::mapping_conflict::{MappingConflict, SourceDisputeIndex};
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
            if !relevant
                || self
                    .verified_dispute_resolution_seq(&stored_id, None)?
                    .is_some()
            {
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
            if !relevant
                || self
                    .verified_dispute_resolution_seq(&stored_id, None)?
                    .is_some()
            {
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

    /// OBS-DISPUTE-LOOKUP-1: cryptographically authenticate one
    /// `_norn_dispute_resolution` row against `_norn_receipt`, the hash-chained
    /// `_norn_ledger` span anchored to `receipt.body.ledger_head` (and the prior
    /// receipt's head), `dispute_consumption_valid`, and `_norn_export_journal`
    /// (receipt, storage version, and exact entries) before any live reader
    /// trusts it.
    pub(crate) fn verify_dispute_resolution_record(
        &self,
        rec: &crate::link_claim_frontier::DisputeResolutionRecord,
    ) -> Result<crate::export::ExportRecord, CellError> {
        let corrupt = |msg: &str| CellError::LedgerCorrupt {
            seq: rec.commit_seq,
            reason: format!(
                "unverified dispute resolution for {}: {msg}",
                rec.dispute_id
            ),
        };
        if !matches!(
            rec.decision.as_str(),
            "accept-first" | "accept-second-as-correction" | "reject"
        ) {
            return Err(corrupt("invalid decision"));
        }
        let Some(receipt) = self.receipt_at(rec.commit_seq)? else {
            return Err(corrupt("missing receipt"));
        };
        if receipt.hash != rec.receipt_hash
            || receipt.body.world != crate::MAIN_WORLD
            || !crate::link_claim_frontier::dispute_consumption_valid(
                self.conn,
                rec.commit_seq,
                &rec.dispute_id,
                receipt.body.consumed.as_ref(),
            )?
        {
            return Err(corrupt("receipt mismatch"));
        }
        let entries = self.authenticated_commit_entries(&receipt, &corrupt)?;
        if !entries
            .iter()
            .any(|entry| matches_dispute_resolution_event(&entry.event, rec))
        {
            return Err(corrupt("missing authenticated dispute_resolved event"));
        }
        let has_journal = self
            .conn
            .query_optional(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
                params![],
                |_row| Ok(()),
            )?
            .is_some();
        if !has_journal {
            return Err(corrupt("missing export journal table"));
        }
        let Some(record) = crate::export_journal::read_record(self.conn, rec.commit_seq)? else {
            return Err(corrupt("missing export journal record"));
        };
        if record.storage_version.unwrap_or(1)
            < crate::link_claim_frontier::FRONTIER_STORAGE_VERSION
            || record.receipt != receipt
            || record.entries != entries
        {
            return Err(corrupt("export journal mismatch"));
        }
        Ok(record)
    }

    /// Authenticate a `_norn_dispute_resolution` row and return the dispute's
    /// hash-authenticated `MappingBundle` bound to `_norn_export_journal`
    /// (`consumed_input`) and cryptographically rechecked against retained
    /// definitions — never trusting unverified `_norn_inbox` bytes.
    pub(crate) fn verified_dispute_resolution_bundle(
        &self,
        rec: &crate::link_claim_frontier::DisputeResolutionRecord,
    ) -> Result<crate::mapping_bundle::MappingBundle, CellError> {
        let corrupt = |msg: &str| CellError::LedgerCorrupt {
            seq: rec.commit_seq,
            reason: format!(
                "unverified dispute resolution for {}: {msg}",
                rec.dispute_id
            ),
        };
        let export = self.verify_dispute_resolution_record(rec)?;
        let observed_val = self.authenticated_dispute_observed(rec, &export, &corrupt)?;
        let observed: norn_kernel::Observed<norn_ir::RuntimeValue> =
            norn_ir::runtime::decode(&observed_val)
                .map_err(|err| corrupt(&format!("invalid dispute Observed: {err}")))?;
        if observed.id.0 != rec.dispute_id {
            return Err(corrupt("observed id mismatch"));
        }
        if rec.dispute_id.starts_with("mapping-conflict:") {
            let conflict: crate::mapping_conflict::MappingConflict =
                norn_ir::runtime::decode(&observed.input)
                    .map_err(|err| corrupt(&format!("invalid conflict evidence: {err}")))?;
            if rec.dispute_id != format!("mapping-conflict:{}", conflict.finding.id) {
                return Err(corrupt("conflict finding id mismatch"));
            }
            self.recheck_mapping_conflict(&conflict)
                .map_err(|err| corrupt(&format!("conflict evidence failed recheck: {err}")))?;
            Ok(conflict.incoming)
        } else if rec.dispute_id.starts_with("mapping-identity:") {
            let guard: crate::mapping_identity::IdentityEvidence =
                norn_ir::runtime::decode(&observed.input)
                    .map_err(|err| corrupt(&format!("invalid identity evidence: {err}")))?;
            self.recheck_identity_evidence(&guard, &rec.dispute_id)
                .map_err(|err| corrupt(&format!("identity evidence failed recheck: {err}")))?;
            Ok(guard.bundle)
        } else {
            Err(corrupt("unknown dispute observation prefix"))
        }
    }

    fn authenticated_dispute_observed(
        &self,
        rec: &crate::link_claim_frontier::DisputeResolutionRecord,
        export: &crate::export::ExportRecord,
        corrupt: &dyn Fn(&str) -> CellError,
    ) -> Result<norn_ir::RuntimeValue, CellError> {
        if export.receipt.body.consumed.is_some() {
            let Some(consumed_val) = export.consumed_input.as_ref() else {
                return Err(corrupt("missing consumed_input in export journal"));
            };
            let crate::export_journal::ConsumedInput::Observation {
                observation_id,
                observed,
                ..
            } = crate::export_journal::decode_observation_input(consumed_val)
                .map_err(|err| corrupt(&format!("invalid consumed input: {err}")))?
            else {
                return Err(corrupt("resolution consumed non-observation input"));
            };
            if observation_id != rec.dispute_id {
                return Err(corrupt("consumed observation id mismatch"));
            }
            return Ok(observed);
        }
        // Legacy pre-v13 generically-consumed dispute resolved at v13 without
        // re-consuming: require the prior consuming commit's export journal
        // record and its hash-verified `consumed_input` (never trust raw
        // `_norn_inbox.observed` bytes).
        let Some(consumed_seq) = self.conn.query_optional(
            &format!(
                "SELECT \"consumed_commit_seq\" FROM \"{}\" WHERE \"observation_id\" = ?1",
                crate::system::INBOX
            ),
            params![&rec.dispute_id],
            |row| row.get::<Option<i64>>(0),
        )?
        else {
            return Err(corrupt("missing legacy dispute inbox row"));
        };
        let Some(prev_i64) = consumed_seq else {
            return Err(corrupt(
                "unconsumed dispute resolution lacks consumed_input",
            ));
        };
        let prev_seq = u64::try_from(prev_i64)
            .map_err(|err| corrupt(&format!("negative prior consumed_commit_seq: {err}")))?;
        self.authenticated_prior_consumed_observed(&rec.dispute_id, prev_seq)
    }

    /// Authenticate the prior pre-v13 consuming commit `prev_seq` for a legacy
    /// generically-consumed `dispute_id` against BOTH its durable receipt
    /// (`receipt.body.consumed == Some(Observation { dispute_id })`) and its
    /// hash-verified `_norn_export_journal` record (`storage_version < 13`,
    /// `record.receipt == receipt`, `consumed_input`), returning the
    /// authenticated `Observed` JSON payload. Shared by both live read
    /// verification and `mapping.dispute.resolve` write admission.
    pub(crate) fn authenticated_prior_consumed_observed(
        &self,
        dispute_id: &str,
        prev_seq: u64,
    ) -> Result<norn_ir::RuntimeValue, CellError> {
        let corrupt = |msg: &str| CellError::LedgerCorrupt {
            seq: prev_seq,
            reason: format!("unverified prior consumption for {dispute_id}: {msg}"),
        };
        let Some(prior_receipt) = self.receipt_at(prev_seq)? else {
            return Err(corrupt("missing prior consuming receipt"));
        };
        let expected_consumed = crate::Consumption::Observation {
            observation_id: dispute_id.to_owned(),
        };
        if prior_receipt.body.world != crate::MAIN_WORLD
            || prior_receipt.body.consumed.as_ref() != Some(&expected_consumed)
        {
            return Err(corrupt(
                "prior consuming receipt does not consume this dispute",
            ));
        }
        let has_journal = self
            .conn
            .query_optional(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
                params![],
                |_row| Ok(()),
            )?
            .is_some();
        if !has_journal {
            return Err(corrupt("missing export journal table"));
        }
        let Some(prior_export) = crate::export_journal::read_record(self.conn, prev_seq)? else {
            return Err(corrupt("missing prior consuming export journal record"));
        };
        if prior_export.receipt != prior_receipt
            || prior_export.storage_version.unwrap_or(1)
                >= crate::link_claim_frontier::FRONTIER_STORAGE_VERSION
        {
            return Err(corrupt(
                "prior consuming export journal disagrees with pre-v13 receipt",
            ));
        }
        let Some(prior_consumed) = prior_export.consumed_input.as_ref() else {
            return Err(corrupt("prior consuming journal missing consumed_input"));
        };
        let crate::export_journal::ConsumedInput::Observation {
            observation_id,
            observed,
            ..
        } = crate::export_journal::decode_observation_input(prior_consumed)
            .map_err(|err| corrupt(&format!("invalid prior consumed input: {err}")))?
        else {
            return Err(corrupt("prior consuming journal is not an observation"));
        };
        if observation_id != dispute_id {
            return Err(corrupt("prior consumed observation id mismatch"));
        }
        Ok(observed)
    }

    /// Read and authenticate every ledger entry in `receipt`'s exact event span
    /// against both the prior receipt's head (or genesis) and `receipt.body.ledger_head`.
    fn authenticated_commit_entries(
        &self,
        receipt: &crate::store::Receipt,
        corrupt: &dyn Fn(&str) -> CellError,
    ) -> Result<Vec<crate::read::LedgerEntry>, CellError> {
        let mut expected_prev = if receipt.body.commit_seq == 1 {
            if receipt.body.first_event_seq != 1 {
                return Err(corrupt("first commit does not start at seq 1"));
            }
            crate::system::GENESIS_HASH.to_owned()
        } else {
            let Some(prev) = self.receipt_at(receipt.body.commit_seq.saturating_sub(1))? else {
                return Err(corrupt("missing prior receipt"));
            };
            if prev.body.first_event_seq.checked_add(prev.body.event_count)
                != Some(receipt.body.first_event_seq)
            {
                return Err(corrupt("receipt event span does not chain"));
            }
            prev.body.ledger_head
        };
        let end = receipt
            .body
            .first_event_seq
            .checked_add(receipt.body.event_count)
            .ok_or_else(|| corrupt("event span overflow"))?;
        let first_i64 = crate::read::to_i64(receipt.body.first_event_seq, "first_event_seq")?;
        let end_i64 = crate::read::to_i64(end, "end_event_seq")?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"seq\", \"commit_seq\", \"world\", \"event\", \"prev_hash\", \"hash\" \
                 FROM \"{}\" WHERE \"seq\" >= ?1 AND \"seq\" < ?2 ORDER BY \"seq\"",
                crate::system::LEDGER
            ),
            params![first_i64, end_i64],
        )?;
        if u64::try_from(rows.len()).ok() != Some(receipt.body.event_count) {
            return Err(corrupt("ledger event count disagrees with receipt"));
        }
        let mut out = Vec::with_capacity(rows.len());
        let mut expected_seq = receipt.body.first_event_seq;
        for raw in rows {
            let row = crate::engine::SqlRow(raw);
            let seq = crate::read::to_u64(row.get(0)?, "seq")?;
            let commit_seq = crate::read::to_u64(row.get(1)?, "commit_seq")?;
            let world: String = row.get(2)?;
            let event_raw: String = row.get(3)?;
            let prev_hash: String = row.get(4)?;
            let hash: String = row.get(5)?;
            if seq != expected_seq
                || commit_seq != receipt.body.commit_seq
                || world != receipt.body.world
                || prev_hash != expected_prev
            {
                return Err(corrupt("ledger chain mismatch"));
            }
            let event = crate::payload_storage::decode_event(self.conn, seq, &world, &event_raw)?;
            if crate::read::entry_hash_with_pin(
                seq,
                commit_seq,
                &world,
                &event,
                &prev_hash,
                receipt.body.fork_pin.as_ref(),
            )? != hash
            {
                return Err(corrupt("ledger entry hash mismatch"));
            }
            expected_prev = hash.clone();
            expected_seq = expected_seq
                .checked_add(1)
                .ok_or_else(|| corrupt("seq overflow"))?;
            out.push(crate::read::LedgerEntry {
                seq,
                commit_seq,
                world,
                event,
                prev_hash,
                hash,
                fork_pin: receipt.body.fork_pin.clone(),
            });
        }
        if expected_prev != receipt.body.ledger_head {
            return Err(corrupt("ledger head disagrees with receipt"));
        }
        Ok(out)
    }

    /// Look up and authenticate the single `_norn_dispute_resolution` row for
    /// `dispute_id`, returning its verified commit sequence (filtered by
    /// optional `max_seq`).
    pub(crate) fn verified_dispute_resolution_seq(
        &self,
        dispute_id: &str,
        max_seq: Option<i64>,
    ) -> Result<Option<u64>, CellError> {
        let Some(rec) = self.conn.query_optional(
            &format!(
                "SELECT \"dispute_id\", \"decision\", \"actor\", \"reason\", \"commit_seq\", \"receipt_hash\" \
                 FROM \"{}\" WHERE \"dispute_id\" = ?1",
                crate::link_claim_frontier::DISPUTE_RESOLUTION
            ),
            params![dispute_id],
            decode_dispute_resolution_row,
        )?
        else {
            self.ensure_no_missing_dispute_resolution(dispute_id)?;
            return Ok(None);
        };
        self.verify_dispute_resolution_record(&rec)?;
        if let Some(f) = max_seq
            && (f < 0 || rec.commit_seq > u64::try_from(f).unwrap_or(0))
        {
            return Ok(None);
        }
        Ok(Some(rec.commit_seq))
    }

    /// Fail closed if `_norn_ledger` already records a v13+ governed
    /// `dispute_resolved` event for `dispute_id` whose `_norn_dispute_resolution`
    /// index row disappeared while the cell was open.
    pub(crate) fn ensure_no_missing_dispute_resolution(
        &self,
        dispute_id: &str,
    ) -> Result<(), CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"commit_seq\" FROM \"{}\" \
                 WHERE \"world\" = ?1 \
                   AND json_extract(\"event\", '$.op') = 'dispute_resolved' \
                   AND json_extract(\"event\", '$.disputeId') = ?2",
                crate::system::LEDGER
            ),
            params![crate::MAIN_WORLD, dispute_id],
        )?;
        if rows.is_empty() {
            return Ok(());
        }
        let has_journal = self
            .conn
            .query_optional(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
                params![],
                |_row| Ok(()),
            )?
            .is_some();
        if !has_journal {
            return Err(CellError::LedgerCorrupt {
                seq: 0,
                reason: format!(
                    "missing export journal table while verifying resolved dispute {dispute_id}"
                ),
            });
        }
        for raw in rows {
            let seq_i64: i64 = crate::engine::SqlRow(raw).get(0)?;
            let commit_seq = u64::try_from(seq_i64).map_err(|err| CellError::LedgerCorrupt {
                seq: 0,
                reason: format!("invalid ledger commit_seq for {dispute_id}: {err}"),
            })?;
            let is_pre_v13 = crate::export_journal::read_record(self.conn, commit_seq)?
                .is_some_and(|rec| {
                    rec.storage_version.unwrap_or(1)
                        < crate::link_claim_frontier::FRONTIER_STORAGE_VERSION
                });
            if !is_pre_v13 {
                return Err(CellError::LedgerCorrupt {
                    seq: commit_seq,
                    reason: format!(
                        "missing _norn_dispute_resolution index row for resolved dispute {dispute_id}"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Whether `commit_seq` is a verified dispute-resolution commit (failing
    /// closed with `LedgerCorrupt` if any indexed row at `commit_seq` is forged).
    pub(crate) fn commit_is_verified_dispute_resolution(
        &self,
        commit_seq: i64,
    ) -> Result<bool, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"dispute_id\", \"decision\", \"actor\", \"reason\", \"commit_seq\", \"receipt_hash\" \
                 FROM \"{}\" WHERE \"commit_seq\" = ?1",
                crate::link_claim_frontier::DISPUTE_RESOLUTION
            ),
            params![commit_seq],
        )?;
        let found = !rows.is_empty();
        for raw in rows {
            let rec = decode_dispute_resolution_row(&crate::engine::SqlRow(raw))?;
            self.verify_dispute_resolution_record(&rec)?;
        }
        Ok(found)
    }
}

pub(crate) fn decode_dispute_resolution_row(
    row: &crate::engine::SqlRow,
) -> Result<crate::link_claim_frontier::DisputeResolutionRecord, CellError> {
    let dispute_id: String = row.get(0)?;
    let decision: String = row.get(1)?;
    let actor: String = row.get(2)?;
    let reason: String = row.get(3)?;
    let seq_i64: i64 = row.get(4)?;
    let receipt_hash: String = row.get(5)?;
    let Ok(commit_seq) = u64::try_from(seq_i64) else {
        return Err(CellError::LedgerCorrupt {
            seq: 0,
            reason: format!("unverified dispute resolution for {dispute_id}: negative commit_seq"),
        });
    };
    Ok(crate::link_claim_frontier::DisputeResolutionRecord {
        dispute_id,
        decision,
        actor,
        reason,
        commit_seq,
        receipt_hash,
    })
}

fn matches_dispute_resolution_event(
    event: &norn_ir::RuntimeValue,
    rec: &crate::link_claim_frontier::DisputeResolutionRecord,
) -> bool {
    event.get("op").and_then(norn_ir::RuntimeValue::as_str) == Some("dispute_resolved")
        && event
            .get("disputeId")
            .and_then(norn_ir::RuntimeValue::as_str)
            == Some(rec.dispute_id.as_str())
        && event
            .get("decision")
            .and_then(norn_ir::RuntimeValue::as_str)
            == Some(rec.decision.as_str())
        && event.get("actor").and_then(norn_ir::RuntimeValue::as_str) == Some(rec.actor.as_str())
        && event.get("reason").and_then(norn_ir::RuntimeValue::as_str) == Some(rec.reason.as_str())
}

/// Record governed dispute resolutions for a committed v13+ main-world batch.
pub(crate) fn record_commit_dispute_resolutions<E: SqlEngine>(
    conn: &E,
    batch: &crate::store::CommitBatch,
    commit_seq: u64,
    receipt_hash: &str,
    storage_version: Option<u32>,
    allow_dispute_resolution: bool,
    entries: &[crate::read::LedgerEntry],
) -> Result<(), CellError> {
    if batch.world == crate::MAIN_WORLD
        && storage_version
            .is_some_and(|v| v >= crate::link_claim_frontier::FRONTIER_STORAGE_VERSION)
    {
        crate::link_claim_frontier::record_dispute_resolutions_in_tx(
            conn,
            commit_seq,
            receipt_hash,
            batch.consumes.as_ref(),
            allow_dispute_resolution,
            entries,
        )?;
    }
    Ok(())
}
