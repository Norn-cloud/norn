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
//! mapping refused (its bundle retains diagnostics) joins no page row: it is
//! quarantined evidence before any commit, exactly as one by one, and it ends
//! the run, so the records before it and after it each commit together.
//! OBS-LINK-DISPUTE-1: a link plan reads the disputes that exist when it
//! commits, so a page must never capture a new dispute row (a conflict or
//! identity finding) after a record it commits: one by one commits that
//! record first. Such a page captures nothing
//! ([`CellError::MappingPageSpansDispute`]) and the host delivers its records
//! one at a time, which is the one-by-one path itself. A record the cell
//! refuses at commit stays pending as quarantined evidence, as it does one by one: such a page
//! is applied record by record, and its row is consumed (applying nothing)
//! once no member is left pending.
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

/// The first step of consuming a captured page row
/// ([`SqliteCell::try_mapping_page`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAttempt {
    /// The page was consumed before; nothing committed.
    Consumed,
    /// Every pending member landed in the page's one commit.
    Landed(Vec<(String, MappingOutcome)>),
    /// The cell refused the page commit: these members, in page order, are
    /// processed one by one, then the page is finished.
    Refused(Vec<String>),
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

/// What capturing a source page atomically recorded, including its complete-page
/// content digest for post-capture pending tracking and idempotent replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSourcePage {
    /// Complete-page content digest persisted in `_norn_source_cursor` / `_norn_source_pending`.
    pub page_digest: String,
    /// Captured page rows and per-record inbox ids.
    pub captured: CapturedPage,
}

/// Whether `error` is a storage/engine/fencing/corruption failure (including
/// one wrapped inside [`CellError::ObservationCommit`]) that must propagate
/// rather than being treated as a per-record business refusal or quarantine
/// outcome.
pub(crate) fn is_store_failure(error: &CellError) -> bool {
    if let CellError::ObservationCommit(commit_error) = error {
        return matches!(
            commit_error.as_ref(),
            norn_kernel::CommitError::Host(inner) if is_store_failure(inner)
        );
    }
    matches!(
        error,
        CellError::Sqlite(_)
            | CellError::Engine(_)
            | CellError::Verifying(_)
            | CellError::Fenced(_)
            | CellError::LedgerCorrupt { .. }
    )
}

impl<E: SqlEngine> CellStore<E> {
    fn validate_page_framing(
        raws: &[Observed<RuntimeValue>],
        input: &MappingInput,
        allow_source_page: bool,
    ) -> Result<(), CellError> {
        if raws.len() > MAX_PAGE_RECORDS {
            return Err(CellError::OutOfRange("mapping page records"));
        }
        let mut total_bytes = 0_usize;
        let mut identities = BTreeSet::new();
        for raw in raws {
            let payload = raw
                .input
                .get("payload")
                .ok_or(CellError::MappingBundleMismatch)?;
            let row_bytes = serde_json::to_string(payload)?.len();
            if allow_source_page && row_bytes > MAX_PAGE_BYTES {
                return Err(CellError::OutOfRange("mapping page bytes"));
            }
            total_bytes = total_bytes.saturating_add(row_bytes);
            let [connector, partition, source, source_id, _event] =
                crate::mapping_identity::keys(&raw.input, input)?;
            if !allow_source_page && !identities.insert((connector, partition, source, source_id)) {
                // The host splits a batch before a repeated identity (OBS-PAGE-1).
                return Err(CellError::MappingBundleMismatch);
            }
        }
        if !allow_source_page && total_bytes > MAX_PAGE_BYTES {
            return Err(CellError::OutOfRange("mapping page bytes"));
        }
        Ok(())
    }

