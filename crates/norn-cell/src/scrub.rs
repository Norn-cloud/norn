//! OPEN-CHECKPOINT-1 (#1551): bounded post-open history scrub (`CellStore::scrub_step`).
//!
//! 1. **Receipts**: verifies receipt bodies, lookup columns (S4), loaded digests, RFC 6962
//!    root, and quantity receipt digest; once done, idempotency misses are proven again.
//! 2. **History**: verifies ledger chain, export cell identity, definition installs (S9), and
//!    value snapshots (S7) in bounded chunks; refreshes target on external `data_version`
//!    changes (S10).
//! 3. **Seed**: verifies tail anchor, durable fence, and pins inside its transaction before
//!    recording the MAC'd checkpoint.
//!
//! Until step 2 ends, history reads and forks refuse with retryable `cell.verifying` (S7/S8/S13).
//! Any defect fences the cell durably (`cell.fenced`, S12) and retries persistence until durable.

use crate::engine::SqlEngine;
use crate::open_checkpoint::{self, ExportPin, OpenCheckpoint};
use crate::quantity::FormatBoundary;
use crate::{CellError, CellStore, LedgerHead};

mod durable_event;
mod history;
mod state;
mod support;

pub use state::ScrubStatus;
pub(crate) use state::{Advance, Gate, Scrub};

/// Shared per-wake SQL row budget passed through all `Phase::History` and
/// `Phase::Seed` sub-reads so total SQL rows per wake strictly stay `<= 512`.
pub(crate) struct RowBudget {
    remaining: usize,
}

impl RowBudget {
    pub(crate) const fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.remaining
    }

    pub(crate) const fn exhausted(&self) -> bool {
        self.remaining == 0
    }

    pub(crate) fn charge(&mut self, rows: usize) {
        self.remaining = self.remaining.saturating_sub(rows);
    }
}

/// The refusal code of a read or write held back by the scrub (retryable).
pub(crate) const VERIFYING: &str = "cell.verifying";
/// The refusal code of a fenced cell.
pub(crate) const FENCED: &str = "cell.fenced";

/// Receipts one receipts step reads (capped well below 512 so fence probes +
/// receipts stay strictly `<= 512` total SQL rows per wake).
pub(crate) const RECEIPT_STEP: usize = 256;
/// Loaded receipt digests one step copies into the rebuilt tree, for a
/// catch-up from a checkpoint (no SQL, hashing only).
const TREE_STEP: u64 = 8_192;
/// Maximum restarts from genesis when an external connection writes between
/// scrub transactions (S10, #1551 round 5).
const MAX_RESTARTS: u32 = 8;

fn fenced() -> CellError {
    CellError::Fenced("the history scrub found a defect".to_owned())
}

fn defect(seq: u64, reason: &str) -> CellError {
    CellError::LedgerCorrupt {
        seq,
        reason: format!("history scrub: {reason}"),
    }
}

/// Verified pins captured at the end of Phase 2 (`History`) or folded in
/// genesis-seed Phase 3 (`Seed`) and re-checked inside Phase 3's transaction
/// before writing the checkpoint (S10).
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedPins {
    export: Option<ExportPin>,
    definitions: String,
    definitions_max_version: u64,
}

/// What happens once Phase 1 (`Receipts`) finishes (`D5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostReceipts {
    /// Verify the full ledger, export identity, definitions, value snapshots,
    /// and quantity projections before seeding.
    VerifyHistory,
    /// Genesis open already verified the history: fold definition pins and seed
    /// the checkpoint directly.
    SeedOnly,
}

/// Phase 3 (`Seed`) sub-state (`D5`): either folding genesis definition pins or
/// holding verified pins ready to write the checkpoint.
#[derive(Debug, Clone)]
enum SeedState {
    /// Bounded definition-pin folding for [`PostReceipts::SeedOnly`].
    FoldingDefinitions(support::SeedDefsCursor),
    /// Verified export and definition pins ready to check and record.
    Ready(VerifiedPins),
}

