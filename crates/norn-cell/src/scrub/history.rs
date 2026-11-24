//! Resumable, row-budget-bounded Phase 2 (`Phase::History`) stepper (S10).
//! Every wake enforces a shared `WAKE_ROW_BUDGET` across retained definitions,
//! receipts, ledger events, fact snapshots, export journal records, and quantity
//! chunk staging so total SQL rows per wake strictly stay `<= 512`.

use std::collections::VecDeque;

use norn_ir::RuntimeValue as Value;

use super::{VerifiedPins, defect};
use crate::codec::hash_json;
use crate::engine::SqlEngine;
use crate::open_checkpoint::verify::DefinitionWalk;
use crate::open_checkpoint::{self, OpenCheckpoint};
use crate::quantity::FormatBoundary;
use crate::read::{LedgerEntry, StoreReader};
use crate::store::InstalledDefinitionSet;
use crate::system::GENESIS_HASH;
use crate::{CellError, CellStore, LedgerHead, Receipt};

/// Shared per-wake row budget for `Phase::History`, leaving generous headroom
/// below the 512-row total per-wake bound for fence/meta overhead.
pub(super) const WAKE_ROW_BUDGET: usize = 256;

/// Worst-case SQL rows read by one `ledger_range` entry (1 ledger row + 1
/// payload blob lookup + up to 4 durable fact-event verification rows).
const EVENT_ROW_WEIGHT: usize = 6;

/// Reserve before starting a new commit or definition-install verification.
const COMMIT_START_RESERVE: usize = 8;
const DEFINITION_INSTALL_RESERVE: usize = 16;
const RECEIPT_PAGE_CAP: usize = 16;

struct RowBudget {
    remaining: usize,
}

impl RowBudget {
    const fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    const fn remaining(&self) -> usize {
        self.remaining
    }

    const fn exhausted(&self) -> bool {
        self.remaining == 0
    }

    fn charge(&mut self, rows: usize) {
        self.remaining = self.remaining.saturating_sub(rows);
    }
}

#[derive(Clone)]
struct ActiveCommit {
    receipt: Receipt,
    next_event_seq: u64,
    last_event_seq: u64,
    end_event_seq: u64,
    prev_hash: String,
    boundary: Option<FormatBoundary>,
    written_index: usize,
    definition_checked: bool,
}

/// Resumable history-scrub cursor carried across `Phase::History` wakes (S10).
#[derive(Clone)]
pub(super) struct HistoryCursor {
    versions: Vec<InstalledDefinitionSet>,
    versions_loaded: bool,
    versions_after: u64,
    anchor_checked: bool,
    pub(super) ledger: LedgerHead,
    boundary: Option<FormatBoundary>,
    time_floor: u64,
    walk: DefinitionWalk,
    buffered: VecDeque<Receipt>,
    active: Option<ActiveCommit>,
    export_checked_through: u64,
}

pub(super) enum HistoryStep {
    Continue,
    Done(VerifiedPins),
}

impl HistoryCursor {
    pub(super) fn new(start: Option<&OpenCheckpoint>) -> Self {
        let ledger = start.map_or_else(LedgerHead::genesis, |cp| cp.head.clone());
        let export_checked_through = ledger.commits;
        Self {
            versions: Vec::new(),
            versions_loaded: false,
            versions_after: 0,
            anchor_checked: false,
            ledger,
            boundary: start.and_then(|cp| cp.boundary),
            time_floor: 0,
            walk: DefinitionWalk::default(),
            buffered: VecDeque::new(),
            active: None,
            export_checked_through,
        }
    }

