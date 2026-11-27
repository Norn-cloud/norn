//! Resumable, row-budget-bounded Phase 2 (`Phase::History`) stepper (S10).
//! Every wake enforces a shared `WAKE_ROW_BUDGET` across retained definitions,
//! receipts, ledger events (including packed payloads and durable fact proofs),
//! fact snapshots, export journal records (including packed blobs), and quantity
//! chunk probes so total SQL rows per wake strictly stay `<= 512`.

use std::collections::VecDeque;

use norn_ir::RuntimeValue as Value;

use super::durable_event::DurableEventCursor;
use super::support::DefInstallCursor;
use super::{RowBudget, VerifiedPins, defect};
use crate::cell_identity::CellId;
use crate::codec::hash_json;
use crate::engine::SqlEngine;
use crate::open_checkpoint::verify::DefinitionWalk;
use crate::open_checkpoint::{self, OpenCheckpoint};
use crate::payload_storage::ExportDecodeCursor;
use crate::quantity::FormatBoundary;
use crate::read::{LedgerEntry, StoreReader};
use crate::store::InstalledDefinitionSet;
use crate::system::GENESIS_HASH;
use crate::{CellError, CellStore, LedgerHead, Receipt};

/// Shared per-wake row budget for `Phase::History` and `Seed` definition folding,
/// leaving generous headroom below the 512-row total per-wake bound.
pub(super) const WAKE_ROW_BUDGET: usize = 256;

/// Worst-case SQL rows read by one `scrub_ledger_page` entry (1 ledger row +
/// 1 payload ref + 1 scope check + 1 payload blob).
pub(super) const LEDGER_ENTRY_MAX_ROWS: usize = 4;
const RECEIPT_PAGE_CAP: usize = 16;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum AnchorStage {
    #[default]
    Ledger,
    ExportPin,
    DefPinInit,
    DefPinRunning,
    Done,
}

#[derive(Clone, Default)]
struct AnchorState {
    stage: AnchorStage,
    anchor_receipt: Option<Receipt>,
    anchor_durable: Option<DurableEventCursor>,
    export_pin_cursor: Option<ExportDecodeCursor>,
    def_install: Option<DefInstallCursor>,
}

#[derive(Clone)]
struct ActiveCommit {
    receipt: Receipt,
    next_event_seq: u64,
    last_event_seq: u64,
    end_event_seq: u64,
    prev_hash: String,
    boundary: Option<FormatBoundary>,
    pending_events: VecDeque<LedgerEntry>,
    durable: Option<DurableEventCursor>,
    events_head_checked: bool,
    written_index: usize,
    definition_checked: bool,
    def_install: Option<DefInstallCursor>,
}

#[derive(Clone, Default)]
enum CachedCellId {
    #[default]
    Unloaded,
    Loaded(Option<CellId>),
}

#[derive(Clone)]
struct ExportScanState {
    checked_through: u64,
    current_id: CachedCellId,
    active_seq: Option<u64>,
    cursor: Option<ExportDecodeCursor>,
}

/// Resumable history-scrub cursor carried across `Phase::History` wakes (S10).
#[derive(Clone)]
pub(super) struct HistoryCursor {
    versions: Vec<InstalledDefinitionSet>,
    versions_loaded: bool,
    versions_after: u64,
    anchor_checked: bool,
    anchor: AnchorState,
    pub(super) ledger: LedgerHead,
    boundary: Option<FormatBoundary>,
    time_floor: u64,
    walk: DefinitionWalk,
    buffered: VecDeque<Receipt>,
    active: Option<ActiveCommit>,
    export_scan: ExportScanState,
}

pub(super) enum HistoryStep {
    Continue,
    Done(VerifiedPins),
}