/// Closed phase state machine for one scrub [`Run`] (`D5`): each phase owns
/// strictly the cursors and proofs valid in that phase.
enum RunPhase {
    /// Phase 1: verify receipts, rebuild the receipt tree, fold quantity digest.
    Receipts {
        receipts_at: u64,
        after: PostReceipts,
    },
    /// Phase 2: verify ledger chain, export identity, definitions, snapshots,
    /// and per-chunk quantity projections.
    History {
        cursor: Box<history::HistoryCursor>,
        quantity_scrub: Box<crate::quantity::QuantityScrub>,
    },
    /// Phase 2b: bounded tail quantity projection verification after Phase 2
    /// reaches `target` and produces [`VerifiedPins`].
    QuantityTail {
        verified_commits: u64,
        quantity_scrub: Box<crate::quantity::QuantityScrub>,
        verified_pins: VerifiedPins,
    },
    /// Phase 3: verify tail anchor and pins, then record the MAC'd checkpoint.
    Seed(SeedState),
}

impl RunPhase {
    const fn label(&self) -> &'static str {
        match self {
            Self::Receipts { .. } => "Receipts",
            Self::History { .. } => "History",
            Self::QuantityTail { .. } => "QuantityTail",
            Self::Seed(SeedState::FoldingDefinitions(_)) => "Seed(FoldingDefinitions)",
            Self::Seed(SeedState::Ready(_)) => "Seed(Ready)",
        }
    }
}

/// One scrub run: verify from `start` (genesis with none) to `target`, then seed.
/// S10: `data_version` tracks `SqlEngine::external_data_version`; external writes restart from genesis.
struct Run {
    start: Option<OpenCheckpoint>,
    /// A checkpoint whose root the rebuilt tree must match at its commits.
    anchor: Option<OpenCheckpoint>,
    target: LedgerHead,
    target_boundary: Option<FormatBoundary>,
    phase: RunPhase,
    tree: crate::transparency::ReceiptLog,
    quantity: Option<String>,
    data_version: Option<u64>,
    restarts: u32,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = formatter.debug_struct("Run");
        out.field("phase", &self.phase.label());
        match &self.phase {
            RunPhase::Receipts { receipts_at, .. } => {
                out.field("receipts_at", receipts_at);
            }
            RunPhase::History { cursor, .. } => {
                out.field("ledger", &cursor.ledger.commits);
            }
            RunPhase::QuantityTail {
                verified_commits, ..
            } => {
                out.field("ledger", verified_commits);
            }
            RunPhase::Seed(_) => {}
        }
        out.field("target", &self.target.commits)
            .finish_non_exhaustive()
    }
}

impl Run {
    fn new(
        start: Option<OpenCheckpoint>,
        anchor: Option<OpenCheckpoint>,
        target: LedgerHead,
        target_boundary: Option<FormatBoundary>,
        after: PostReceipts,
        data_version: Option<u64>,
    ) -> Self {
        let receipts_at = start.as_ref().map_or(0, |start| start.head.commits);
        let quantity = start.as_ref().map(|start| start.quantity_receipts.clone());
        Self {
            start,
            anchor,
            target,
            target_boundary,
            phase: RunPhase::Receipts { receipts_at, after },
            tree: crate::transparency::ReceiptLog::new(),
            quantity,
            data_version,
            restarts: 0,
        }
    }

    /// Discard cached step state and restart full verification from genesis
    /// through the refreshed `target` after an external write (S10).
    fn reset_from_genesis(
        &mut self,
        data_version: Option<u64>,
        target: LedgerHead,
        target_boundary: Option<FormatBoundary>,
    ) {
        self.start = None;
        self.target = target;
        self.target_boundary = target_boundary;
        self.phase = RunPhase::Receipts {
            receipts_at: 0,
            after: PostReceipts::VerifyHistory,
        };
        self.tree = crate::transparency::ReceiptLog::new();
        self.quantity = None;
        self.data_version = data_version;
        self.restarts = self.restarts.saturating_add(1);
    }

    const fn is_in_receipts(&self) -> bool {
        matches!(self.phase, RunPhase::Receipts { .. })
    }

    fn status(&self, history_ready: bool) -> ScrubStatus {
        let (phase, verified) = match &self.phase {
            RunPhase::Receipts { receipts_at, .. } => ("receipts", *receipts_at),
            RunPhase::History { cursor, .. } => ("history", cursor.ledger.commits),
            RunPhase::QuantityTail {
                verified_commits, ..
            } => ("history", *verified_commits),
            RunPhase::Seed(_) => ("seed", self.target.commits),
        };
        ScrubStatus::Verifying {
            phase,
            verified,
            target: self.target.commits,
            history_ready,
        }
    }

