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
mod support;

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
/// Consecutive engine failures a step retries before it fences the cell.
const TRANSIENT_RETRIES: u32 = 3;
/// Maximum restarts from genesis when an external connection writes between
/// scrub transactions (S10, #1551 round 5).
const MAX_RESTARTS: u32 = 8;

/// Which history reads wait for the scrub; copied into every reader the
/// store serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gate {
    covered_seq: Option<u64>,
    idempotency_unproven: bool,
    fenced: bool,
}

impl Gate {
    /// Nothing is held back.
    pub(crate) const OPEN: Self = Self {
        covered_seq: None,
        idempotency_unproven: false,
        fenced: false,
    };

    /// Refuse if the cell has been fenced by the history scrub (S12).
    ///
    /// # Errors
    /// [`CellError::Fenced`] on a fenced cell.
    pub(crate) fn refuse_fenced(self) -> Result<(), CellError> {
        if self.fenced {
            return Err(fenced());
        }
        Ok(())
    }

    /// A read that needs the whole history complete (S7, S8).
    ///
    /// # Errors
    /// [`CellError::Verifying`] while the scrub runs; [`CellError::Fenced`].
    pub(crate) fn history(self, what: &'static str) -> Result<(), CellError> {
        self.refuse_fenced()?;
        if self.covered_seq.is_some() {
            return Err(CellError::Verifying(what));
        }
        Ok(())
    }

    /// A ledger page after `after` (S8).
    /// # Errors
    /// [`CellError::Verifying`] or [`CellError::Fenced`].
    pub(crate) fn ledger(self, after: u64) -> Result<(), CellError> {
        self.refuse_fenced()?;
        if self.covered_seq.is_some_and(|seq| after < seq) {
            return Err(CellError::Verifying(
                "ledger pages the open checkpoint covers",
            ));
        }
        Ok(())
    }

    /// An idempotency lookup missed before receipt scrub completed (S4).
    /// # Errors
    /// [`CellError::Verifying`] or [`CellError::Fenced`].
    pub(crate) fn receipt_miss(self) -> Result<(), CellError> {
        self.refuse_fenced()?;
        if self.idempotency_unproven {
            return Err(CellError::Verifying(
                "idempotency keys the open checkpoint covers",
            ));
        }
        Ok(())
    }
}

fn fenced() -> CellError {
    CellError::Fenced("the history scrub found a defect".to_owned())
}

fn defect(seq: u64, reason: &str) -> CellError {
    CellError::LedgerCorrupt {
        seq,
        reason: format!("history scrub: {reason}"),
    }
}

/// Where the cell's history scrub stands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScrubStatus {
    /// Nothing to verify: the history the cell serves is verified.
    Idle,
    /// A scrub is running.
    Verifying {
        /// `receipts`, `history` or `seed`.
        phase: &'static str,
        /// Commits verified in this phase.
        verified: u64,
        /// The head it verifies to.
        target: u64,
        /// Whether history reads are served already (false only after an
        /// open from a checkpoint, until its history is verified).
        history_ready: bool,
    },
    /// The scrub found a defect: the cell refuses commits and history reads.
    Fenced {
        /// What did not verify.
        finding: String,
    },
}

/// What a commit's checkpoint advancement may do.
pub(crate) enum Advance {
    /// Nothing now: a scrub is running, the history is unverified, the cell
    /// is fenced or this is a fork.
    Hold,
    /// Advance from this validated checkpoint.
    From(OpenCheckpoint),
    /// No validated checkpoint: the scrub seeds one.
    Seed,
}

/// The scrub's state on one store handle.
#[derive(Debug, Default)]
pub(crate) struct Scrub {
    covered: Option<LedgerHead>,
    idempotency_unproven: bool,
    validated: Option<OpenCheckpoint>,
    run: Option<Run>,
    finding: Option<String>,
    fence_persisted: bool,
    /// Last `PRAGMA data_version` at which no sibling durable fence existed (S12).
    fence_checked_dv: Option<u64>,
    /// Last `PRAGMA data_version` at which `_norn_link_claim_frontier` verified clean.
    frontier_checked_dv: Option<u64>,
    /// `PRAGMA data_version` at open or last verified checkpoint (S10 catch-up baseline).
    baseline_dv: Option<u64>,
    transient: u32,
    fork: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Receipts,
    History,
    QuantityTail,
    Seed,
}

/// Verified pins captured at the end of Phase 2 (`History`) and re-checked
/// inside Phase 3 (`Seed`)'s transaction before writing the checkpoint (S10).
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedPins {
    export: Option<ExportPin>,
    definitions: String,
    definitions_max_version: u64,
}