impl HistoryCursor {
    pub(super) fn new(start: Option<&OpenCheckpoint>) -> Self {
        let ledger = start.map_or_else(LedgerHead::genesis, |cp| cp.head.clone());
        let checked_through = ledger.commits;
        Self {
            versions: Vec::new(),
            versions_loaded: false,
            versions_after: 0,
            anchor_checked: false,
            anchor: AnchorState::default(),
            ledger,
            boundary: start.and_then(|cp| cp.boundary),
            time_floor: 0,
            walk: DefinitionWalk::default(),
            buffered: VecDeque::new(),
            active: None,
            export_scan: ExportScanState {
                checked_through,
                current_id: CachedCellId::Unloaded,
                active_seq: None,
                cursor: None,
            },
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
        if !self.anchor_checked
            && !self.step_verify_anchor(store, reader, start, target.commits, &mut budget)?
        {
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
                &mut budget,
            )?;
        }
        self.step_export_records(&store.conn, &mut budget)?;
        if self.ledger.commits == target.commits
            && self.active.is_none()
            && self.export_scan.checked_through == target.commits
            && self.export_scan.active_seq.is_none()
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
        if budget.remaining() < 7 {
            return Ok(false);
        }
        let cap = budget.remaining().saturating_sub(3) / 4;
        let page =
            reader.installed_definition_versions_after(self.versions_after, cap.max(1), budget)?;
        let read = page.len();
        if let Some(last) = page.last() {
            self.versions_after = last.version;
        }
        self.versions.extend(page);
        if read < cap.max(1) {
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
        target_commits: u64,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        if !self.step_anchor_ledger_and_receipt(reader, budget)? {
            return Ok(false);
        }
        if let Some(start_cp) = start {
            if !self.step_anchor_export_pin(&store.conn, start_cp, budget)? {
                return Ok(false);
            }
            if !self.step_anchor_definitions(store, reader, start_cp, budget)? {
                return Ok(false);
            }
        } else if target_commits == 0 && self.anchor.stage != AnchorStage::Done {
            if budget.exhausted() {
                return Ok(false);
            }
            let stray = reader.receipts_page_from(0, 1)?;
            budget.charge(stray.len());
            if let Some(receipt) = stray.first() {
                return Err(CellError::LedgerCorrupt {
                    seq: receipt.body.commit_seq,
                    reason: "unexpected receipt in empty history".to_owned(),
                });
            }
            self.anchor.stage = AnchorStage::Done;
        }
        self.anchor_checked = true;
        Ok(true)
    }

    fn step_anchor_ledger_and_receipt<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        if let Some(durable) = self.anchor.anchor_durable.as_mut() {
            if !durable.step(reader, &self.versions, budget)? {
                return Ok(false);
            }
            self.anchor.anchor_durable = None;
            self.anchor.stage = AnchorStage::ExportPin;
            return Ok(true);
        }
        if self.anchor.stage != AnchorStage::Ledger {
            return Ok(true);
        }
        let base = self.ledger.seq;
        let anchor_corrupt = || CellError::LedgerCorrupt {
            seq: base,
            reason: "the verified checkpoint no longer anchors the ledger".to_owned(),
        };
        if self.ledger.commits > 0 && self.anchor.anchor_receipt.is_none() {
            if budget.exhausted() {
                return Ok(false);
            }
            let anchor = reader.receipts_after(self.ledger.commits.saturating_sub(1), 1)?;
            budget.charge(anchor.len());
            let receipt = anchor.into_iter().next().ok_or_else(anchor_corrupt)?;
            if receipt.body.commit_seq != self.ledger.commits
                || receipt.body.ledger_head != self.ledger.hash
                || receipt
                    .body
                    .first_event_seq
                    .checked_add(receipt.body.event_count)
                    != base.checked_add(1)
                || hash_json(&receipt.body)? != receipt.hash
            {
                return Err(anchor_corrupt());
            }
            self.time_floor = receipt.body.logical_time;
            self.anchor.anchor_receipt = Some(receipt);
        } else if self.ledger.commits == 0 && base != 0 {
            return Err(anchor_corrupt());
        }
        if base == 0 {
            if self.ledger.hash != GENESIS_HASH {
                return Err(anchor_corrupt());
            }
            self.anchor.stage = AnchorStage::ExportPin;
            return Ok(true);
        }
        if budget.remaining() < LEDGER_ENTRY_MAX_ROWS {
            return Ok(false);
        }
        let entries = reader.scrub_ledger_page(base, 1, budget)?;
        let entry = entries
            .into_iter()
            .next()
            .filter(|entry| entry.seq == base && entry.hash == self.ledger.hash)
            .ok_or_else(anchor_corrupt)?;
        if crate::history::durable_event_is_proven(&entry.event)
            && let Some(receipt) = self.anchor.anchor_receipt.as_ref()
            && let Some(mut durable) =
                DurableEventCursor::new(&entry.world, entry.commit_seq, receipt, &entry.event)?
            && !durable.step(reader, &self.versions, budget)?
        {
            self.anchor.anchor_durable = Some(durable);
            return Ok(false);
        }
        self.anchor.stage = AnchorStage::ExportPin;
        Ok(true)
    }