    /// The rebuilt tree at the anchor's size rebuilds the anchor's root.
    fn check_anchor(&self) -> Result<(), CellError> {
        if let Some(anchor) = &self.anchor
            && self.tree.len() == anchor.head.commits
            && open_checkpoint::root_hex(self.tree.root().as_bytes()) != anchor.receipts_root
        {
            return Err(defect(
                anchor.head.commits,
                "the receipts the checkpoint covers no longer rebuild its root",
            ));
        }
        Ok(())
    }

    /// Push `leaves` into the rebuilt tree, checking the anchor root the moment
    /// the tree reaches `anchor.head.commits`.
    fn push_leaves(&mut self, leaves: Vec<String>) -> Result<(), CellError> {
        self.check_anchor()?;
        for leaf in leaves {
            self.tree.push(leaf);
            self.check_anchor()?;
        }
        Ok(())
    }
}

/// Staged Phase 1 receipt batch applied after its transaction commits.
struct ReceiptsBatch {
    leaves: Vec<String>,
    log_append: Vec<(u64, String)>,
    receipts_at: u64,
    quantity: String,
    done: bool,
}

/// Progress of one Phase 2b (`QuantityTail`) step.
enum TailProgress {
    /// Tail verification has more bounded pages remaining.
    Continue(Box<crate::quantity::QuantityScrub>),
    /// Tail verification finished; ready to enter Phase 3 (`Seed`).
    Done,
}

/// Staged result of one scrub step transaction (S11): applied to [`Run`] only
/// after the transaction commits cleanly.
enum StepOutcome {
    Restart {
        data_version: Option<u64>,
        target: LedgerHead,
        boundary: Option<FormatBoundary>,
    },
    TreeBatch(Vec<String>),
    ReceiptsBatch(ReceiptsBatch),
    HistoryQuantityStep {
        quantity_scrub: Box<crate::quantity::QuantityScrub>,
    },
    HistoryChunk {
        history_cursor: Box<history::HistoryCursor>,
        quantity_scrub: Box<crate::quantity::QuantityScrub>,
        step: history::HistoryStep,
    },
    QuantityTailStep(TailProgress),
    SeedPins(support::SeedDefsStep),
    Seeded(Option<Box<OpenCheckpoint>>),
}

enum StepProgress {
    Continue,
    Done(Option<Box<OpenCheckpoint>>),
}

impl<E: SqlEngine> CellStore<E> {
    /// Where the history scrub stands (OPEN-CHECKPOINT-1, #1551 round 4).
    #[must_use]
    pub fn scrub_status(&self) -> ScrubStatus {
        self.sync_durable_fence();
        self.scrub.borrow().status()
    }

    /// Whether a local fence finding still needs durable persistence (S12).
    #[must_use]
    pub fn fence_persist_pending(&self) -> bool {
        self.scrub.borrow().needs_fence_persist()
    }

    /// Whether the scrub has work left: the host runs [`Self::scrub_step`]
    /// again at once (its due-work clock reports the cell due now).
    #[must_use]
    pub fn scrub_pending(&self) -> bool {
        matches!(self.scrub_status(), ScrubStatus::Verifying { .. }) || self.fence_persist_pending()
    }