    /// Advance `Phase::History` by at most `WAKE_ROW_BUDGET` rows across all
    /// sub-reads, saving resume cursors whenever the wake budget is exhausted.
    pub(super) fn step<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        reader: &StoreReader<'_, E>,
        start: Option<&OpenCheckpoint>,
        target: &LedgerHead,
        target_boundary: Option<FormatBoundary>,
        quantity_scrub: &mut crate::quantity::QuantityScrub,
    ) -> Result<HistoryStep, CellError> {
        let mut budget = RowBudget::new(WAKE_ROW_BUDGET);
        if !self.versions_loaded && !self.step_load_versions(reader, &mut budget)? {
            return Ok(HistoryStep::Continue);
        }
        if !self.anchor_checked && !self.step_verify_anchor(store, reader, start, &mut budget)? {
            return Ok(HistoryStep::Continue);
        }
        let completed_after = self.ledger.commits;
        let mut completed_receipts = Vec::new();
        self.step_commits(
            store,
            reader,
            target.commits,
            &mut budget,
            &mut completed_receipts,
        )?;
        if !completed_receipts.is_empty() {
            quantity_scrub.verify_chunk(
                &store.conn,
                &completed_receipts,
                completed_after,
                self.ledger.commits,
            )?;
        }
        if self.export_checked_through < self.ledger.commits && budget.remaining() >= 3 {
            let max_records = budget.remaining().saturating_sub(1) / 2;
            let (next_through, checked) = crate::cell_identity::verify_records_step(
                &store.conn,
                self.export_checked_through,
                self.ledger.commits,
                max_records,
            )?;
            self.export_checked_through = next_through;
            budget.charge(checked.saturating_mul(2).saturating_add(1));
        }
        if self.ledger.commits == target.commits
            && self.active.is_none()
            && self.export_checked_through == target.commits
            && budget.remaining() >= 2
        {
            return self
                .finish(store, target, target_boundary)
                .map(HistoryStep::Done);
        }
        Ok(HistoryStep::Continue)
    }

    fn step_load_versions<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        // Each version reads 1 row plus up to 3 companion lookups (+2 schema probes).
        let cap = (budget.remaining() / 5).max(1);
        let page = reader.installed_definition_versions_after(self.versions_after, cap)?;
        let read = page.len();
        budget.charge(read.saturating_mul(4).saturating_add(2));
        if let Some(last) = page.last() {
            self.versions_after = last.version;
        }
        self.versions.extend(page);
        if read < cap {
            self.versions_loaded = true;
            return Ok(true);
        }
        Ok(false)
    }

    fn step_verify_anchor<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        reader: &StoreReader<'_, E>,
        start: Option<&OpenCheckpoint>,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        if budget.remaining() < 20 {
            return Ok(false);
        }
        let base = self.ledger.seq;
        let anchor_corrupt = || CellError::LedgerCorrupt {
            seq: base,
            reason: "the verified checkpoint no longer anchors the ledger".to_owned(),
        };
        if base == 0 {
            if self.ledger.hash != GENESIS_HASH {
                return Err(anchor_corrupt());
            }
        } else {
            let anchor = reader.ledger_range(base, Some(1))?;
            budget.charge(EVENT_ROW_WEIGHT);
            if !anchor
                .first()
                .is_some_and(|entry| entry.seq == base && entry.hash == self.ledger.hash)
            {
                return Err(anchor_corrupt());
            }
        }
        if self.ledger.commits > 0 {
            let anchor = reader.receipts_after(self.ledger.commits.saturating_sub(1), 1)?;
            budget.charge(1);
            self.time_floor = anchor
                .first()
                .map_or(0, |receipt| receipt.body.logical_time);
            if !anchor.first().is_some_and(|receipt| {
                receipt.body.commit_seq == self.ledger.commits
                    && receipt.body.ledger_head == self.ledger.hash
                    && receipt
                        .body
                        .first_event_seq
                        .checked_add(receipt.body.event_count)
                        == base.checked_add(1)
                    && hash_json(&receipt.body).is_ok_and(|hash| hash == receipt.hash)
            }) {
                return Err(anchor_corrupt());
            }
        } else if base != 0 {
            return Err(anchor_corrupt());
        }
        if let Some(start_cp) = start {
            crate::cell_identity::verify_export_pin(&store.conn, start_cp)?;
            self.walk = store.definitions_at_checkpoint(reader, &self.versions, start_cp)?;
            budget.charge(12);
        }
        self.anchor_checked = true;
        Ok(true)
    }

    fn step_commits<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        reader: &StoreReader<'_, E>,
        target_commits: u64,
        budget: &mut RowBudget,
        completed: &mut Vec<Receipt>,
    ) -> Result<(), CellError> {
        while !budget.exhausted() && (self.active.is_some() || self.ledger.commits < target_commits)
        {
            if self.active.is_none() {
                if budget.remaining() < COMMIT_START_RESERVE {
                    break;
                }
                let receipt = self.next_receipt(reader, target_commits, budget)?;
                self.active = Some(self.begin_commit(receipt)?);
            }
            let mut active = self.active.take().ok_or_else(|| {
                defect(self.ledger.commits, "missing active history scrub commit")
            })?;
            if !step_commit_events(reader, &mut active, self.ledger.seq == 0, budget)? {
                self.active = Some(active);
                break;
            }
            if !step_commit_written(reader, &mut active, budget)? {
                self.active = Some(active);
                break;
            }
            if !self.step_commit_definition(store, reader, &mut active, budget)? {
                self.active = Some(active);
                break;
            }
            self.ledger = LedgerHead {
                seq: active.last_event_seq,
                hash: active.prev_hash,
                commits: active.receipt.body.commit_seq,
            };
            self.boundary = active.boundary;
            self.time_floor = active.receipt.body.logical_time;
            let has_quantity = active.receipt.body.quantity.is_some();
            completed.push(active.receipt);
            if has_quantity && completed.len() >= 32 {
                break;
            }
        }
        Ok(())
    }

    fn next_receipt<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        target_commits: u64,
        budget: &mut RowBudget,
    ) -> Result<Receipt, CellError> {
        if let Some(receipt) = self.buffered.pop_front() {
            return Ok(receipt);
        }
        let left = target_commits.saturating_sub(self.ledger.commits);
        let limit = usize::try_from(left)
            .unwrap_or(usize::MAX)
            .min((budget.remaining() / 8).clamp(1, RECEIPT_PAGE_CAP));
        let page = reader.receipts_page_from(self.ledger.commits, limit)?;
        budget.charge(page.len().max(1));
        if page.is_empty() {
            return Err(defect(
                self.ledger.commits,
                "the ledger ends before the head the open verified",
            ));
        }
        self.buffered.extend(page);
        self.buffered.pop_front().ok_or_else(|| {
            defect(
                self.ledger.commits,
                "the ledger ends before the head the open verified",
            )
        })
    }

    fn begin_commit(&self, receipt: Receipt) -> Result<ActiveCommit, CellError> {
        let expected_commit = next_seq(self.ledger.commits, "commit_seq")?;
        let commit_seq = receipt.body.commit_seq;
        let corrupt = |reason: &str| CellError::LedgerCorrupt {
            seq: commit_seq,
            reason: reason.to_owned(),
        };
        if commit_seq != expected_commit {
            return Err(corrupt("commit sequence gap"));
        }
        if receipt.body.logical_time < self.time_floor {
            return Err(corrupt("receipt logical time goes back"));
        }
        validate_quantity_receipt_header(&receipt)?;
        let expected_first = next_seq(self.ledger.seq, "seq")?;
        if receipt.body.first_event_seq != expected_first {
            return Err(corrupt("receipt span does not continue the ledger"));
        }
        let end_event_seq = receipt
            .body
            .first_event_seq
            .checked_add(receipt.body.event_count)
            .ok_or_else(|| corrupt("event span out of range"))?;
        Ok(ActiveCommit {
            next_event_seq: receipt.body.first_event_seq,
            last_event_seq: self.ledger.seq,
            end_event_seq,
            prev_hash: self.ledger.hash.clone(),
            boundary: self.boundary,
            written_index: 0,
            definition_checked: false,
            receipt,
        })
    }

    fn step_commit_definition<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        reader: &StoreReader<'_, E>,
        active: &mut ActiveCommit,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        if active.definition_checked {
            return Ok(true);
        }
        let is_install = self
            .versions
            .get(self.walk.version_index)
            .is_some_and(|version| version.commit_seq == active.receipt.body.commit_seq);
        if is_install && budget.remaining() < DEFINITION_INSTALL_RESERVE {
            return Ok(false);
        }
        store.definition_walk_step(reader, &self.versions, &mut self.walk, &active.receipt)?;
        if is_install {
            budget.charge(DEFINITION_INSTALL_RESERVE);
        }
        active.definition_checked = true;
        Ok(true)
    }

    fn finish<E: SqlEngine>(
        &self,
        store: &CellStore<E>,
        target: &LedgerHead,
        target_boundary: Option<FormatBoundary>,
    ) -> Result<VerifiedPins, CellError> {
        let through = target.commits;
        if &self.ledger != target || self.boundary != target_boundary {
            return Err(defect(
                through,
                "the verified history does not end at the head the open verified",
            ));
        }
        let installed = self
            .versions
            .iter()
            .filter(|version| version.commit_seq <= through)
            .count();
        if self.walk.version_index != installed {
            return Err(defect(
                through,
                "the definition history and the receipts disagree",
            ));
        }
        Ok(VerifiedPins {
            export: crate::cell_identity::last_record_through(&store.conn, through)?,
            definitions: open_checkpoint::definitions_digest(&self.versions, through)?,
        })
    }
}