    fn step_anchor_export_pin(
        &mut self,
        engine: &impl SqlEngine,
        start_cp: &OpenCheckpoint,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        if self.anchor.stage != AnchorStage::ExportPin {
            return Ok(true);
        }
        if self.anchor.export_pin_cursor.is_none() {
            if budget.remaining() < 2 {
                return Ok(false);
            }
            let last = crate::cell_identity::last_record_through(engine, start_cp.head.commits)?;
            budget.charge(usize::from(last.is_some()));
            if last != start_cp.export {
                return Err(open_checkpoint::corrupt(
                    start_cp.head.commits,
                    "the last export record it covers is not the one it pinned",
                ));
            }
            let Some(pin) = &start_cp.export else {
                self.anchor.stage = AnchorStage::DefPinInit;
                return Ok(true);
            };
            self.anchor.export_pin_cursor = Some(
                ExportDecodeCursor::open(engine, pin.seq, budget)?.ok_or_else(|| {
                    CellError::LedgerCorrupt {
                        seq: pin.seq,
                        reason: "retained journal record disappeared during admission".to_owned(),
                    }
                })?,
            );
        }
        let Some(cursor) = self.anchor.export_pin_cursor.as_mut() else {
            return Err(CellError::OutOfRange("anchor export cursor"));
        };
        let Some(record) = cursor.step(engine, budget)? else {
            return Ok(false);
        };
        if budget.remaining() < 2 {
            self.anchor.export_pin_cursor = Some(ExportDecodeCursor::ready(record));
            return Ok(false);
        }
        let current = crate::cell_identity::read(engine)?;
        budget.charge(2);
        crate::cell_identity::check_export_record_identity(&record, current.as_ref())?;
        self.anchor.export_pin_cursor = None;
        self.anchor.stage = AnchorStage::DefPinInit;
        Ok(true)
    }