    /// Run one bounded step of the history scrub in its own transaction,
    /// between commits. Step progress is staged and applied only after the
    /// transaction commits (S11). A defect fences the cell durably (S12); the
    /// returned status says so and carries the finding.
    pub fn scrub_step(&self) -> ScrubStatus {
        struct StepGuard<'a> {
            scrub: &'a std::cell::RefCell<Scrub>,
            run: Option<Box<Run>>,
        }
        impl Drop for StepGuard<'_> {
            fn drop(&mut self) {
                if let Some(run) = self.run.take()
                    && let Ok(mut scrub) = self.scrub.try_borrow_mut()
                {
                    scrub.restore_aborted_step(run);
                }
            }
        }
        self.persist_pending_fence();
        self.sync_durable_fence();
        let Some(run) = self.scrub.borrow_mut().begin_step() else {
            return self.scrub.borrow().status();
        };
        let mut guard = StepGuard {
            scrub: &self.scrub,
            run: Some(run),
        };
        // S11: `scrub_run_step` borrows `&run` immutably; `run` is only
        // mutated after the transaction commits cleanly. If the engine unwinds,
        // `StepGuard::drop` restores `run` so the handle never gets stuck in `Stepping`.
        let outcome = match guard.run.as_deref() {
            Some(run) => self.conn.transaction(|| self.scrub_run_step(run)),
            None => return self.scrub.borrow().status(),
        };
        let applied = outcome.and_then(|step| match guard.run.as_deref_mut() {
            Some(run) => apply_step(run, &self.transparency, step),
            None => Err(CellError::Engine(
                "history scrub step missing active run".to_owned(),
            )),
        });
        let Some(run) = guard.run.take() else {
            return self.scrub.borrow().status();
        };
        match applied {
            Ok(StepProgress::Continue) => {
                self.scrub.borrow_mut().finish_step_continue(run);
            }
            Ok(StepProgress::Done(checkpoint)) => {
                let commits = run.target.commits;
                self.scrub.borrow_mut().finish_step_done(&run, checkpoint);
                tracing::info!(commits, "history scrub: verified; checkpoint seeded");
            }
            Err(error) => {
                let retried = matches!(error, CellError::Engine(_) | CellError::Sqlite(_))
                    && self.scrub.borrow_mut().try_retry_transient(run);
                if retried {
                    tracing::warn!(%error, "history scrub: step failed; retrying");
                } else {
                    self.fence_on_defect(&error);
                }
            }
        }
        self.scrub.borrow().status()
    }

    fn fence_on_defect(&self, error: &CellError) {
        let finding = error.to_string();
        tracing::error!(%finding, "history scrub: the cell is fenced");
        self.scrub.borrow_mut().fence_pending(finding);
        // S12: drop the checkpoint row and persist the durable fence marker in
        // one transaction so subsequent calls, sibling handles and reopens fail
        // closed; retried on subsequent calls if persistence fails transiently.
        self.persist_pending_fence();
    }

    /// Run the scrub to its end (tests and tools): every step, back to back.
    pub fn scrub_to_end(&self) -> ScrubStatus {
        loop {
            let status = self.scrub_step();
            if !matches!(status, ScrubStatus::Verifying { .. }) {
                return status;
            }
        }
    }

    fn scrub_run_step(&self, run: &Run) -> Result<(StepOutcome, Option<u64>), CellError> {
        open_checkpoint::check_fence(&self.conn)?;
        // S10 (#1551 round 5): detect any external write to the SQLite file
        // between scrub transactions. Reading after `check_fence` guarantees
        // SQLite has opened its read snapshot and refreshed `PRAGMA data_version`.
        let dv = self.conn.external_data_version()?;
        if self.conn.read_isolation() != crate::engine::ReadIsolation::ExclusiveSynchronous
            && dv.is_none()
        {
            return Err(CellError::Engine(
                "history scrub requires external data_version on shared SQLite engines".to_owned(),
            ));
        }
        if run.data_version.is_some() && run.data_version != dv {
            if run.restarts >= MAX_RESTARTS {
                return Err(CellError::Engine(
                    "history scrub: concurrent database modifications prevented verification"
                        .to_owned(),
                ));
            }
            tracing::warn!(
                previous = run.data_version,
                current = dv,
                "history scrub: external write detected; restarting from genesis to live tail"
            );
            let loaded_len = self.transparency.borrow().len();
            let (target, boundary) = self.restart_scrub_target(loaded_len)?;
            return Ok((
                StepOutcome::Restart {
                    data_version: dv,
                    target,
                    boundary,
                },
                dv,
            ));
        }
        let outcome = match &run.phase {
            RunPhase::Receipts { receipts_at, .. } => self.scrub_receipts(run, *receipts_at)?,
            RunPhase::History {
                cursor,
                quantity_scrub,
            } => self.scrub_history(run, cursor, quantity_scrub)?,
            RunPhase::QuantityTail { quantity_scrub, .. } => {
                self.scrub_quantity_tail(run, quantity_scrub)?
            }
            RunPhase::Seed(seed_state) => self.scrub_seed(run, seed_state)?,
        };
        Ok((outcome, dv))
    }

    /// Phase 3: verify tail anchor and O(1) pins inside the seed tx before
    /// recording, folding definitions in bounded pages first on genesis seed-only runs (S10).
    fn scrub_seed(&self, run: &Run, seed_state: &SeedState) -> Result<StepOutcome, CellError> {
        let pins = match seed_state {
            SeedState::FoldingDefinitions(cursor) => {
                return Ok(StepOutcome::SeedPins(
                    cursor.clone().step(self, run.target.commits)?,
                ));
            }
            SeedState::Ready(pins) => pins,
        };
        let expected_hash = run
            .target
            .commits
            .checked_sub(1)
            .and_then(|idx| usize::try_from(idx).ok())
            .and_then(|idx| run.tree.get(idx))
            .map(String::as_str);
        self.verify_target_tail_anchor(&run.target, expected_hash)?;
        let export = crate::cell_identity::last_record_through(&self.conn, run.target.commits)?;
        let def_max = self
            .verifier()
            .latest_definition_version_through(run.target.commits)?;
        if export != pins.export || def_max != pins.definitions_max_version {
            return Err(defect(
                run.target.commits,
                "export or definition pins changed before checkpoint seed",
            ));
        }
        let quantity_receipts = match &run.quantity {
            Some(quantity) => quantity.clone(),
            None => open_checkpoint::quantity_seed()?,
        };
        let checkpoint = OpenCheckpoint {
            head: run.target.clone(),
            receipts_root: open_checkpoint::root_hex(run.tree.root().as_bytes()),
            boundary: run.target_boundary,
            quantity_receipts,
            export,
            definitions: pins.definitions.clone(),
        };
        Ok(StepOutcome::Seeded(
            self.record_scrub_checkpoint(checkpoint)?.map(Box::new),
        ))
    }

    /// Phase 1: the receipts after `receipts_at`, a page at a time; for
    /// a run from a checkpoint, first the digests it covers, copied from the
    /// loaded log into the rebuilt tree. Purely reads and stages the batch.
    fn scrub_receipts(&self, run: &Run, mut receipts_at: u64) -> Result<StepOutcome, CellError> {
        let log = self.transparency.borrow();
        let start = run.start.as_ref().map_or(0, |start| start.head.commits);
        if run.tree.len() < start {
            let to = start.min(run.tree.len().saturating_add(TREE_STEP));
            let count = usize::try_from(to.saturating_sub(run.tree.len()))
                .map_err(|_range| CellError::OutOfRange("tree step"))?;
            let mut leaves = Vec::with_capacity(count);
            let mut pos = run.tree.len();
            while pos < to {
                let leaf = usize::try_from(pos)
                    .ok()
                    .and_then(|index| log.get(index))
                    .ok_or_else(|| defect(pos, "the loaded receipt log is short"))?;
                leaves.push(leaf.clone());
                pos = pos.saturating_add(1);
            }
            return Ok(StepOutcome::TreeBatch(leaves));
        }
        let mut quantity = match &run.quantity {
            Some(quantity) => quantity.clone(),
            None => open_checkpoint::quantity_seed()?,
        };
        let page = self.verifier().receipts_after(receipts_at, RECEIPT_STEP)?;
        let read = page.len();
        let loaded_len = log.len();
        let mut leaves = Vec::with_capacity(read);
        let mut log_append = Vec::new();
        for receipt in page {
            let commit = receipt.body.commit_seq;
            if commit > run.target.commits {
                break;
            }
            let expected = receipts_at
                .checked_add(1)
                .ok_or(CellError::OutOfRange("commit_seq"))?;
            if commit != expected {
                return Err(defect(expected, "a receipt is missing from the history"));
            }
            if commit <= loaded_len {
                let loaded = usize::try_from(commit.saturating_sub(1))
                    .ok()
                    .and_then(|index| log.get(index));
                if loaded != Some(&receipt.hash) {
                    return Err(defect(
                        commit,
                        "a stored receipt digest is not the one the open loaded",
                    ));
                }
            } else {
                log_append.push((commit, receipt.hash.clone()));
            }
            if receipt.body.quantity.is_some() {
                quantity = open_checkpoint::quantity_extend(&quantity, commit, &receipt.hash)?;
            }
            leaves.push(receipt.hash);
            receipts_at = commit;
        }
        let done = receipts_at == run.target.commits;
        if !done && read == 0 {
            return Err(defect(
                receipts_at,
                "the history holds fewer receipts than the head the open verified",
            ));
        }
        Ok(StepOutcome::ReceiptsBatch(ReceiptsBatch {
            leaves,
            log_append,
            receipts_at,
            quantity,
            done,
        }))
    }

    /// Phase 2: advance history verification under a shared per-wake row budget
    /// (resumable across wakes by keyset cursor, S10).
    fn scrub_history(
        &self,
        run: &Run,
        cursor: &history::HistoryCursor,
        current_quantity_scrub: &crate::quantity::QuantityScrub,
    ) -> Result<StepOutcome, CellError> {
        let reader = self.verifier();
        if current_quantity_scrub.has_pending_work() {
            let mut quantity_scrub = current_quantity_scrub.clone();
            quantity_scrub.step_pending(&self.conn, &reader)?;
            return Ok(StepOutcome::HistoryQuantityStep {
                quantity_scrub: Box::new(quantity_scrub),
            });
        }
        let mut history_cursor = cursor.clone();
        let mut quantity_scrub = current_quantity_scrub.clone();
        let step = history_cursor.step(
            self,
            &reader,
            run.start.as_ref(),
            &run.target,
            run.target_boundary,
            &mut quantity_scrub,
        )?;
        Ok(StepOutcome::HistoryChunk {
            history_cursor: Box::new(history_cursor),
            quantity_scrub: Box::new(quantity_scrub),
            step,
        })
    }

    /// Phase 2b: bounded (`<= 512`-row) tail quantity projection verification (S10).
    fn scrub_quantity_tail(
        &self,
        run: &Run,
        current_quantity_scrub: &crate::quantity::QuantityScrub,
    ) -> Result<StepOutcome, CellError> {
        let reader = self.verifier();
        let mut quantity_scrub = current_quantity_scrub.clone();
        let done = quantity_scrub.step_tail(
            &self.conn,
            &reader,
            run.target.commits,
            run.target_boundary,
        )?;
        Ok(StepOutcome::QuantityTailStep(if done {
            TailProgress::Done
        } else {
            TailProgress::Continue(Box::new(quantity_scrub))
        }))
    }
}

