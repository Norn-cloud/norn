//! OBS-PAGE-1: a page of source records is one delivery, never one record.
//!
//! Capture derives each record's bundle exactly as one-by-one capture does
//! (its own inbox row, conflict and identity checks), then records the page
//! as one or more page rows: consecutive runs of the page's mapped records,
//! split before any record that writes a canonical object an earlier record
//! of the run writes. Each page row is one delivery, consumed by one commit
//! that applies its still-pending records in page order; the kernel's one
//! write per key per commit then never merges two records' writes, so
//! versions, events and the CAS chain are the one-by-one ones. A record the
//! mapping or the cell refuses stays pending as quarantined evidence, as it
//! does one by one: such a page is applied record by record, and its row is
//! consumed (applying nothing) once no member is left pending.
use crate::{
    CellError, CellStore, SqliteCell, StoreReader,
    engine::SqlEngine,
    mapping_bundle::{MappingBundle, MappingInput},
    mapping_process::MappingOutcome,
};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    CellCommit, CommitModel, DecisionTurn, FactKey, ObservationAuthorization, ObservationEvent,
    ObservationId, Observed, TypeCatalog,
};
use std::collections::BTreeSet;

/// The most records one page carries.
pub const MAX_PAGE_RECORDS: usize = 1_000;
/// The most canonical payload bytes one page carries.
pub const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
/// The page row's observation id prefix.
pub(crate) const PAGE_PREFIX: &str = "mapping-page:";
const PAGE_CONTRACT: &str = "norn.mapping_page/v1";

/// The member bundle ids a page row names, in page order.
pub(crate) fn members(observed: &RuntimeValue) -> Result<Vec<String>, CellError> {
    let input = observed
        .get("input")
        .ok_or(CellError::MappingBundleMismatch)?;
    if input.get("contract").and_then(RuntimeValue::as_str) != Some(PAGE_CONTRACT) {
        return Err(CellError::MappingBundleMismatch);
    }
    input
        .get("members")
        .and_then(RuntimeValue::as_array)
        .ok_or(CellError::MappingBundleMismatch)?
        .iter()
        .map(|member| {
            member
                .as_str()
                .filter(|id| id.starts_with("mapping-bundle:"))
                .map(str::to_owned)
                .ok_or(CellError::MappingBundleMismatch)
        })
        .collect()
}

/// What capturing a page recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPage {
    /// The page rows (one delivery each), in order, with whether each was
    /// newly captured.
    pub pages: Vec<(String, bool)>,
    /// Each record's own inbox row, in page order: a `mapping-bundle:` row is
    /// a member of one of `pages`; a conflict or identity finding is not.
    pub records: Vec<String>,
}