    fn step_anchor_definitions<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        reader: &StoreReader<'_, E>,
        start_cp: &OpenCheckpoint,
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        let commits = start_cp.head.commits;
        if self.anchor.stage == AnchorStage::DefPinInit {
            if open_checkpoint::definitions_digest(&self.versions, commits)? != start_cp.definitions
            {
                return Err(open_checkpoint::corrupt(
                    commits,
                    "the retained definition versions it covers changed",
                ));
            }
            let mut walk = DefinitionWalk::default();
            for version in self
                .versions
                .iter()
                .take_while(|version| version.commit_seq <= commits)
            {
                if version.version != walk.next_version
                    || version.commit_seq <= walk.previous_install_seq
                {
                    return Err(CellError::LedgerCorrupt {
                        seq: version.commit_seq,
                        reason: "definition history version or receipt order is inconsistent"
                            .to_owned(),
                    });
                }
                walk.installed(version)?;
            }
            if commits > 0
                && self
                    .anchor
                    .anchor_receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.body.definitions_version != walk.active_version)
            {
                return Err(CellError::LedgerCorrupt {
                    seq: commits,
                    reason:
                        "the open checkpoint's receipt names a definitions version not in force"
                            .to_owned(),
                });
            }
            if let Some(index) = walk.version_index.checked_sub(1) {
                if budget.exhausted() {
                    return Ok(false);
                }
                let version = self
                    .versions
                    .get(index)
                    .ok_or(CellError::OutOfRange("definition history index"))?;
                let receipt = reader
                    .receipt_at(version.commit_seq)?
                    .filter(|receipt| receipt.body.definitions_version == version.version)
                    .ok_or_else(|| CellError::LedgerCorrupt {
                        seq: version.commit_seq,
                        reason: "the active definitions have no installing receipt".to_owned(),
                    })?;
                budget.charge(1);
                self.anchor.def_install = Some(DefInstallCursor::new(index, receipt));
            }
            self.walk = walk;
            self.anchor.stage = AnchorStage::DefPinRunning;
        }
        if let Some(install) = self.anchor.def_install.as_mut() {
            if !install.step(store, reader, &self.versions, budget)? {
                return Ok(false);
            }
            self.anchor.def_install = None;
        }
        self.anchor.stage = AnchorStage::Done;
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
                if budget.remaining() < LEDGER_ENTRY_MAX_ROWS {
                    break;
                }
                let receipt = self.next_receipt(reader, target_commits, budget)?;
                crate::fork_evidence::verify_receipt_fork_binding(store.fork_pin(), &receipt.body)?;
                self.active = Some(self.begin_commit(receipt)?);
            }
            let mut active = self.active.take().ok_or_else(|| {
                defect(self.ledger.commits, "missing active history scrub commit")
            })?;
            if !step_commit_events(
                reader,
                &self.versions,
                &mut active,
                self.ledger.seq == 0,
                budget,
            )? {
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
        budget.charge(page.len());
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
            pending_events: VecDeque::new(),
            durable: None,
            events_head_checked: false,
            written_index: 0,
            definition_checked: false,
            def_install: None,
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
        if active.def_install.is_none() {
            if let Some(version) = self.versions.get(self.walk.version_index)
                && version.commit_seq == active.receipt.body.commit_seq
            {
                if version.version != self.walk.next_version
                    || version.commit_seq <= self.walk.previous_install_seq
                    || active.receipt.body.definitions_version != version.version
                {
                    return Err(CellError::LedgerCorrupt {
                        seq: active.receipt.body.commit_seq,
                        reason: "definition history version or receipt order is inconsistent"
                            .to_owned(),
                    });
                }
                active.def_install = Some(DefInstallCursor::new(
                    self.walk.version_index,
                    active.receipt.clone(),
                ));
            } else if active.receipt.body.definitions_version != self.walk.active_version {
                return Err(CellError::LedgerCorrupt {
                    seq: active.receipt.body.commit_seq,
                    reason: "receipt names a definitions version not in force".to_owned(),
                });
            } else {
                active.definition_checked = true;
                return Ok(true);
            }
        }
        if let Some(install) = active.def_install.as_mut() {
            if !install.step(store, reader, &self.versions, budget)? {
                return Ok(false);
            }
            let version = self
                .versions
                .get(self.walk.version_index)
                .ok_or(CellError::OutOfRange("definition history index"))?;
            self.walk.installed(version)?;
            active.def_install = None;
        }
        active.definition_checked = true;
        Ok(true)
    }

    fn step_export_records(
        &mut self,
        engine: &impl SqlEngine,
        budget: &mut RowBudget,
    ) -> Result<(), CellError> {
        let through = self.ledger.commits;
        while self.export_scan.active_seq.is_some() || self.export_scan.checked_through < through {
            if self.export_scan.active_seq.is_none() {
                if budget.exhausted() {
                    return Ok(());
                }
                let Some(seq) = crate::cell_identity::next_record_seq_between(
                    engine,
                    self.export_scan.checked_through,
                    through,
                    budget,
                )?
                else {
                    self.export_scan.checked_through = through;
                    return Ok(());
                };
                self.export_scan.active_seq = Some(seq);
            }
            if matches!(self.export_scan.current_id, CachedCellId::Unloaded) {
                if budget.remaining() < 2 {
                    return Ok(());
                }
                self.export_scan.current_id =
                    CachedCellId::Loaded(crate::cell_identity::read(engine)?);
                budget.charge(2);
            }
            let Some(seq) = self.export_scan.active_seq else {
                return Ok(());
            };
            if self.export_scan.cursor.is_none() {
                if budget.exhausted() {
                    return Ok(());
                }
                self.export_scan.cursor = Some(
                    ExportDecodeCursor::open(engine, seq, budget)?.ok_or_else(|| {
                        CellError::LedgerCorrupt {
                            seq,
                            reason: "retained journal record disappeared during admission"
                                .to_owned(),
                        }
                    })?,
                );
            }
            let Some(cursor) = self.export_scan.cursor.as_mut() else {
                return Ok(());
            };
            let Some(record) = cursor.step(engine, budget)? else {
                return Ok(());
            };
            let current = match &self.export_scan.current_id {
                CachedCellId::Loaded(id) => id.as_ref(),
                CachedCellId::Unloaded => None,
            };
            crate::cell_identity::check_export_record_identity(&record, current)?;
            self.export_scan.cursor = None;
            self.export_scan.active_seq = None;
            self.export_scan.checked_through = seq;
        }
        Ok(())
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
        let mut installed_count = 0_u64;
        let mut max_version = 0_u64;
        for version in &self.versions {
            if version.commit_seq <= through {
                installed_count = installed_count.saturating_add(1);
                max_version = max_version.max(version.version);
            }
        }
        if usize::try_from(installed_count).ok() != Some(self.walk.version_index) {
            return Err(defect(
                through,
                "the definition history and the receipts disagree",
            ));
        }
        Ok(VerifiedPins {
            export: crate::cell_identity::last_record_through(&store.conn, through)?,
            definitions: open_checkpoint::definitions_digest(&self.versions, through)?,
            definitions_max_version: max_version,
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
    versions: &[InstalledDefinitionSet],
    active: &mut ActiveCommit,
    from_genesis: bool,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    if active.events_head_checked {
        return Ok(true);
    }
    let commit_seq = active.receipt.body.commit_seq;
    let corrupt = |reason: &str| CellError::LedgerCorrupt {
        seq: commit_seq,
        reason: reason.to_owned(),
    };
    loop {
        if let Some(durable) = active.durable.as_mut() {
            if !durable.step(reader, versions, budget)? {
                return Ok(false);
            }
            active.durable = None;
        }
        if let Some(entry) = active.pending_events.pop_front() {
            verify_event_entry(active, &entry)?;
            active.prev_hash = entry.hash;
            active.last_event_seq = entry.seq;
            active.next_event_seq = next_seq(entry.seq, "seq")?;
            if crate::history::durable_event_is_proven(&entry.event) {
                active.durable = DurableEventCursor::new(
                    &entry.world,
                    entry.commit_seq,
                    &active.receipt,
                    &entry.event,
                )?;
            }
            continue;
        }
        if active.next_event_seq >= active.end_event_seq {
            break;
        }
        let cap = budget.remaining() / LEDGER_ENTRY_MAX_ROWS;
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
        let entries =
            reader.scrub_ledger_page(from, want, active.receipt.body.fork_pin.as_ref(), budget)?;
        if entries.len() < want {
            return Err(corrupt("receipt names events past the ledger head"));
        }
        active.pending_events.extend(entries);
    }
    if active.receipt.body.ledger_head != active.prev_hash {
        return Err(corrupt("receipt head does not match the ledger"));
    }
    active.events_head_checked = true;
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
    if entry.commit_seq != body.commit_seq
        || entry.world != body.world
        || entry.fork_pin != body.fork_pin
    {
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