/// Apply one committed step's staged progress to `run` (S11), failing closed on
/// any cross-phase mismatch.
fn apply_step(
    run: &mut Run,
    transparency: &std::cell::RefCell<crate::transparency::ReceiptLog>,
    (step, data_version): (StepOutcome, Option<u64>),
) -> Result<StepProgress, CellError> {
    if let StepOutcome::Restart {
        data_version: dv,
        target,
        boundary,
    } = step
    {
        run.reset_from_genesis(dv, target, boundary);
        return Ok(StepProgress::Continue);
    }
    if run.data_version.is_none() {
        run.data_version = data_version;
    }
    match step {
        StepOutcome::Restart { .. } => Ok(StepProgress::Continue),
        StepOutcome::TreeBatch(leaves) => {
            if !matches!(run.phase, RunPhase::Receipts { .. }) {
                return Err(defect(
                    run.target.commits,
                    "tree batch applied outside receipts phase",
                ));
            }
            run.push_leaves(leaves)?;
            Ok(StepProgress::Continue)
        }
        StepOutcome::ReceiptsBatch(batch) => apply_receipts_batch(run, transparency, batch),
        StepOutcome::HistoryQuantityStep { quantity_scrub } => {
            let RunPhase::History {
                quantity_scrub: slot,
                ..
            } = &mut run.phase
            else {
                return Err(defect(
                    run.target.commits,
                    "history quantity step applied outside history phase",
                ));
            };
            *slot = quantity_scrub;
            Ok(StepProgress::Continue)
        }
        StepOutcome::HistoryChunk {
            history_cursor,
            quantity_scrub,
            step: history_step,
        } => apply_history_chunk(run, history_cursor, quantity_scrub, history_step),
        StepOutcome::QuantityTailStep(progress) => apply_quantity_tail(run, progress),
        StepOutcome::SeedPins(defs_step) => apply_seed_pins(run, defs_step),
        StepOutcome::Seeded(checkpoint) => {
            if !matches!(run.phase, RunPhase::Seed(SeedState::Ready(_))) {
                return Err(defect(
                    run.target.commits,
                    "seeded checkpoint applied before verified pins were ready",
                ));
            }
            Ok(StepProgress::Done(checkpoint))
        }
    }
}

