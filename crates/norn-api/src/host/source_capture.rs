//! SOURCE-CAPTURE-1: host-side durable lease acquisition, atomic complete-page
//! source capture over [`CellStore::capture_source_page`], and post-capture
//! inbox processing / crash recovery.

use std::collections::BTreeMap;

use norn_cell::engine::SqlEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::mapping_page::PageAttempt;
use norn_cell::source_cursor::{
    CursorAdvanceSpec, PendingSourceCapture, SourceCursorRecord, SourceLease,
};
use norn_cell::{CellError, MappingOutcome};
use norn_ir::RuntimeValue;
use norn_kernel::Observed;

use super::{DevHost, IngestReport, Ingested, PAGE_ROWS};
use crate::refusal::Refusal;

/// Typed failure returned by [`DevHost::acquire_source_lease`],
/// [`DevHost::seed_source_cursors`], [`DevHost::capture_source_page_atomic`],
/// and [`DevHost::ingest_source_page`].
#[derive(Debug, Clone)]
pub enum SourceCaptureError {
    /// The sync lease is missing, expired, or superseded by a newer generation.
    LeaseFenced(String),
    /// The durable source cursor moved since this page was planned, or a
    /// duplicate delivery's content digest did not match the committed page.
    CursorConflict(String),
    /// Any other framing, mapping, or store failure.
    Refusal(Refusal),
}

impl SourceCaptureError {
    /// Stable source-sync error code and message pair.
    #[must_use]
    pub fn code_and_message(self) -> (&'static str, String) {
        match self {
            Self::LeaseFenced(message) => ("source.lease_fenced", message),
            Self::CursorConflict(message) => ("source.cursor_conflict", message),
            Self::Refusal(refusal) => ("source.ingest", refusal.message),
        }
    }

    fn from_cell(error: CellError) -> Self {
        match error {
            CellError::SourceLeaseFenced { .. } => Self::LeaseFenced(error.to_string()),
            CellError::SourceCursorConflict { .. } => Self::CursorConflict(error.to_string()),
            other => Self::Refusal(Refusal::host(other)),
        }
    }
}

impl From<Refusal> for SourceCaptureError {
    fn from(refusal: Refusal) -> Self {
        Self::Refusal(refusal)
    }
}

impl IngestReport {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            outcomes: Vec::with_capacity(capacity),
            next: None,
            pages: 0,
            commits: 0,
            work_ms: 0,
        }
    }
}

impl<E: SqlEngine> DevHost<E> {
    /// Acquire or take over an expired durable sync lease for `connection_id`.
    ///
    /// # Errors
    /// [`SourceCaptureError::LeaseFenced`] if another active lease owns
    /// `connection_id` at `now_ms`, or [`SourceCaptureError::Refusal`] on host
    /// or store failure.
    pub fn acquire_source_lease(
        &self,
        connection_id: &str,
        lease_id: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<SourceLease, SourceCaptureError> {
        let _epoch = self.definition_read()?;
        self.observing_raw(|cell| {
            cell.acquire_source_lease(connection_id, lease_id, now_ms, ttl_ms)
        })?
        .map_err(SourceCaptureError::from_cell)
    }

    /// Release a durable sync lease if `(lease_id, generation)` still owns it.
    ///
    /// # Errors
    /// Host or store failure.
    pub fn release_source_lease(
        &self,
        connection_id: &str,
        lease_id: &str,
        generation: u64,
    ) -> Result<bool, Refusal> {
        let _epoch = self.definition_read()?;
        self.observing(|cell| cell.release_source_lease(connection_id, lease_id, generation))
    }

    /// Seed legacy dataset cursors (upgraded from `source.Connection.watermark`)
    /// into `_norn_source_cursor` under the active fencing lease using
    /// `INSERT OR IGNORE`.
    ///
    /// # Errors
    /// [`SourceCaptureError::LeaseFenced`] or store failure.
    pub fn seed_source_cursors(
        &self,
        connection_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
        cursors: &BTreeMap<String, String>,
    ) -> Result<(), SourceCaptureError> {
        let _epoch = self.definition_read()?;
        self.observing_raw(|cell| {
            cell.seed_source_cursors(connection_id, lease_id, generation, now_ms, cursors)
        })?
        .map_err(SourceCaptureError::from_cell)
    }

    /// Read all durably committed dataset cursors for `connection_id`.
    ///
    /// # Errors
    /// Host or store failure.
    pub fn source_cursors(
        &self,
        connection_id: &str,
    ) -> Result<BTreeMap<String, SourceCursorRecord>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .source_cursors(connection_id)
            .map_err(Refusal::host)
    }