/// One scrub run: verify from `start` (genesis with none) to `target`, then seed.
/// S10: `data_version` tracks `SqlEngine::external_data_version`; external writes restart from genesis.
struct Run {
    start: Option<OpenCheckpoint>,
    /// A checkpoint whose root the rebuilt tree must match at its commits.
    anchor: Option<OpenCheckpoint>,
    target: LedgerHead,
    target_boundary: Option<FormatBoundary>,
    /// Whether to verify the history (false after a genesis open, which
    /// just verified it: the run only rebuilds what the seed records).
    history: bool,
    phase: Phase,
    tree: crate::transparency::ReceiptLog,
    quantity: Option<String>,
    receipts_at: u64,
    history_cursor: history::HistoryCursor,
    quantity_scrub: crate::quantity::QuantityScrub,
    verified_pins: Option<VerifiedPins>,
    seed_defs: support::SeedDefsCursor,
    data_version: Option<u64>,
    restarts: u32,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Run")
            .field("phase", &self.phase)
            .field("receipts_at", &self.receipts_at)
            .field("ledger", &self.history_cursor.ledger.commits)
            .field("target", &self.target.commits)
            .finish_non_exhaustive()
    }
}

impl Run {
    fn new(
        start: Option<OpenCheckpoint>,
        anchor: Option<OpenCheckpoint>,
        target: LedgerHead,
        target_boundary: Option<FormatBoundary>,
        history: bool,
        data_version: Option<u64>,
    ) -> Self {
        Self {
            receipts_at: start.as_ref().map_or(0, |start| start.head.commits),
            history_cursor: history::HistoryCursor::new(start.as_ref()),
            quantity: start.as_ref().map(|start| start.quantity_receipts.clone()),
            quantity_scrub: crate::quantity::QuantityScrub::new(start.as_ref(), target_boundary),
            start,
            anchor,
            target,
            target_boundary,
            history,
            phase: Phase::Receipts,
            tree: crate::transparency::ReceiptLog::new(),
            verified_pins: None,
            seed_defs: support::SeedDefsCursor::default(),
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
        self.history = true;
        self.phase = Phase::Receipts;
        self.tree = crate::transparency::ReceiptLog::new();
        self.quantity = None;
        self.receipts_at = 0;
        self.history_cursor = history::HistoryCursor::new(None);
        self.quantity_scrub = crate::quantity::QuantityScrub::new(None, target_boundary);
        self.verified_pins = None;
        self.seed_defs = support::SeedDefsCursor::default();
        self.data_version = data_version;
        self.restarts = self.restarts.saturating_add(1);
    }

    fn status(&self, history_ready: bool) -> ScrubStatus {
        let (phase, verified) = match self.phase {
            Phase::Receipts => ("receipts", self.receipts_at),
            Phase::History | Phase::QuantityTail => ("history", self.history_cursor.ledger.commits),
            Phase::Seed => ("seed", self.target.commits),
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

/// Staged result of one scrub step transaction (S11): applied to [`Run`] only
/// after the transaction commits cleanly.
enum StepOutcome {
    Restart {
        data_version: Option<u64>,
        target: LedgerHead,
        boundary: Option<FormatBoundary>,
    },
    TreeBatch(Vec<String>),
    ReceiptsBatch {
        leaves: Vec<String>,
        log_append: Vec<(u64, String)>,
        receipts_at: u64,
        quantity: String,
        done: bool,
    },
    HistoryChunk {
        history_cursor: Box<history::HistoryCursor>,
        quantity_scrub: Box<crate::quantity::QuantityScrub>,
        verified_pins: Option<VerifiedPins>,
        done: bool,
    },
    QuantityStep {
        quantity_scrub: Box<crate::quantity::QuantityScrub>,
        done: bool,
    },
    SeedPins {
        seed_defs: support::SeedDefsCursor,
        verified_pins: Option<VerifiedPins>,
    },
    Seeded(Option<OpenCheckpoint>),
}

impl Scrub {
    /// The scrub of a handle that just opened at `head`: from `trusted`,
    /// the authentic checkpoint the open started from, a full history scrub
    /// that holds back history reads; after a genesis open, a run that only
    /// seeds the checkpoint; nothing without a key (`keyed`) or when the open
    /// `recorded` one itself (an upgrade).
    pub(crate) fn after_open(
        keyed: bool,
        trusted: Option<OpenCheckpoint>,
        recorded: Option<OpenCheckpoint>,
        head: &LedgerHead,
        boundary: Option<FormatBoundary>,
        data_version: Option<u64>,
    ) -> Self {
        let mut scrub = Self {
            fence_checked_dv: data_version,
            frontier_checked_dv: data_version,
            baseline_dv: data_version,
            ..Self::default()
        };
        if !keyed {
            return scrub;
        }
        if let Some(checkpoint) = trusted {
            scrub.covered = Some(checkpoint.head.clone());
            scrub.idempotency_unproven = true;
            scrub.run = Some(Run::new(
                None,
                Some(checkpoint.clone()),
                head.clone(),
                boundary,
                true,
                data_version,
            ));
            scrub.validated = Some(checkpoint);
        } else if let Some(checkpoint) = recorded {
            scrub.validated = Some(checkpoint);
        } else if head.commits > open_checkpoint::MAX_ADVANCE {
            // A shorter history is checkpointed by the next commit that is
            // due one, from genesis, within the commit's bound.
            scrub.run = Some(Run::new(
                None,
                None,
                head.clone(),
                boundary,
                false,
                data_version,
            ));
        }
        scrub
    }

    /// A fork's scrub state: only created from an unfenced, scrub-verified
    /// parent (S13), and never runs a scrub or records a checkpoint of its own.
    pub(crate) fn for_fork(&self) -> Self {
        Self {
            covered: None,
            idempotency_unproven: false,
            finding: self.finding.clone(),
            fence_persisted: self.fence_persisted,
            fork: true,
            ..Self::default()
        }
    }

    pub(crate) fn pending_fence_finding(&self) -> Option<String> {
        self.needs_fence_persist()
            .then(|| self.finding.clone())
            .flatten()
    }

    pub(crate) const fn mark_fence_persisted(&mut self) {
        self.fence_persisted = true;
    }

    pub(crate) const fn is_fenced(&self) -> bool {
        self.finding.is_some()
    }

    pub(crate) const fn needs_fence_persist(&self) -> bool {
        self.finding.is_some() && !self.fence_persisted
    }

    pub(crate) const fn fence_checked_dv(&self) -> Option<u64> {
        self.fence_checked_dv
    }

    pub(crate) const fn set_fence_checked_dv(&mut self, dv: u64) {
        self.fence_checked_dv = Some(dv);
    }

    pub(crate) const fn frontier_checked_dv(&self) -> Option<u64> {
        self.frontier_checked_dv
    }

    pub(crate) const fn set_frontier_checked_dv(&mut self, dv: u64) {
        self.frontier_checked_dv = Some(dv);
    }

    pub(crate) fn record_durable_fence(&mut self, finding: String) {
        self.finding = Some(finding);
        self.fence_persisted = true;
        self.run = None;
    }

    /// The gate the served reader carries.
    pub(crate) fn gate(&self) -> Gate {
        Gate {
            covered_seq: self.covered.as_ref().map(|head| head.seq),
            idempotency_unproven: self.idempotency_unproven,
            fenced: self.finding.is_some(),
        }
    }

    /// # Errors
    /// [`CellError::Fenced`] on a fenced cell.
    pub(crate) fn refuse_fenced(&self) -> Result<(), CellError> {
        self.finding
            .as_ref()
            .map_or(Ok(()), |finding| Err(CellError::Fenced(finding.clone())))
    }

    /// S13: refuse to fork while a checkpointed cell's history scrub is
    /// incomplete or the cell is fenced.
    ///
    /// # Errors
    /// [`CellError::Fenced`] on a fenced cell; [`CellError::Verifying`] while
    /// covered history or receipts remain unverified.
    pub(crate) fn refuse_fork(&self) -> Result<(), CellError> {
        self.refuse_fenced()?;
        if self.covered.is_some() || self.idempotency_unproven {
            return Err(CellError::Verifying(
                "forking while the open checkpoint history scrub is incomplete",
            ));
        }
        Ok(())
    }

    /// An idempotency lookup missed.
    ///
    /// # Errors
    /// [`CellError::Verifying`] while a covered receipt may hide its key.
    pub(crate) fn refuse_unproven_miss(&self) -> Result<(), CellError> {
        self.refuse_fenced()?;
        if self.idempotency_unproven {
            return Err(CellError::Verifying(
                "idempotency keys the open checkpoint covers",
            ));
        }
        Ok(())
    }

    pub(crate) fn advancement(&self) -> Advance {
        if self.fork || self.finding.is_some() || self.run.is_some() || self.covered.is_some() {
            return Advance::Hold;
        }
        self.validated.clone().map_or(Advance::Seed, Advance::From)
    }

    pub(crate) const fn baseline_dv(&self) -> Option<u64> {
        self.baseline_dv
    }

    pub(crate) fn validated(&mut self, checkpoint: OpenCheckpoint, genesis_dv: Option<u64>) {
        self.validated = Some(checkpoint);
        if genesis_dv.is_some() {
            self.baseline_dv = genesis_dv;
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.validated = None;
    }

    /// Start closing the gap from the validated checkpoint (genesis with
    /// none) to `target` off the commit path, pinned to the open/validated
    /// `PRAGMA data_version` baseline so any external write before wake 1 restarts (S10).
    pub(crate) fn catch_up(&mut self, target: LedgerHead, boundary: Option<FormatBoundary>) {
        if self.fork || self.finding.is_some() || self.run.is_some() {
            return;
        }
        self.run = Some(Run::new(
            self.validated.clone(),
            self.validated.clone(),
            target,
            boundary,
            true,
            self.baseline_dv,
        ));
    }

    pub(crate) fn status(&self) -> ScrubStatus {
        if let Some(finding) = &self.finding {
            return ScrubStatus::Fenced {
                finding: finding.clone(),
            };
        }
        self.run
            .as_ref()
            .map_or(ScrubStatus::Idle, |run| run.status(self.covered.is_none()))
    }
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
        self.persist_pending_fence();
        self.sync_durable_fence();
        let Some(mut run) = ({
            let mut scrub = self.scrub.borrow_mut();
            if scrub.finding.is_some() {
                None
            } else {
                scrub.run.take()
            }
        }) else {
            return self.scrub.borrow().status();
        };
        // S11: `scrub_run_step` borrows `&run` immutably; `run` is only
        // mutated after the transaction commits cleanly.
        let outcome = self.conn.transaction(|| self.scrub_run_step(&run));
        let applied = outcome.and_then(|step| apply_step(&mut run, &self.transparency, step));
        match applied {
            Ok(StepProgress::Continue) => {
                let mut scrub = self.scrub.borrow_mut();
                scrub.transient = 0;
                if run.start.is_none() && run.phase != Phase::Receipts {
                    scrub.idempotency_unproven = false;
                } else if run.phase == Phase::Receipts && scrub.covered.is_some() {
                    scrub.covered = Some(run.target.clone());
                    scrub.idempotency_unproven = true;
                }
                scrub.run = Some(run);
            }
            Ok(StepProgress::Done(checkpoint)) => {
                let mut scrub = self.scrub.borrow_mut();
                scrub.transient = 0;
                scrub.covered = None;
                scrub.idempotency_unproven = false;
                if checkpoint.is_some() {
                    scrub.validated = checkpoint;
                }
                if run.start.is_none() && run.data_version.is_some() {
                    scrub.baseline_dv = run.data_version;
                }
                tracing::info!(
                    commits = run.target.commits,
                    "history scrub: verified; checkpoint seeded"
                );
            }
            Err(error)
                if matches!(error, CellError::Engine(_) | CellError::Sqlite(_))
                    && self.scrub.borrow().transient < TRANSIENT_RETRIES =>
            {
                let mut scrub = self.scrub.borrow_mut();
                scrub.transient = scrub.transient.saturating_add(1);
                tracing::warn!(%error, "history scrub: step failed; retrying");
                scrub.run = Some(run);
            }
            Err(error) => {
                self.fence_on_defect(&error);
            }
        }
        self.scrub.borrow().status()
    }

    fn fence_on_defect(&self, error: &CellError) {
        let finding = error.to_string();
        tracing::error!(%finding, "history scrub: the cell is fenced");
        {
            let mut scrub = self.scrub.borrow_mut();
            scrub.finding = Some(finding);
            scrub.fence_persisted = false;
        }
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
        let outcome = match run.phase {
            Phase::Receipts => self.scrub_receipts(run)?,
            Phase::History => self.scrub_history(run)?,
            Phase::QuantityTail => self.scrub_quantity_tail(run)?,
            Phase::Seed => self.scrub_seed(run)?,
        };
        Ok((outcome, dv))
    }

    /// Phase 3: verify tail anchor and O(1) pins inside the seed tx before
    /// recording, folding definitions in bounded pages first if `!run.history` (S10).
    fn scrub_seed(&self, run: &Run) -> Result<StepOutcome, CellError> {
        let Some(pins) = &run.verified_pins else {
            let mut seed_defs = run.seed_defs.clone();
            let verified_pins = seed_defs.step(self, run.target.commits)?;
            return Ok(StepOutcome::SeedPins {
                seed_defs,
                verified_pins,
            });
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
            self.record_scrub_checkpoint(checkpoint)?,
        ))
    }

    /// Phase 1: the receipts after `run.receipts_at`, a page at a time; for
    /// a run from a checkpoint, first the digests it covers, copied from the
    /// loaded log into the rebuilt tree. Purely reads and stages the batch.
    fn scrub_receipts(&self, run: &Run) -> Result<StepOutcome, CellError> {
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
        let page = self
            .verifier()
            .receipts_after(run.receipts_at, RECEIPT_STEP)?;
        let read = page.len();
        let loaded_len = log.len();
        let mut receipts_at = run.receipts_at;
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
        Ok(StepOutcome::ReceiptsBatch {
            leaves,
            log_append,
            receipts_at,
            quantity,
            done,
        })
    }

    /// Phase 2: advance history verification under a shared per-wake row budget
    /// (resumable across wakes by keyset cursor, S10).
    fn scrub_history(&self, run: &Run) -> Result<StepOutcome, CellError> {
        let reader = self.verifier();
        if run.quantity_scrub.has_pending_work() {
            let mut quantity_scrub = run.quantity_scrub.clone();
            quantity_scrub.step_pending(&self.conn, &reader)?;
            return Ok(StepOutcome::QuantityStep {
                quantity_scrub: Box::new(quantity_scrub),
                done: false,
            });
        }
        let mut history_cursor = run.history_cursor.clone();
        let mut quantity_scrub = run.quantity_scrub.clone();
        let (verified_pins, done) = match history_cursor.step(
            self,
            &reader,
            run.start.as_ref(),
            &run.target,
            run.target_boundary,
            &mut quantity_scrub,
        )? {
            history::HistoryStep::Continue => (None, false),
            history::HistoryStep::Done(pins) => (Some(pins), true),
        };
        Ok(StepOutcome::HistoryChunk {
            history_cursor: Box::new(history_cursor),
            quantity_scrub: Box::new(quantity_scrub),
            verified_pins,
            done,
        })
    }

    /// Phase 2b: bounded (`<= 512`-row) tail quantity projection verification (S10).
    fn scrub_quantity_tail(&self, run: &Run) -> Result<StepOutcome, CellError> {
        let reader = self.verifier();
        let mut quantity_scrub = run.quantity_scrub.clone();
        let done = quantity_scrub.step_tail(
            &self.conn,
            &reader,
            run.target.commits,
            run.target_boundary,
        )?;
        Ok(StepOutcome::QuantityStep {
            quantity_scrub: Box::new(quantity_scrub),
            done,
        })
    }
}

enum StepProgress {
    Continue,
    Done(Option<OpenCheckpoint>),
}

/// Apply one committed step's staged progress to `run` (S11).
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
            run.push_leaves(leaves)?;
            Ok(StepProgress::Continue)
        }
        StepOutcome::ReceiptsBatch {
            leaves,
            log_append,
            receipts_at,
            quantity,
            done,
        } => {
            if !log_append.is_empty() {
                let mut log = transparency.borrow_mut();
                for (commit, hash) in log_append {
                    if log.len().checked_add(1) == Some(commit) {
                        log.push(hash);
                    }
                }
            }
            run.push_leaves(leaves)?;
            run.receipts_at = receipts_at;
            run.quantity = Some(quantity);
            if done {
                run.phase = if run.history {
                    Phase::History
                } else {
                    Phase::Seed
                };
            }
            Ok(StepProgress::Continue)
        }
        StepOutcome::HistoryChunk {
            history_cursor,
            quantity_scrub,
            verified_pins,
            done,
        } => {
            run.history_cursor = *history_cursor;
            run.quantity_scrub = *quantity_scrub;
            if done {
                run.verified_pins = verified_pins;
                run.phase = Phase::QuantityTail;
            }
            Ok(StepProgress::Continue)
        }
        StepOutcome::QuantityStep {
            quantity_scrub,
            done,
        } => {
            run.quantity_scrub = *quantity_scrub;
            if done {
                run.phase = Phase::Seed;
            }
            Ok(StepProgress::Continue)
        }
        StepOutcome::SeedPins {
            seed_defs,
            verified_pins,
        } => {
            run.seed_defs = seed_defs;
            run.verified_pins = verified_pins;
            Ok(StepProgress::Continue)
        }
        StepOutcome::Seeded(checkpoint) => Ok(StepProgress::Done(checkpoint)),
    }
}
