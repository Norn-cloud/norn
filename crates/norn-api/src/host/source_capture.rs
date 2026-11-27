//! SOURCE-CAPTURE-1: host-side durable lease acquisition and atomic
//! source-page capture over [`CellStore::capture_source_page`].

use std::collections::{BTreeMap, VecDeque};

use norn_cell::CellError;
use norn_cell::engine::SqlEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::mapping_page::{CapturedPage, PageAttempt};
use norn_cell::source_cursor::{CursorAdvanceSpec, SourceCursorRecord, SourceLease};
use norn_ir::RuntimeValue;
use norn_kernel::Observed;

use super::{DevHost, IngestReport, Ingested, Page, pages};
use crate::refusal::Refusal;

/// Typed failure returned by [`DevHost::acquire_source_lease`] and
/// [`DevHost::ingest_source_page`].
#[derive(Debug, Clone)]
pub enum SourceCaptureError {
    /// The sync lease is missing, expired, or superseded by a newer generation.
    LeaseFenced(String),
    /// The durable source cursor moved since this page was planned.
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

    /// SOURCE-CAPTURE-1: capture a page of source rows (or an empty terminal
    /// page) and atomically advance `_norn_source_cursor` in one SQLite
    /// transaction under `advance`'s fencing lease, then process each captured
    /// page row.
    ///
    /// If a large page must split due to byte bounds or `OBS-LINK-DISPUTE-1`,
    /// non-final sub-pages verify the active lease without advancing the cursor
    /// and only the final sub-page advances `_norn_source_cursor`.
    ///
    /// # Errors
    /// Lease fencing, cursor conflict, framing refusals, or store failures.
    pub fn ingest_source_page(
        &self,
        rows: &[(Observed<RuntimeValue>, MappingInput)],
        advance: &CursorAdvanceSpec,
    ) -> Result<Vec<Ingested>, SourceCaptureError> {
        let _epoch = self.definition_read()?;
        if rows.is_empty() {
            self.observing_raw(|cell| cell.capture_source_page(&[], None, advance))?
                .map_err(SourceCaptureError::from_cell)?;
            return Ok(Vec::new());
        }
        let mut framed: VecDeque<Page<'_>> = pages(rows, false).into();
        let mut out: Vec<(usize, Ingested)> = Vec::with_capacity(rows.len());
        let mut report = IngestReport {
            outcomes: Vec::new(),
            next: None,
            pages: 0,
            commits: 0,
            work_ms: 0,
        };
        while let Some((input, page)) = framed.pop_front() {
            let step_advance = if framed.is_empty() {
                advance.clone()
            } else {
                advance.without_cursor_advance()
            };
            let Some(captured) =
                self.capture_source_subpage(input, page.clone(), &mut framed, &step_advance)?
            else {
                continue;
            };
            let processed = self.process_captured_source_page(&page, captured, &mut report)?;
            out.extend(processed);
        }
        out.sort_by_key(|(at, _)| *at);
        Ok(out.into_iter().map(|(_, ingested)| ingested).collect())
    }

    fn capture_source_subpage<'a>(
        &self,
        input: &'a MappingInput,
        mut page: Vec<(usize, &'a Observed<RuntimeValue>)>,
        framed: &mut VecDeque<Page<'a>>,
        advance: &CursorAdvanceSpec,
    ) -> Result<Option<CapturedPage>, SourceCaptureError> {
        let delivered: Vec<Observed<RuntimeValue>> =
            page.iter().map(|(_, raw)| (*raw).clone()).collect();
        match self
            .observing_raw(|cell| cell.capture_source_page(&delivered, Some(input), advance))?
        {
            Ok(captured) => Ok(Some(captured)),
            Err(CellError::OutOfRange("mapping page bytes")) if page.len() > 1 => {
                let rest = page.split_off(page.len() / 2);
                framed.push_front((input, rest));
                framed.push_front((input, page));
                Ok(None)
            }
            Err(CellError::MappingPageSpansDispute) if page.len() > 1 => {
                for row in page.into_iter().rev() {
                    framed.push_front((input, vec![row]));
                }
                Ok(None)
            }
            Err(error) => Err(SourceCaptureError::from_cell(error)),
        }
    }

    fn process_captured_source_page(
        &self,
        page: &[(usize, &Observed<RuntimeValue>)],
        captured: CapturedPage,
        report: &mut IngestReport,
    ) -> Result<Vec<(usize, Ingested)>, Refusal> {
        let CapturedPage {
            pages: captured_pages,
            records,
        } = captured;
        let mut outcomes = BTreeMap::new();
        for (page_id, _new) in &captured_pages {
            let attempt = self.observing(|cell| cell.try_mapping_page(page_id))?;
            let members = match attempt {
                PageAttempt::Consumed => continue,
                PageAttempt::Landed(landed) => {
                    outcomes.extend(landed);
                    Vec::new()
                }
                PageAttempt::Refused(members) => members,
            };
            if members.is_empty() {
                continue;
            }
            for member in &members {
                let outcome = self.observing(|cell| cell.process_mapping_outcome(member))?;
                outcomes.insert(member.clone(), outcome);
            }
            let _ = self.observing(|cell| cell.finish_mapping_page(page_id))?;
        }
        let rows = page.iter().map(|(at, _)| *at).zip(records);
        self.answer_page(rows, outcomes, report, 0)
    }
}