    /// Read the durable lease row for `connection_id`, if any.
    ///
    /// # Errors
    /// Host or store failure.
    pub fn source_lease(&self, connection_id: &str) -> Result<Option<SourceLease>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .source_lease(connection_id)
            .map_err(Refusal::host)
    }

    /// Read all durably captured source pages for `connection_id` whose
    /// post-capture canonical processing has not yet completed.
    ///
    /// # Errors
    /// Host or store failure.
    pub fn pending_source_captures(
        &self,
        connection_id: &str,
    ) -> Result<Vec<PendingSourceCapture>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .pending_source_captures(connection_id)
            .map_err(Refusal::host)
    }

    /// Remove or mark processed a pending source capture entry after every
    /// captured inbox row has been processed cleanly.
    ///
    /// # Errors
    /// Host or store failure.
    pub fn complete_source_pending(
        &self,
        connection_id: &str,
        dataset: &str,
        page_digest: &str,
    ) -> Result<(), Refusal> {
        let _epoch = self.definition_read()?;
        self.observing(|cell| cell.complete_source_pending(connection_id, dataset, page_digest))
    }

    /// SOURCE-CAPTURE-1: ingest sync report observations (`source.Connection` /
    /// `source.SyncRun`), checking inside each SQLite capture transaction that
    /// any existing durable lease for `connection_id` still belongs to
    /// `(lease_id, generation)` and has not expired at `now_ms`.
    ///
    /// # Errors
    /// [`SourceCaptureError::LeaseFenced`] or store failure.
    pub fn ingest_source_report_checked(
        &self,
        rows: &[(Observed<RuntimeValue>, MappingInput)],
        lease: Option<(&str, &str, u64, u64)>,
    ) -> Result<(), SourceCaptureError> {
        let _epoch = self.definition_read()?;
        for (raw, input) in rows {
            let captured = self
                .observing_raw(|cell| {
                    cell.capture_source_report_checked(std::slice::from_ref(raw), input, lease)
                })?
                .map_err(SourceCaptureError::from_cell)?;
            let mut outcomes = BTreeMap::new();
            for (page_id, _) in &captured.pages {
                let _consumed = self.process_one_mapping_page(page_id, &mut outcomes)?;
            }
            for record_id in captured.records {
                if !outcomes.contains_key(&record_id) {
                    let _outcome =
                        self.observing(|cell| cell.process_mapping_outcome(&record_id))?;
                }
            }
        }
        Ok(())
    }

    /// SOURCE-CAPTURE-1: capture a complete source page (or empty terminal page)
    /// and atomically advance `_norn_source_cursor` + `_norn_source_pending` in
    /// one SQLite transaction under `advance`'s active fencing lease.
    ///
    /// If the page spans a new `OBS-LINK-DISPUTE-1` conflict or identity finding
    /// after a normal row, falls back to atomic per-row lease-checked capture
    /// (persisting each row into `_norn_source_pending` inside its capture tx
    /// before promotion) and commits `_norn_source_cursor` only once every row
    /// has been durably captured and promoted.
    ///
    /// # Errors
    /// Lease fencing, cursor conflict, framing refusals, or store failures.
    pub fn capture_source_page_atomic(
        &self,
        rows: &[(Observed<RuntimeValue>, MappingInput)],
        advance: &CursorAdvanceSpec,
        max_run_rows: usize,
    ) -> Result<(PendingSourceCapture, BTreeMap<String, MappingOutcome>), SourceCaptureError> {
        let _epoch = self.definition_read()?;
        let Some((_, input)) = rows.first() else {
            let captured = self
                .observing_raw(|cell| cell.capture_source_page(&[], None, advance, max_run_rows))?
                .map_err(SourceCaptureError::from_cell)?;
            return Ok((
                PendingSourceCapture {
                    connection_id: advance.connection_id.clone(),
                    dataset: advance.dataset.clone(),
                    page_digest: captured.page_digest,
                    pages: Vec::new(),
                    records: Vec::new(),
                    captured_at_ms: advance.now_ms,
                },
                BTreeMap::new(),
            ));
        };
        if rows.iter().any(|(_, row_input)| row_input != input) {
            return Err(SourceCaptureError::from_cell(
                CellError::MappingBundleMismatch,
            ));
        }
        let delivered: Vec<Observed<RuntimeValue>> =
            rows.iter().map(|(raw, _)| raw.clone()).collect();
        match self.observing_raw(|cell| {
            cell.capture_source_page(&delivered, Some(input), advance, max_run_rows)
        })? {
            Ok(captured) => Ok((
                PendingSourceCapture {
                    connection_id: advance.connection_id.clone(),
                    dataset: advance.dataset.clone(),
                    page_digest: captured.page_digest,
                    pages: captured
                        .captured
                        .pages
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect(),
                    records: captured.captured.records,
                    captured_at_ms: advance.now_ms,
                },
                BTreeMap::new(),
            )),
            Err(CellError::MappingPageSpansDispute) if delivered.len() > 1 => {
                self.capture_source_page_dispute_fallback(&delivered, input, advance)
            }
            Err(error) => Err(SourceCaptureError::from_cell(error)),
        }
    }

    fn capture_source_page_dispute_fallback(
        &self,
        delivered: &[Observed<RuntimeValue>],
        input: &MappingInput,
        advance: &CursorAdvanceSpec,
    ) -> Result<(PendingSourceCapture, BTreeMap<String, MappingOutcome>), SourceCaptureError> {
        let full_digest = norn_cell::source_cursor::page_digest(advance, delivered)
            .map_err(SourceCaptureError::from_cell)?;
        let mut all_pages = Vec::new();
        let mut all_records = Vec::with_capacity(delivered.len());
        let mut outcomes = BTreeMap::new();
        for (index, raw) in delivered.iter().enumerate() {
            let pending_key = format!("{full_digest}:row:{index}");
            // Atomically verify lease + expected cursor, capture this row, and
            // record it in `_norn_source_pending` in one SQLite write_tx.
            let captured = self
                .observing_raw(|cell| {
                    cell.capture_source_dispute_row(raw, input, advance, &pending_key)
                })?
                .map_err(SourceCaptureError::from_cell)?;
            for (page_id, _) in &captured.pages {
                let _consumed = self.process_one_mapping_page(page_id, &mut outcomes)?;
                all_pages.push(page_id.clone());
            }
            for record_id in captured.records {
                if !outcomes.contains_key(&record_id) {
                    let outcome =
                        self.observing(|cell| cell.process_mapping_outcome(&record_id))?;
                    outcomes.insert(record_id.clone(), outcome);
                }
                all_records.push(record_id);
            }
            self.complete_source_pending(&advance.connection_id, &advance.dataset, &pending_key)?;
        }
        self.observing_raw(|cell| {
            cell.commit_source_page_cursor(advance, &full_digest, &all_pages, &all_records)
        })?
        .map_err(SourceCaptureError::from_cell)?;
        Ok((
            PendingSourceCapture {
                connection_id: advance.connection_id.clone(),
                dataset: advance.dataset.clone(),
                page_digest: full_digest,
                pages: Vec::new(),
                records: all_records,
                captured_at_ms: advance.now_ms,
            },
            outcomes,
        ))
    }

    /// Process at most `max_rows` rows of an already-captured
    /// [`PendingSourceCapture`], strictly honoring `SYNC-YIELD-2` pacing even
    /// when a previously captured `mapping-page:` run is larger than `max_rows`
    /// or contains multiple quarantined members.
    ///
    /// Fails closed if any expected captured observation id is absent from this
    /// cell's `_norn_inbox`.
    ///
    /// # Errors
    /// Store or missing-inbox failures; mapping/commit refusals are returned as
    /// [`MappingOutcome::Quarantined`].
    pub fn process_source_page_step(
        &self,
        pending: &mut PendingSourceCapture,
        outcomes: &mut BTreeMap<String, MappingOutcome>,
        max_rows: usize,
    ) -> Result<Vec<Ingested>, Refusal> {
        let _epoch = self.definition_read()?;
        let limit = max_rows.max(1);
        let allow_consumed_drain =
            self.advance_pending_pages(pending, outcomes, max_rows, limit)?;
        let mut processed = Vec::new();
        while processed.len() < limit {
            let Some(id) = pending.records.first() else {
                break;
            };
            if let Some(outcome) = outcomes.remove(id) {
                let id = pending.records.remove(0);
                processed.push(Ingested {
                    observation_id: id,
                    outcome,
                });
                continue;
            }
            if !outcomes.is_empty() || pending.pages.is_empty() || allow_consumed_drain {
                let id = pending.records.remove(0);
                if !self.inbox_observation_exists(&id)? {
                    return Err(Refusal::host(CellError::MappingBundleMismatch));
                }
                let outcome = self.observing(|cell| cell.process_mapping_outcome(&id))?;
                processed.push(Ingested {
                    observation_id: id,
                    outcome,
                });
                continue;
            }
            break;
        }
        Ok(processed)
    }

    fn advance_pending_pages(
        &self,
        pending: &mut PendingSourceCapture,
        outcomes: &mut BTreeMap<String, MappingOutcome>,
        max_rows: usize,
        limit: usize,
    ) -> Result<bool, Refusal> {
        let mut allow_consumed_drain = false;
        while let Some(page_id) = pending.pages.first().cloned() {
            if !self.inbox_observation_exists(&page_id)? {
                return Err(Refusal::host(CellError::MappingBundleMismatch));
            }
            let Some(ids) = self.pending_page_member_ids(&page_id)? else {
                pending.pages.remove(0);
                allow_consumed_drain = true;
                if max_rows != usize::MAX {
                    break;
                }
                continue;
            };
            // Filter to members still queued in `pending.records` and not yet
            // processed in `outcomes`, so quarantined members (which remain
            // pending in `_norn_inbox`) are never re-selected across paced steps.
            let remaining: Vec<String> = ids
                .iter()
                .filter(|id| {
                    pending.records.iter().any(|rec| rec == *id) && !outcomes.contains_key(*id)
                })
                .cloned()
                .collect();
            if remaining.is_empty() {
                let _finished = self.observing(|cell| cell.finish_mapping_page(&page_id))?;
                pending.pages.remove(0);
                allow_consumed_drain = true;
            } else if remaining.len() == ids.len() && ids.len() <= limit {
                pending.pages.remove(0);
                if self.process_one_mapping_page(&page_id, outcomes)? {
                    allow_consumed_drain = true;
                }
            } else {
                for member in remaining.iter().take(limit) {
                    let outcome = self.observing(|cell| cell.process_mapping_outcome(member))?;
                    outcomes.insert(member.clone(), outcome);
                }
                if remaining.len() <= limit {
                    let _finished = self.observing(|cell| cell.finish_mapping_page(&page_id))?;
                    pending.pages.remove(0);
                }
            }
            if max_rows != usize::MAX {
                break;
            }
        }
        Ok(allow_consumed_drain)
    }

    /// SOURCE-CAPTURE-1: capture a complete source page (or empty terminal page)
    /// atomically with `_norn_source_cursor`, process all captured rows into
    /// canonical tables, and clear `_norn_source_pending` once complete.
    ///
    /// # Errors
    /// Lease fencing, cursor conflict, framing refusals, or store failures.
    pub fn ingest_source_page(
        &self,
        rows: &[(Observed<RuntimeValue>, MappingInput)],
        advance: &CursorAdvanceSpec,
    ) -> Result<Vec<Ingested>, SourceCaptureError> {
        let (mut pending, mut outcomes) =
            self.capture_source_page_atomic(rows, advance, PAGE_ROWS)?;
        if pending.pages.is_empty() && pending.records.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(pending.records.len());
        while !pending.pages.is_empty() || !pending.records.is_empty() {
            out.extend(self.process_source_page_step(&mut pending, &mut outcomes, usize::MAX)?);
        }
        self.complete_source_pending(
            &pending.connection_id,
            &pending.dataset,
            &pending.page_digest,
        )?;
        Ok(out)
    }

    /// Recover and process every stranded post-capture pending source page for
    /// `connection_id` left behind by an earlier crash or store error.
    ///
    /// # Errors
    /// Store failures; mapping/commit refusals are returned as
    /// [`MappingOutcome::Quarantined`].
    pub fn recover_pending_source_pages(
        &self,
        connection_id: &str,
    ) -> Result<Vec<Ingested>, Refusal> {
        let mut recovered = Vec::new();
        for mut pending in self.pending_source_captures(connection_id)? {
            let mut outcomes = BTreeMap::new();
            while !pending.pages.is_empty() || !pending.records.is_empty() {
                recovered.extend(self.process_source_page_step(
                    &mut pending,
                    &mut outcomes,
                    usize::MAX,
                )?);
            }
            self.complete_source_pending(
                &pending.connection_id,
                &pending.dataset,
                &pending.page_digest,
            )?;
        }
        Ok(recovered)
    }

    fn inbox_observation_exists(&self, observation_id: &str) -> Result<bool, Refusal> {
        let cell = self.cell()?;
        cell.store()
            .read()
            .inbox_observation_exists(observation_id)
            .map_err(Refusal::host)
    }

    fn pending_page_member_ids(&self, page_id: &str) -> Result<Option<Vec<String>>, Refusal> {
        let cell = self.cell()?;
        cell.store()
            .read()
            .pending_page_member_ids(page_id)
            .map_err(Refusal::host)
    }

    fn process_one_mapping_page(
        &self,
        page_id: &str,
        outcomes: &mut BTreeMap<String, MappingOutcome>,
    ) -> Result<bool, Refusal> {
        let attempt = self.observing(|cell| cell.try_mapping_page(page_id))?;
        let members = match attempt {
            PageAttempt::Consumed => return Ok(true),
            PageAttempt::Landed(landed) => {
                outcomes.extend(landed);
                return Ok(false);
            }
            PageAttempt::Refused(members) => members,
        };
        for member in &members {
            let outcome = self.observing(|cell| cell.process_mapping_outcome(member))?;
            outcomes.insert(member.clone(), outcome);
        }
        let _finished = self.observing(|cell| cell.finish_mapping_page(page_id))?;
        Ok(false)
    }
}