fn apply_receipts_batch(
    run: &mut Run,
    transparency: &std::cell::RefCell<crate::transparency::ReceiptLog>,
    batch: ReceiptsBatch,
) -> Result<StepProgress, CellError> {
    let RunPhase::Receipts { after, .. } = run.phase else {
        return Err(defect(
            run.target.commits,
            "receipts batch applied outside receipts phase",
        ));
    };
    if !batch.log_append.is_empty() {
        let mut log = transparency.borrow_mut();
        for (commit, hash) in batch.log_append {
            if log.len().checked_add(1) == Some(commit) {
                log.push(hash);
            }
        }
    }
    run.push_leaves(batch.leaves)?;
    run.quantity = Some(batch.quantity);
    run.phase = if batch.done {
        match after {
            PostReceipts::VerifyHistory => RunPhase::History {
                cursor: Box::new(history::HistoryCursor::new(run.start.as_ref())),
                quantity_scrub: Box::new(crate::quantity::QuantityScrub::new(
                    run.start.as_ref(),
                    run.target_boundary,
                )),
            },
            PostReceipts::SeedOnly => RunPhase::Seed(SeedState::FoldingDefinitions(
                support::SeedDefsCursor::default(),
            )),
        }
    } else {
        RunPhase::Receipts {
            receipts_at: batch.receipts_at,
            after,
        }
    };
    Ok(StepProgress::Continue)
}