    fn capture_mapping_page_rows_in_tx(
        &self,
        raws: &[Observed<RuntimeValue>],
        input: &MappingInput,
        canonical: CommitModel<'_, '_>,
        max_run_rows: usize,
    ) -> Result<CapturedPage, CellError> {
        // Capture writes only inbox rows: a plan proven once holds for
        // every record of the page (INGEST-DERIVE-ONCE-1).
        let _pinned = crate::mapping_bundle::PinnedPlans::open(&self.mapping_plans);
        let run_cap = max_run_rows.clamp(1, MAX_PAGE_RECORDS);
        #[allow(clippy::type_complexity, reason = "in-tx run accumulator")]
        let mut runs: Vec<(
            Vec<String>,
            BTreeSet<FactKey>,
            BTreeSet<[String; 4]>,
            usize,
            &Observed<RuntimeValue>,
        )> = Vec::new();
        let mut records = Vec::with_capacity(raws.len());
        let mut split = false;
        // Whether a record this page commits was captured yet.
        let mut member = false;
        for raw in raws {
            let row_bytes = raw
                .input
                .get("payload")
                .map(serde_json::to_string)
                .transpose()?
                .map_or(0, |text| text.len());
            let [connector, partition, source, source_id, _event] =
                crate::mapping_identity::keys(&raw.input, input)?;
            let identity = [connector, partition, source, source_id];
            let (id, new) = self.capture_mapping_bundle(raw, input)?;
            records.push(id.clone());
            if !id.starts_with("mapping-bundle:") {
                // Conflict and identity findings stay pending, as one by
                // one. OBS-LINK-DISPUTE-1: a new one after a member would
                // exist at that member's commit, which one by one makes
                // before capturing it; the transaction rolls back.
                if new && member {
                    return Err(CellError::MappingPageSpansDispute);
                }
                continue;
            }
            let bundle = self.read().inbox_bundle(&id)?;
            if !bundle.diagnostics.is_empty() {
                // INGEST-BATCH-1: a record the mapping refused is quarantined
                // evidence before any commit, as one by one; it joins no
                // page, so it never sends its page down the per-record path.
                // It still ends the run: records on either side of it never
                // share a commit they would not share one by one
                // (OBS-PAGE-EQUIV).
                split = true;
                continue;
            }
            let keys: BTreeSet<FactKey> = bundle
                .records
                .iter()
                .map(|r| canonical.canonical(&FactKey::new(r.object_type.as_str(), &r.key)))
                .collect();
            member = true;
            match runs.last_mut() {
                Some((ids, written, seen_ids, run_bytes, _))
                    if !split
                        && ids.len() < run_cap
                        && run_bytes.saturating_add(row_bytes) <= MAX_PAGE_BYTES
                        && written.is_disjoint(&keys)
                        && !seen_ids.contains(&identity) =>
                {
                    ids.push(id);
                    written.extend(keys);
                    seen_ids.insert(identity);
                    *run_bytes = run_bytes.saturating_add(row_bytes);
                }
                _ => runs.push((vec![id], keys, BTreeSet::from([identity]), row_bytes, raw)),
            }
            split = false;
        }
        let mut pages = Vec::with_capacity(runs.len());
        for (ids, _, _, _, first) in runs {
            pages.push(self.inject_mapping_page_run(ids, first)?);
        }
        Ok(CapturedPage { pages, records })
    }