fn next_seq(value: u64, what: &'static str) -> Result<u64, CellError> {
    value.checked_add(1).ok_or(CellError::OutOfRange(what))
}

fn validate_quantity_receipt_header(receipt: &Receipt) -> Result<(), CellError> {
    let body = &receipt.body;
    if body.quantity.is_some() != body.quantity_result.is_some()
        || (body.quantity_intent.is_some() && body.quantity.is_none())
        || body.quantity.as_ref().is_some_and(|requests| {
            requests.is_empty()
                || requests.len() > 100
                || body
                    .quantity_result
                    .as_ref()
                    .is_none_or(|results| results.len() != requests.len())
        })
    {
        return Err(CellError::LedgerCorrupt {
            seq: body.commit_seq,
            reason: "quantity receipt provenance is incomplete".to_owned(),
        });
    }
    if let Some(requests) = &body.quantity
        && body.event_count
            < u64::try_from(requests.len())
                .map_err(|_range| norn_kernel::quantity::QuantityError::Corrupt)?
    {
        return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
    }
    Ok(())
}

fn step_commit_events<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    active: &mut ActiveCommit,
    from_genesis: bool,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    let commit_seq = active.receipt.body.commit_seq;
    let corrupt = |reason: &str| CellError::LedgerCorrupt {
        seq: commit_seq,
        reason: reason.to_owned(),
    };
    while active.next_event_seq < active.end_event_seq {
        let cap = budget.remaining() / EVENT_ROW_WEIGHT;
        if cap == 0 {
            return Ok(false);
        }
        let want = usize::try_from(active.end_event_seq.saturating_sub(active.next_event_seq))
            .unwrap_or(usize::MAX)
            .min(cap);
        let from = if from_genesis && active.last_event_seq == 0 {
            0
        } else {
            active.next_event_seq
        };
        let entries = reader.ledger_range(from, Some(want))?;
        let mut read = 0_usize;
        for entry in entries {
            read = read.saturating_add(1);
            let cost = if crate::history::durable_event_is_proven(&entry.event) {
                EVENT_ROW_WEIGHT
            } else if entry.event.get("type").and_then(Value::as_str) == Some("command.result") {
                2
            } else {
                1
            };
            budget.charge(cost);
            verify_event_entry(active, &entry)?;
            active.prev_hash = entry.hash;
            active.last_event_seq = entry.seq;
            active.next_event_seq = next_seq(entry.seq, "seq")?;
        }
        if read < want {
            return Err(corrupt("receipt names events past the ledger head"));
        }
    }
    if active.receipt.body.ledger_head != active.prev_hash {
        return Err(corrupt("receipt head does not match the ledger"));
    }
    Ok(true)
}