impl<E: SqlEngine> CellStore<E> {
    /// Capture a page of raw source records read from one dataset. Framing
    /// (record count, payload bytes, a repeated record identity) refuses the
    /// whole page before anything is captured.
    ///
    /// # Errors
    /// Framing refusals, invalid pins, or the durable inbox failure.
    pub fn inject_mapping_page(
        &mut self,
        raws: &[Observed<RuntimeValue>],
        input: &MappingInput,
    ) -> Result<CapturedPage, CellError> {
        if raws.is_empty() || raws.len() > MAX_PAGE_RECORDS {
            return Err(CellError::OutOfRange("mapping page records"));
        }
        let mut bytes = 0_usize;
        let mut identities = BTreeSet::new();
        for raw in raws {
            let payload = raw
                .input
                .get("payload")
                .ok_or(CellError::MappingBundleMismatch)?;
            bytes = bytes.saturating_add(serde_json::to_string(payload)?.len());
            let [connector, partition, source, source_id, _event] =
                crate::mapping_identity::keys(&raw.input, input)?;
            if !identities.insert((connector, partition, source, source_id)) {
                // The host splits a batch before a repeated identity (OBS-PAGE-1).
                return Err(CellError::MappingBundleMismatch);
            }
        }
        if bytes > MAX_PAGE_BYTES {
            return Err(CellError::OutOfRange("mapping page bytes"));
        }
        let model = self.model().clone();
        let catalog = TypeCatalog::new(model.object_types()).with_interfaces(model.interfaces());
        let canonical = CommitModel::new(&catalog);
        self.conn.transaction(|| {
            let mut runs: Vec<(Vec<String>, BTreeSet<FactKey>, &Observed<RuntimeValue>)> =
                Vec::new();
            let mut records = Vec::with_capacity(raws.len());
            for raw in raws {
                let (id, _new) = self.capture_mapping_bundle(raw, input)?;
                records.push(id.clone());
                if !id.starts_with("mapping-bundle:") {
                    // Conflict and identity findings stay pending, as one by one.
                    continue;
                }
                let bundle = self.read().inbox_bundle(&id)?;
                let keys: BTreeSet<FactKey> = bundle
                    .records
                    .iter()
                    .map(|r| canonical.canonical(&FactKey::new(r.object_type.as_str(), &r.key)))
                    .collect();
                match runs.last_mut() {
                    Some((ids, written, _)) if written.is_disjoint(&keys) => {
                        ids.push(id);
                        written.extend(keys);
                    }
                    _ => runs.push((vec![id], keys, raw)),
                }
            }
            let mut pages = Vec::with_capacity(runs.len());
            for (ids, _, first) in runs {
                let digest = norn_ir::canonical::domain_hash_hex(b"norn.mapping.page.v1\0", &ids)?;
                let id = format!("{PAGE_PREFIX}{digest}");
                let observed = Observed {
                    id: ObservationId(id.clone()),
                    provenance: first.provenance.clone(),
                    input: norn_ir::runtime_object! {
                        "contract" => PAGE_CONTRACT,
                        "members" => RuntimeValue::Array(ids.into_iter().map(RuntimeValue::from).collect()),
                    },
                };
                let inserted = crate::store::inject_observation(
                    &self.conn,
                    &id,
                    &norn_ir::runtime::encode(&observed).map_err(CellError::Encoding)?,
                )?;
                pages.push((id, inserted));
            }
            Ok(CapturedPage { pages, records })
        })
    }
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The bundle a `mapping-bundle:` inbox row retains, consumed or not.
    pub(crate) fn inbox_bundle(&self, id: &str) -> Result<MappingBundle, CellError> {
        let encoded: String = self
            .conn
            .query_optional(
                &format!(
                    "SELECT \"observed\" FROM \"{}\" WHERE \"observation_id\" = ?1",
                    crate::system::INBOX
                ),
                crate::engine::params![id],
                |row| row.get(0),
            )?
            .ok_or(CellError::MappingBundleMismatch)?;
        let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        if observed.id.0 != id || id != format!("mapping-bundle:{}", bundle.digest) {
            return Err(CellError::MappingBundleMismatch);
        }
        Ok(bundle)
    }

    /// The ids of a pending page's members that are still pending, in page
    /// order, read without decoding any row; `None` when the page itself is
    /// not pending. The commit guard compares them with the bundles applied.
    pub(crate) fn pending_page_member_ids(
        &self,
        page: &str,
    ) -> Result<Option<Vec<String>>, CellError> {
        let inbox = crate::system::INBOX;
        let rows = self.conn.query(
            &format!(
                "SELECT p.\"observation_id\", m.\"observation_id\" FROM \"{inbox}\" p \
                 LEFT JOIN json_each(p.\"observed\", '$.input.members') AS listed \
                 LEFT JOIN \"{inbox}\" m ON m.\"observation_id\" = listed.value AND m.\"consumed_commit_seq\" IS NULL \
                 WHERE p.\"observation_id\" = ?1 AND p.\"consumed_commit_seq\" IS NULL ORDER BY listed.key"
            ),
            crate::engine::params![page],
        )?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut ids = Vec::new();
        for row in rows {
            let row = crate::engine::SqlRow(row);
            if let Some(id) = row.get::<Option<String>>(1)? {
                ids.push(id);
            }
        }
        Ok(Some(ids))
    }