fn apply_history_chunk(
    run: &mut Run,
    history_cursor: Box<history::HistoryCursor>,
    quantity_scrub: Box<crate::quantity::QuantityScrub>,
    history_step: history::HistoryStep,
) -> Result<StepProgress, CellError> {
    if !matches!(run.phase, RunPhase::History { .. }) {
        return Err(defect(
            run.target.commits,
            "history chunk applied outside history phase",
        ));
    }
    run.phase = match history_step {
        history::HistoryStep::Continue => RunPhase::History {
            cursor: history_cursor,
            quantity_scrub,
        },
        history::HistoryStep::Done(verified_pins) => RunPhase::QuantityTail {
            verified_commits: history_cursor.ledger.commits,
            quantity_scrub,
            verified_pins,
        },
    };
    Ok(StepProgress::Continue)
}

fn apply_quantity_tail(run: &mut Run, progress: TailProgress) -> Result<StepProgress, CellError> {
    match progress {
        TailProgress::Continue(next_scrub) => {
            let RunPhase::QuantityTail { quantity_scrub, .. } = &mut run.phase else {
                return Err(defect(
                    run.target.commits,
                    "quantity tail progress applied outside quantity tail phase",
                ));
            };
            *quantity_scrub = next_scrub;
            Ok(StepProgress::Continue)
        }
        TailProgress::Done => {
            if !matches!(run.phase, RunPhase::QuantityTail { .. }) {
                return Err(defect(
                    run.target.commits,
                    "quantity tail completion applied outside quantity tail phase",
                ));
            }
            let RunPhase::QuantityTail { verified_pins, .. } = std::mem::replace(
                &mut run.phase,
                RunPhase::Receipts {
                    receipts_at: 0,
                    after: PostReceipts::VerifyHistory,
                },
            ) else {
                return Err(defect(
                    run.target.commits,
                    "quantity tail completion applied outside quantity tail phase",
                ));
            };
            run.phase = RunPhase::Seed(SeedState::Ready(verified_pins));
            Ok(StepProgress::Continue)
        }
    }
}