fn verify_event_entry(active: &mut ActiveCommit, entry: &LedgerEntry) -> Result<(), CellError> {
    let event_corrupt = |reason: &str| CellError::LedgerCorrupt {
        seq: entry.seq,
        reason: reason.to_owned(),
    };
    if entry.seq != active.next_event_seq {
        return Err(event_corrupt("sequence gap"));
    }
    if entry.prev_hash != active.prev_hash {
        return Err(event_corrupt("previous hash does not chain"));
    }
    let body = &active.receipt.body;
    if entry.commit_seq != body.commit_seq || entry.world != body.world {
        return Err(CellError::LedgerCorrupt {
            seq: body.commit_seq,
            reason: "an event does not belong to its receipt".to_owned(),
        });
    }
    if let Some(requests) = &body.quantity {
        if let Some(marker) = crate::quantity::boundary_marker(entry, body)?
            && active.boundary.replace(marker).is_some()
        {
            return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
        }
        let start = active
            .end_event_seq
            .checked_sub(
                u64::try_from(requests.len())
                    .map_err(|_range| norn_kernel::quantity::QuantityError::Corrupt)?,
            )
            .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
        if entry.seq >= start {
            let request = requests
                .get(
                    usize::try_from(entry.seq.saturating_sub(start))
                        .map_err(|_range| norn_kernel::quantity::QuantityError::Corrupt)?,
                )
                .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
            if active
                .boundary
                .is_none_or(|boundary| body.first_event_seq < boundary.seq)
                || entry.event != crate::quantity::transition_event(request)?
            {
                return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
            }
        } else if entry.event.get("type").and_then(Value::as_str) == Some("quantity.transition") {
            return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
        }
    }
    Ok(())
}

fn step_commit_written<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    active: &mut ActiveCommit,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    let body = &active.receipt.body;
    while active.written_index < body.written.len() {
        if budget.exhausted() {
            return Ok(false);
        }
        let row = body
            .written
            .get(active.written_index)
            .ok_or_else(|| defect(body.commit_seq, "written snapshot index out of bounds"))?;
        reader.verify_written_snapshot(&body.world, body.commit_seq, row)?;
        budget.charge(1);
        active.written_index = active.written_index.saturating_add(1);
    }
    Ok(true)
}
