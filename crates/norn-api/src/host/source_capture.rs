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

    /// Remove a pending source capture entry after every captured inbox row has
    /// been processed cleanly.
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

    /// SOURCE-CAPTURE-1: capture a complete source page (or empty terminal page)
    /// and atomically advance `_norn_source_cursor` + `_norn_source_pending` in
    /// one SQLite transaction under `advance`'s active fencing lease.
    ///
    /// `max_run_rows` caps each internal `mapping-page:` run inside that single
    /// transaction so post-capture processing can yield at `SYNC-YIELD-2` pace
    /// without ever committing a partial capture prefix.
    ///
    /// # Errors
    /// Lease fencing, cursor conflict, framing refusals, or store failures.
    pub fn capture_source_page_atomic(
        &self,
        rows: &[(Observed<RuntimeValue>, MappingInput)],
        advance: &CursorAdvanceSpec,
        max_run_rows: usize,
    ) -> Result<PendingSourceCapture, SourceCaptureError> {
        let _epoch = self.definition_read()?;
        let Some((_, input)) = rows.first() else {
            let captured = self
                .observing_raw(|cell| cell.capture_source_page(&[], None, advance, max_run_rows))?
                .map_err(SourceCaptureError::from_cell)?;
            return Ok(PendingSourceCapture {
                connection_id: advance.connection_id.clone(),
                dataset: advance.dataset.clone(),
                page_digest: captured.page_digest,
                pages: Vec::new(),
                records: Vec::new(),
                captured_at_ms: advance.now_ms,
            });
        };
        if rows.iter().any(|(_, row_input)| row_input != input) {
            return Err(SourceCaptureError::from_cell(
                CellError::MappingBundleMismatch,
            ));
        }
        let delivered: Vec<Observed<RuntimeValue>> =
            rows.iter().map(|(raw, _)| raw.clone()).collect();
        let captured = self
            .observing_raw(|cell| {
                cell.capture_source_page(&delivered, Some(input), advance, max_run_rows)
            })?
            .map_err(SourceCaptureError::from_cell)?;
        Ok(PendingSourceCapture {
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
        })
    }

    /// Process up to one `mapping-page:` run (or `max_rows` standalone /
    /// already-consumed records) of an already-captured [`PendingSourceCapture`].
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
        let mut allow_consumed_drain = false;
        while !pending.pages.is_empty() {
            let page_id = pending.pages.remove(0);
            if !self.inbox_observation_exists(&page_id)? {
                return Err(Refusal::host(CellError::MappingBundleMismatch));
            }
            let consumed = self.process_one_mapping_page(&page_id, outcomes)?;
            if consumed {
                allow_consumed_drain = true;
            }
            if max_rows != usize::MAX {
                break;
            }
        }
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
        let mut pending = self.capture_source_page_atomic(rows, advance, PAGE_ROWS)?;
        if pending.pages.is_empty() && pending.records.is_empty() {
            return Ok(Vec::new());
        }
        let mut outcomes = BTreeMap::new();
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
        let _ = self.observing(|cell| cell.finish_mapping_page(page_id))?;
        Ok(false)
    }
}