fn apply_seed_pins(
    run: &mut Run,
    defs_step: support::SeedDefsStep,
) -> Result<StepProgress, CellError> {
    if !matches!(run.phase, RunPhase::Seed(SeedState::FoldingDefinitions(_))) {
        return Err(defect(
            run.target.commits,
            "seed definition step applied outside definition-folding state",
        ));
    }
    run.phase = match defs_step {
        support::SeedDefsStep::Continue(cursor) => {
            RunPhase::Seed(SeedState::FoldingDefinitions(cursor))
        }
        support::SeedDefsStep::Ready(pins) => RunPhase::Seed(SeedState::Ready(pins)),
    };
    Ok(StepProgress::Continue)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{
        PostReceipts, ReceiptsBatch, Run, RunPhase, SeedState, StepOutcome, StepProgress,
        TailProgress, VerifiedPins, apply_step, history, support,
    };
    use crate::open_checkpoint::OpenCheckpoint;
    use crate::transparency::ReceiptLog;
    use crate::{CellError, LedgerHead};

    fn head(commits: u64, seq: u64) -> LedgerHead {
        LedgerHead {
            seq,
            hash: format!("h-{commits}"),
            commits,
        }
    }

    fn checkpoint(commits: u64, seq: u64) -> OpenCheckpoint {
        OpenCheckpoint {
            head: head(commits, seq),
            receipts_root: format!("root-{commits}"),
            boundary: None,
            quantity_receipts: "q".to_owned(),
            export: None,
            definitions: "d".to_owned(),
        }
    }

    fn pins() -> VerifiedPins {
        VerifiedPins {
            export: None,
            definitions: "defs-digest".to_owned(),
            definitions_max_version: 1,
        }
    }

    #[test]
    fn run_phase_verify_history_transitions_and_restart() -> Result<(), CellError> {
        let log = RefCell::new(ReceiptLog::new());
        let mut run = Run::new(
            None,
            None,
            head(2, 4),
            None,
            PostReceipts::VerifyHistory,
            Some(1),
        );
        assert!(matches!(
            apply_step(&mut run, &log, (StepOutcome::Seeded(None), Some(1))),
            Err(CellError::LedgerCorrupt { .. })
        ));
        assert!(matches!(
            apply_step(
                &mut run,
                &log,
                (StepOutcome::QuantityTailStep(TailProgress::Done), Some(1))
            ),
            Err(CellError::LedgerCorrupt { .. })
        ));

        let step = apply_step(
            &mut run,
            &log,
            (
                StepOutcome::ReceiptsBatch(ReceiptsBatch {
                    leaves: vec!["r1".to_owned(), "r2".to_owned()],
                    log_append: Vec::new(),
                    receipts_at: 2,
                    quantity: "q2".to_owned(),
                    done: true,
                }),
                Some(1),
            ),
        )?;
        assert!(matches!(step, StepProgress::Continue));
        assert!(matches!(run.phase, RunPhase::History { .. }));

        let step = apply_step(
            &mut run,
            &log,
            (
                StepOutcome::HistoryChunk {
                    history_cursor: Box::new(history::HistoryCursor::new(None)),
                    quantity_scrub: Box::new(crate::quantity::QuantityScrub::new(None, None)),
                    step: history::HistoryStep::Done(pins()),
                },
                Some(1),
            ),
        )?;
        assert!(matches!(step, StepProgress::Continue));
        assert!(matches!(run.phase, RunPhase::QuantityTail { .. }));

        let step = apply_step(
            &mut run,
            &log,
            (StepOutcome::QuantityTailStep(TailProgress::Done), Some(1)),
        )?;
        assert!(matches!(step, StepProgress::Continue));
        assert!(matches!(run.phase, RunPhase::Seed(SeedState::Ready(_))));

        let step = apply_step(
            &mut run,
            &log,
            (
                StepOutcome::Restart {
                    data_version: Some(2),
                    target: head(3, 6),
                    boundary: None,
                },
                Some(2),
            ),
        )?;
        assert!(matches!(step, StepProgress::Continue));
        assert_eq!(run.restarts, 1);
        assert!(matches!(
            run.phase,
            RunPhase::Receipts {
                receipts_at: 0,
                after: PostReceipts::VerifyHistory
            }
        ));
        Ok(())
    }

    #[test]
    fn run_phase_seed_only_requires_verified_pins_before_seeded() -> Result<(), CellError> {
        let log = RefCell::new(ReceiptLog::new());
        let mut seed_only = Run::new(
            None,
            None,
            head(1, 2),
            None,
            PostReceipts::SeedOnly,
            Some(1),
        );
        assert!(matches!(
            apply_step(
                &mut seed_only,
                &log,
                (
                    StepOutcome::ReceiptsBatch(ReceiptsBatch {
                        leaves: vec!["r1".to_owned()],
                        log_append: Vec::new(),
                        receipts_at: 1,
                        quantity: "q1".to_owned(),
                        done: true,
                    }),
                    Some(1),
                ),
            )?,
            StepProgress::Continue
        ));
        assert!(matches!(
            seed_only.phase,
            RunPhase::Seed(SeedState::FoldingDefinitions(_))
        ));
        assert!(matches!(
            apply_step(&mut seed_only, &log, (StepOutcome::Seeded(None), Some(1))),
            Err(CellError::LedgerCorrupt { .. })
        ));
        assert!(matches!(
            apply_step(
                &mut seed_only,
                &log,
                (
                    StepOutcome::SeedPins(support::SeedDefsStep::Ready(pins())),
                    Some(1)
                ),
            )?,
            StepProgress::Continue
        ));
        assert!(matches!(
            seed_only.phase,
            RunPhase::Seed(SeedState::Ready(_))
        ));
        let done = apply_step(
            &mut seed_only,
            &log,
            (
                StepOutcome::Seeded(Some(Box::new(checkpoint(1, 2)))),
                Some(1),
            ),
        )?;
        assert!(matches!(done, StepProgress::Done(Some(_))));
        Ok(())
    }
}