    fn inject_mapping_page_run(
        &self,
        ids: Vec<String>,
        first: &Observed<RuntimeValue>,
    ) -> Result<(String, bool), CellError> {
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
        Ok((id, inserted))
    }

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
        if raws.is_empty() {
            return Err(CellError::OutOfRange("mapping page records"));
        }
        Self::validate_page_framing(raws, input, false)?;
        let model = self.model().clone();
        let catalog = TypeCatalog::new(model.object_types()).with_interfaces(model.interfaces());
        let canonical = CommitModel::new(&catalog);
        self.write_tx(|| {
            self.capture_mapping_page_rows_in_tx(raws, input, canonical, MAX_PAGE_RECORDS)
        })
    }

    /// SOURCE-CAPTURE-1: validate the active fencing lease and expected
    /// previous cursor, capture every row of the complete page (split into
    /// `mapping-page:` runs of at most `max_run_rows` and `MAX_PAGE_BYTES`
    /// inside this single transaction, or an empty terminal page when `raws`
    /// is empty), record pending processing state in `_norn_source_pending`,
    /// and advance `_norn_source_cursor` atomically in one SQLite transaction.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::SourceCursorConflict`],
    /// framing refusals, invalid pins, or store failures (rolling back inbox
    /// capture, pending recovery record, and cursor advancement together).
    pub fn capture_source_page(
        &mut self,
        raws: &[Observed<RuntimeValue>],
        input: Option<&MappingInput>,
        advance: &crate::source_cursor::CursorAdvanceSpec,
        max_run_rows: usize,
    ) -> Result<CapturedSourcePage, CellError> {
        let input = if raws.is_empty() {
            None
        } else {
            let input = input.ok_or(CellError::MappingBundleMismatch)?;
            Self::validate_page_framing(raws, input, true)?;
            Some(input)
        };
        let digest =
            crate::source_cursor::page_digest(advance, input.map(|i| i.mapping.as_str()), raws)?;
        let model = self.model().clone();
        let catalog = TypeCatalog::new(model.object_types()).with_interfaces(model.interfaces());
        let canonical = CommitModel::new(&catalog);
        self.write_tx(|| {
            let precondition =
                crate::source_cursor::check_capture_precondition(&self.conn, advance, &digest)?;
            let captured = match &precondition {
                crate::source_cursor::Precondition::IdempotentReplay { pages, records } => {
                    // Exact replay returns the originally captured observation
                    // ids without creating fresh inbox rows (even if wall-clock
                    // `extracted_at` changed).
                    CapturedPage {
                        pages: pages.iter().cloned().map(|id| (id, false)).collect(),
                        records: records.clone(),
                    }
                }
                crate::source_cursor::Precondition::Advance => match input {
                    Some(input) => {
                        self.capture_mapping_page_rows_in_tx(raws, input, canonical, max_run_rows)?
                    }
                    None => CapturedPage {
                        pages: Vec::new(),
                        records: Vec::new(),
                    },
                },
            };
            let page_ids: Vec<String> = captured.pages.iter().map(|(id, _)| id.clone()).collect();
            let payload = crate::source_cursor::CaptureCommitPayload {
                digest: &digest,
                pages: &page_ids,
                records: &captured.records,
                record_pending: true,
            };
            crate::source_cursor::commit_cursor_in_tx(
                &self.conn,
                advance,
                &precondition,
                &payload,
            )?;
            Ok(CapturedSourcePage {
                page_digest: digest,
                captured,
            })
        })
    }

    /// SOURCE-CAPTURE-1: atomically verify the active fencing lease and
    /// expected previous cursor, capture one row during `OBS-LINK-DISPUTE-1`
    /// fallback, and record it in `_norn_source_pending` inside one SQLite
    /// transaction so a crash before promotion never strands the row and a
    /// lease takeover cannot interleave with capture.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::SourceCursorConflict`],
    /// framing refusals, or store errors.
    pub fn capture_source_dispute_row(
        &mut self,
        raw: &Observed<RuntimeValue>,
        input: &MappingInput,
        advance: &crate::source_cursor::CursorAdvanceSpec,
        pending_key: &str,
    ) -> Result<CapturedPage, CellError> {
        let single = std::slice::from_ref(raw);
        Self::validate_page_framing(single, input, true)?;
        let model = self.model().clone();
        let catalog = TypeCatalog::new(model.object_types()).with_interfaces(model.interfaces());
        let canonical = CommitModel::new(&catalog);
        self.write_tx(|| {
            let precondition =
                crate::source_cursor::check_capture_precondition(&self.conn, advance, pending_key)?;
            if let crate::source_cursor::Precondition::IdempotentReplay { pages, records } =
                precondition
            {
                return Ok(CapturedPage {
                    pages: pages.into_iter().map(|id| (id, false)).collect(),
                    records,
                });
            }
            let captured = self.capture_mapping_page_rows_in_tx(single, input, canonical, 1)?;
            let page_ids: Vec<String> = captured.pages.iter().map(|(id, _)| id.clone()).collect();
            crate::source_cursor::insert_pending_in_tx(
                &self.conn,
                &advance.connection_id,
                &advance.dataset,
                pending_key,
                &page_ids,
                &captured.records,
                advance.now_ms,
            )?;
            Ok(captured)
        })
    }

    /// SOURCE-CAPTURE-1: capture a sync report page while verifying inside the
    /// same SQLite transaction that any existing lease row for `connection_id`
    /// still belongs to `(lease_id, generation)` and has not expired at `now_ms`.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], framing refusals, or store errors.
    pub fn capture_source_report_checked(
        &mut self,
        raws: &[Observed<RuntimeValue>],
        input: &MappingInput,
        lease: Option<(&str, &str, u64, u64)>,
    ) -> Result<CapturedPage, CellError> {
        if raws.is_empty() {
            return Err(CellError::OutOfRange("mapping page records"));
        }
        Self::validate_page_framing(raws, input, false)?;
        let model = self.model().clone();
        let catalog = TypeCatalog::new(model.object_types()).with_interfaces(model.interfaces());
        let canonical = CommitModel::new(&catalog);
        self.write_tx(|| {
            if let Some((connection_id, lease_id, generation, now_ms)) = lease
                && self.read().source_lease(connection_id)?.is_some()
            {
                crate::source_cursor::verify_active_lease(
                    &self.conn,
                    connection_id,
                    lease_id,
                    generation,
                    now_ms,
                )?;
            }
            self.capture_mapping_page_rows_in_tx(raws, input, canonical, MAX_PAGE_RECORDS)
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
    ///
    /// # Errors
    /// Store errors.
    pub fn pending_page_member_ids(&self, page: &str) -> Result<Option<Vec<String>>, CellError> {
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
    /// [`Self::try_mapping_page`] and [`Self::finish_mapping_page`] are the
    /// same steps, one commit each, for a host that releases the cell between
    /// commits (INGEST-YIELD-1).
    ///
    /// # Errors
    /// Store failures only; refusals are outcomes.
    pub fn process_mapping_page(
        &mut self,
        page_id: &str,
    ) -> Result<Option<Vec<(String, MappingOutcome)>>, CellError> {
        match self.try_mapping_page(page_id)? {
            PageAttempt::Consumed => Ok(None),
            PageAttempt::Landed(outcomes) => Ok(Some(outcomes)),
            PageAttempt::Refused(members) => {
                // The per-record path: the same outcomes one-by-one delivery has.
                let mut outcomes = Vec::with_capacity(members.len());
                for id in members {
                    let outcome = self.process_mapping_outcome(&id)?;
                    outcomes.push((id, outcome));
                }
                self.finish_mapping_page(page_id)?;
                Ok(Some(outcomes))
            }
        }
    }

    /// The first step of [`Self::process_mapping_page`]: one commit applying
    /// every still-pending member of the page, or, when the cell refuses it,
    /// the members to process one by one (one commit each, through
    /// [`Self::process_mapping_outcome`]) before [`Self::finish_mapping_page`].
    ///
    /// # Errors
    /// Store failures only; refusals are outcomes.
    pub fn try_mapping_page(&mut self, page_id: &str) -> Result<PageAttempt, CellError> {
        let Some((observed, members)) = self.pending_page(page_id)? else {
            return Ok(PageAttempt::Consumed);
        };
        match self.commit_page(&observed, &members) {
            Ok(()) => Ok(PageAttempt::Landed(
                members
                    .into_iter()
                    .map(|(id, _)| (id, MappingOutcome::Landed))
                    .collect(),
            )),
            Err(error) if is_store_failure(&error) => Err(error),
            Err(_refused) => Ok(PageAttempt::Refused(
                members.into_iter().map(|(id, _)| id).collect(),
            )),
        }
    }

    /// The last step of a refused page ([`Self::try_mapping_page`]): the page
    /// is consumed, in a commit of its own, once its last member is; a
    /// quarantined member keeps it pending, and it applies when that member
    /// is processed again. Returns whether it committed.
    ///
    /// # Errors
    /// Store failures, or a refused consumption.
    pub fn finish_mapping_page(&mut self, page_id: &str) -> Result<bool, CellError> {
        let Some((observed, members)) = self.pending_page(page_id)? else {
            return Ok(false);
        };
        if !members.is_empty() {
            return Ok(false);
        }
        self.commit_page(&observed, &[])?;
        Ok(true)
    }

    /// A pending page row and its still-pending members; `None` when the page
    /// was consumed.
    #[allow(
        clippy::type_complexity,
        reason = "the page row with its pending members"
    )]
    fn pending_page(
        &self,
        page_id: &str,
    ) -> Result<Option<(Observed<RuntimeValue>, Vec<(String, MappingBundle)>)>, CellError> {
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
        Ok(Some((observed, members)))
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
        // INGEST-DERIVE-ONCE-1: every bundle is checked by the commit's own
        // guard (`check_mapping_bundle`, inside the transaction, against the
        // state the commit applies to). Any refusal of the page, by the guard
        // or before it, sends its members down the per-record path, where each
        // is checked again with its own outcome, so a check here first would
        // only repeat the guard's.
        for bundle in &bundles {
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

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// See [`CellStore::bind_source_mapping`].
    ///
    /// # Errors
    /// Lease fencing, out-of-range timestamp, or store errors.
    pub fn bind_source_mapping(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        self.store_mut().bind_source_mapping(
            connection_id,
            mapping_id,
            lease_id,
            generation,
            now_ms,
        )
    }

    /// See [`CellStore::reset_source_cursors`].
    ///
    /// # Errors
    /// Lease fencing, pending captures conflict, out-of-range timestamp, or store errors.
    pub fn reset_source_cursors(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        self.store_mut().reset_source_cursors(
            connection_id,
            mapping_id,
            lease_id,
            generation,
            now_ms,
        )
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Persist `connection_id -> mapping_id` in `_norn_source_binding` under
    /// the active fencing lease, clearing any orphan `processed = 1` replay
    /// rows if the previous binding was absent or different.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::OutOfRange`], or store errors.
    pub fn bind_source_mapping(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        use crate::engine::params;
        use crate::source_cursor::{SOURCE_BINDING, SOURCE_PENDING, verify_active_lease};
        let gen_i64 =
            i64::try_from(generation).map_err(|_| CellError::OutOfRange("binding generation"))?;
        let now_i64 =
            i64::try_from(now_ms).map_err(|_| CellError::OutOfRange("binding updated_at_ms"))?;
        self.write_tx(|| {
            verify_active_lease(&self.conn, connection_id, lease_id, generation, now_ms)?;
            let prev: Option<String> = self.conn.query_optional(
                &format!(
                    "SELECT \"mapping_id\" FROM \"{SOURCE_BINDING}\" WHERE \"connection_id\" = ?1"
                ),
                params![connection_id],
                |row| row.get(0),
            )?;
            if prev.as_deref() != Some(mapping_id) {
                self.conn.execute(
                    &format!(
                        "DELETE FROM \"{SOURCE_PENDING}\" \
                         WHERE \"connection_id\" = ?1 AND \"processed\" = 1"
                    ),
                    params![connection_id],
                )?;
            }
            self.conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{SOURCE_BINDING}\" \
                     (\"connection_id\", \"mapping_id\", \"generation\", \"updated_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                params![connection_id, mapping_id, gen_i64, now_i64],
            )?;
            Ok(())
        })
    }

    /// Clear durable cursors/replays and rebind `connection_id` to `mapping_id`
    /// under the active lease. Refuses if unprocessed pending captures remain.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::SourceCursorConflict`],
    /// [`CellError::OutOfRange`], or store errors.
    pub fn reset_source_cursors(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        use crate::engine::params;
        use crate::source_cursor::{
            SOURCE_BINDING, SOURCE_CURSOR, SOURCE_PENDING, verify_active_lease,
        };
        let gen_i64 =
            i64::try_from(generation).map_err(|_| CellError::OutOfRange("binding generation"))?;
        let now_i64 =
            i64::try_from(now_ms).map_err(|_| CellError::OutOfRange("binding updated_at_ms"))?;
        self.write_tx(|| {
            verify_active_lease(&self.conn, connection_id, lease_id, generation, now_ms)?;
            let pending: Option<String> = self.conn.query_optional(
                &format!("SELECT \"dataset\" FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1 AND \"processed\" = 0 LIMIT 1"),
                params![connection_id],
                |row| row.get(0),
            )?;
            if let Some(dataset) = pending {
                return Err(CellError::SourceCursorConflict {
                    connection_id: connection_id.to_owned(),
                    dataset,
                    expected: None,
                    actual: Some("pending".to_owned()),
                });
            }
            self.conn.execute(
                &format!("DELETE FROM \"{SOURCE_CURSOR}\" WHERE \"connection_id\" = ?1"),
                params![connection_id],
            )?;
            self.conn.execute(
                &format!("DELETE FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1"),
                params![connection_id],
            )?;
            self.conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{SOURCE_BINDING}\" \
                     (\"connection_id\", \"mapping_id\", \"generation\", \"updated_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                params![connection_id, mapping_id, gen_i64, now_i64],
            )?;
            Ok(())
        })
    }
}

impl<E: SqlEngine> crate::StoreReader<'_, E> {
    /// Durably bound mapping pin for `connection_id` from `_norn_source_binding`.
    ///
    /// # Errors
    /// Store errors.
    pub fn source_mapping_binding(&self, connection_id: &str) -> Result<Option<String>, CellError> {
        use crate::engine::params;
        use crate::source_cursor::SOURCE_BINDING;
        self.conn.query_optional(
            &format!(
                "SELECT \"mapping_id\" FROM \"{SOURCE_BINDING}\" WHERE \"connection_id\" = ?1"
            ),
            params![connection_id],
            |row| row.get(0),
        )
    }
}