    /// A pending page's members that are still pending, in page order, with
    /// their bundles. Members consumed earlier (one by one, or by another
    /// page) are skipped: each record is consumed once.
    pub(crate) fn pending_page_members(
        &self,
        page: &RuntimeValue,
    ) -> Result<Vec<(String, MappingBundle)>, CellError> {
        let mut pending = Vec::new();
        for id in members(page)? {
            if self.pending_observation(&id)?.is_some() {
                let bundle = self.inbox_bundle(&id)?;
                pending.push((id, bundle));
            }
        }
        Ok(pending)
    }
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// Consume one captured page row: every still-pending member in one
    /// commit when all of them apply, else member by member (a refused
    /// member stays pending as quarantined evidence, exactly as one by one)
    /// and then the page row in a commit of its own. Returns each member's
    /// outcome, in page order; `None` when the page was already consumed.
    ///
    /// # Errors
    /// Store failures only; refusals are outcomes.
    pub fn process_mapping_page(
        &mut self,
        page_id: &str,
    ) -> Result<Option<Vec<(String, MappingOutcome)>>, CellError> {
        let Some(pending) = self.store().read().pending_observation(page_id)? else {
            return Ok(None);
        };
        if !page_id.starts_with(PAGE_PREFIX) {
            return Err(CellError::UnsupportedMappingProfile);
        }
        let observed: Observed<RuntimeValue> =
            norn_ir::runtime::decode(&pending.observed).map_err(CellError::Encoding)?;
        if observed.id.0 != page_id {
            return Err(CellError::MappingBundleMismatch);
        }
        let members = self
            .store()
            .read()
            .pending_page_members(&pending.observed)?;
        match self.commit_page(&observed, &members) {
            Ok(()) => {
                return Ok(Some(
                    members
                        .into_iter()
                        .map(|(id, _)| (id, MappingOutcome::Landed))
                        .collect(),
                ));
            }
            Err(error @ (CellError::Sqlite(_) | CellError::Engine(_))) => return Err(error),
            Err(_refused) => {}
        }
        // The per-record path: the same outcomes one-by-one delivery has.
        let mut outcomes = Vec::with_capacity(members.len());
        for (id, _) in &members {
            outcomes.push((id.clone(), self.process_mapping_outcome(id)?));
        }
        // The page is consumed once its last member is: a quarantined member
        // keeps it pending, and it applies when that member is processed again.
        if self
            .store()
            .read()
            .pending_page_members(&pending.observed)?
            .is_empty()
        {
            self.commit_page(&observed, &[])?;
        }
        Ok(Some(outcomes))
    }

    /// One commit consuming `page` and applying `members` in page order.
    fn commit_page(
        &mut self,
        page: &Observed<RuntimeValue>,
        members: &[(String, MappingBundle)],
    ) -> Result<(), CellError> {
        let model = self.store().model().clone();
        let catalog = TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let turn = DecisionTurn {
            id: page.id.0.clone(),
            now: self.now(),
            world: self.store().world(),
            consumes: Some(page.consumed()),
        };
        let mut commit = CellCommit::empty().consuming(page.consumed());
        let mut authorization = ObservationAuthorization::empty();
        let bundles: Vec<MappingBundle> = members.iter().map(|(_, b)| b.clone()).collect();
        for bundle in &bundles {
            self.store().read().check_mapping_bundle(bundle)?;
            self.store().read().prepare_mapping_records(
                bundle,
                &catalog,
                &turn,
                &mut commit,
                &mut authorization,
            )?;
        }
        if !bundles.is_empty() {
            let reader = self.store().read();
            reader.prepare_mapping_links(
                &bundles,
                &catalog,
                &turn,
                &mut commit,
                &mut authorization,
            )?;
            // LAW-LINK-1, as one by one (`process_mapping_bundle`): laws that read
            // a link are evaluated once the page's links are known, over the state
            // this commit leaves; a changed target re-checks the subjects that
            // link to it.
            authorization
                .refresh_link_findings(&catalog, &mut commit, &crate::host::GuardView(&reader))
                .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        }
        authorization
            .seal_snapshot_event_chunks(&commit)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        self.commit_observed_evidence(commit, &authorization, &bundles)
            .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
        Ok(())
    }
}
