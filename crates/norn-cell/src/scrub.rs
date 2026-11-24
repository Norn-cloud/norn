//! OPEN-CHECKPOINT-1 (#1551 round 4): the history scrub.
//!
//! An open from an authentic checkpoint verifies all live state and the
//! history after the checkpoint ([`crate::open_checkpoint`]). The history
//! it covers is verified here, after the open, off the commit path: the
//! same checks a genesis open runs, from genesis to the head the open
//! verified, in bounded steps between commits.
//!
//! # Placement
//!
//! The scrub runs on the cell's own connection, one bounded step at a time,
//! each step in its own transaction ([`CellStore::scrub_step`]). The host
//! drives it from its due-work clock (`norn_api::timers`): the Durable
//! Object's alarm and the dev host's timer loop. A step reads at most
//! [`RECEIPT_STEP`] receipts or verifies [`HISTORY_STEP`] commits, so the
//! longest a commit waits behind it is one step.
//!
//! Each step stages its progress and commits it to the run state only after
//! its transaction succeeds (S11), so a transient engine failure never loses
//! or corrupts accumulated scrub state.
//!
//! # What it verifies, in order
//!
//! 1. **Receipts** through the open's head: each body against its digest
//!    and its lookup columns against its body (an idempotency key or world
//!    hidden by a changed selector, S4), each digest against the one the
//!    open loaded, and the rebuilt RFC 6962 root against the checkpoint's.
//!    It also folds the quantity receipt digest. When it ends, an
//!    idempotency miss proves a first commit again.
//! 2. **History**, in chunks from genesis: the ledger chain (sequence,
//!    links, entry hashes, events in their receipt's commit and world, the
//!    quantity provenance and boundary, logical time; S8), every export
//!    record's cell identity, every definition install with its typed
//!    export record, superseded installs included (S9), and every value
//!    version against its receipt (S7). The last chunk must end at the
//!    open's head with its quantity boundary.
//! 3. **Seed**: inside its own transaction, verifies the target tail anchor
//!    and durable fence (S10, S12), verifies the export and definition pins
//!    still match the verified history, and records the MAC'd checkpoint.
//!
//! Until step 2 ends, history reads a genesis open would have proven
//! complete are refused as retryable `cell.verifying` ([`Gate`]): historical
//! frames and as-of reads (S7), ledger/finding/export/transparency reads
//! (S8), and forks of the cell (S13). Until step 1 ends, any idempotency
//! lookup miss is refused the same way (S4). Live reads and unkeyed writes
//! proceed.
//!
//! A defect fences the cell durably (S12): every commit and host read is
//! refused as `cell.fenced`, the finding is logged and kept for the host
//! ([`ScrubStatus::Fenced`]), the checkpoint row is dropped, and a durable
//! fence marker is recorded in `_norn_meta` so sibling handles and reopens
//! fail closed too.

use crate::engine::SqlEngine;
use crate::open_checkpoint::verify::DefinitionWalk;
use crate::open_checkpoint::{self, ExportPin, Loaded, OpenCheckpoint};
use crate::quantity::FormatBoundary;
use crate::{CellError, CellStore, LedgerHead};

/// The refusal code of a read or write held back by the scrub (retryable).
pub(crate) const VERIFYING: &str = "cell.verifying";
/// The refusal code of a fenced cell.
pub(crate) const FENCED: &str = "cell.fenced";

/// Receipts one receipts step reads.
pub(crate) const RECEIPT_STEP: usize = 512;
/// Commits one history step verifies.
pub(crate) const HISTORY_STEP: u64 = 64;
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
    /// The ledger sequence of the trusted checkpoint, while the history
    /// through it is unverified.
    covered_seq: Option<u64>,
    /// Whether covered receipt lookup columns have not yet been verified (S4).
    idempotency_unproven: bool,
    fenced: bool,
    /// Whether this gate belongs to a store-served reader (`CellStore::read`),
    /// which must also check durable fences written by sibling handles (S12).
    served: bool,
}

impl Gate {
    /// Nothing is held back.
    pub(crate) const OPEN: Self = Self {
        covered_seq: None,
        idempotency_unproven: false,
        fenced: false,
        served: false,
    };

    /// Whether this gate was issued by `CellStore::read()`.
    pub(crate) const fn is_served(self) -> bool {
        self.served
    }

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

    /// A ledger page of the entries after `after` (S8).
    ///
    /// # Errors
    /// [`CellError::Verifying`] when the page reaches an entry the checkpoint
    /// covers and the scrub has not verified; [`CellError::Fenced`].
    pub(crate) fn ledger(self, after: u64) -> Result<(), CellError> {
        self.refuse_fenced()?;
        if self.covered_seq.is_some_and(|seq| after < seq) {
            return Err(CellError::Verifying(
                "ledger pages the open checkpoint covers",
            ));
        }
        Ok(())
    }

    /// An idempotency lookup missed: refuse until receipt scrub proves every
    /// covered receipt's lookup columns (S4).
    ///
    /// # Errors
    /// [`CellError::Verifying`] while receipt scrub is incomplete;
    /// [`CellError::Fenced`] on a fenced cell.
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
    /// The trusted checkpoint's head while the history through it is
    /// unverified (after an open from it).
    covered: Option<LedgerHead>,
    /// Whether an idempotency miss may hide a covered receipt.
    idempotency_unproven: bool,
    /// The checkpoint row this handle validated or wrote.
    validated: Option<OpenCheckpoint>,
    run: Option<Run>,
    finding: Option<String>,
    /// Whether `finding` has been durably written to `_norn_meta` and the
    /// checkpoint row deleted (S12). While false, subsequent calls retry
    /// `fence_durably` until persistence succeeds.
    fence_persisted: bool,
    transient: u32,
    fork: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Receipts,
    History,
    Seed,
}

/// Verified pins captured at the end of Phase 2 (`History`) and re-checked
/// inside Phase 3 (`Seed`)'s transaction before writing the checkpoint (S10).
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedPins {
    export: Option<ExportPin>,
    definitions: String,
}

/// One scrub run: verify from `start` (genesis with none) to `target`, then
/// seed the checkpoint at `target`.
///
/// S10 (#1551 round 5): across stepped transactions, `data_version` tracks
/// [`SqlEngine::external_data_version`] (`PRAGMA data_version` on native
/// SQLite, constant `Some(0)` on exclusive single-connection DO/Rivet
/// engines). If another connection commits between scrub steps, the run
/// restarts from genesis instead of stamping a checkpoint over cached earlier
/// chunks that may have been mutated externally.
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
    ledger: LedgerHead,
    boundary: Option<FormatBoundary>,
    walk: Option<DefinitionWalk>,
    verified_pins: Option<VerifiedPins>,
    data_version: Option<u64>,
    restarts: u32,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Run")
            .field("phase", &self.phase)
            .field("receipts_at", &self.receipts_at)
            .field("ledger", &self.ledger.commits)
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
            ledger: start
                .as_ref()
                .map_or_else(LedgerHead::genesis, |start| start.head.clone()),
            boundary: start.as_ref().and_then(|start| start.boundary),
            quantity: start.as_ref().map(|start| start.quantity_receipts.clone()),
            start,
            anchor,
            target,
            target_boundary,
            history,
            phase: Phase::Receipts,
            tree: crate::transparency::ReceiptLog::new(),
            walk: None,
            verified_pins: None,
            data_version,
            restarts: 0,
        }
    }

    /// Discard cached step state and restart full verification from genesis
    /// after observing an external write between scrub transactions (S10).
    fn reset_from_genesis(&mut self, data_version: Option<u64>) {
        self.start = None;
        self.history = true;
        self.phase = Phase::Receipts;
        self.tree = crate::transparency::ReceiptLog::new();
        self.quantity = None;
        self.receipts_at = 0;
        self.ledger = LedgerHead::genesis();
        self.boundary = None;
        self.walk = None;
        self.verified_pins = None;
        self.data_version = data_version;
        self.restarts = self.restarts.saturating_add(1);
    }

    fn status(&self, history_ready: bool) -> ScrubStatus {
        let (phase, verified) = match self.phase {
            Phase::Receipts => ("receipts", self.receipts_at),
            Phase::History => ("history", self.ledger.commits),
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
    Restart(Option<u64>),
    TreeBatch(Vec<String>),
    ReceiptsBatch {
        leaves: Vec<String>,
        receipts_at: u64,
        quantity: String,
        done: bool,
    },
    HistoryChunk {
        head: LedgerHead,
        boundary: Option<FormatBoundary>,
        walk: DefinitionWalk,
        verified_pins: Option<VerifiedPins>,
        done: bool,
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
        let mut scrub = Self::default();
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

    /// Whether a local fence finding still needs durable persistence (S12).
    pub(crate) const fn needs_fence_persist(&self) -> bool {
        self.finding.is_some() && !self.fence_persisted
    }

    /// The gate the served reader carries.
    pub(crate) fn gate(&self) -> Gate {
        Gate {
            covered_seq: self.covered.as_ref().map(|head| head.seq),
            idempotency_unproven: self.idempotency_unproven,
            fenced: self.finding.is_some(),
            served: true,
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

    pub(crate) fn validated(&mut self, checkpoint: OpenCheckpoint) {
        self.validated = Some(checkpoint);
    }

    pub(crate) fn invalidate(&mut self) {
        self.validated = None;
    }

    /// Start closing the gap from the validated checkpoint (genesis with
    /// none) to `target` off the commit path.
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
            None,
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
    /// Retry persisting a local fence marker that previously failed transiently
    /// (S12, #1551 round 5).
    pub(crate) fn persist_pending_fence(&self) {
        let finding = {
            let scrub = self.scrub.borrow();
            if !scrub.needs_fence_persist() {
                return;
            }
            scrub.finding.clone()
        };
        let Some(finding) = finding else {
            return;
        };
        match open_checkpoint::fence_durably(&self.conn, &finding) {
            Ok(()) => {
                self.scrub.borrow_mut().fence_persisted = true;
            }
            Err(fence_error) => {
                tracing::warn!(%fence_error, "history scrub: durable fence write failed; will retry");
            }
        }
    }

    /// Observe any durable fence recorded by a sibling handle (S12).
    fn sync_durable_fence(&self) {
        if self.scrub.borrow().finding.is_some() {
            return;
        }
        if let Ok(Some(finding)) = open_checkpoint::read_fence(&self.conn) {
            let mut scrub = self.scrub.borrow_mut();
            scrub.finding = Some(finding);
            scrub.fence_persisted = true;
            scrub.run = None;
        }
    }

    /// Where the history scrub stands (OPEN-CHECKPOINT-1, #1551 round 4).
    #[must_use]
    pub fn scrub_status(&self) -> ScrubStatus {
        self.sync_durable_fence();
        self.scrub.borrow().status()
    }

    /// Whether the scrub has work left: the host runs [`Self::scrub_step`]
    /// again at once (its due-work clock reports the cell due now).
    #[must_use]
    pub fn scrub_pending(&self) -> bool {
        matches!(self.scrub_status(), ScrubStatus::Verifying { .. })
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
        let applied = outcome.and_then(|step| apply_step(&mut run, step));
        match applied {
            Ok(StepProgress::Continue) => {
                let mut scrub = self.scrub.borrow_mut();
                scrub.transient = 0;
                if run.start.is_none() && run.phase != Phase::Receipts {
                    scrub.idempotency_unproven = false;
                } else if run.phase == Phase::Receipts && scrub.covered.is_some() {
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
                "history scrub: external write detected between transactions; restarting from genesis"
            );
            return Ok((StepOutcome::Restart(dv), dv));
        }
        let outcome = match run.phase {
            Phase::Receipts => self.scrub_receipts(run)?,
            Phase::History => self.scrub_history(run)?,
            Phase::Seed => self.scrub_seed(run)?,
        };
        Ok((outcome, dv))
    }

    /// Phase 3: verify the tail anchor and verified pins inside the seed
    /// transaction before recording the checkpoint (S10).
    fn scrub_seed(&self, run: &Run) -> Result<StepOutcome, CellError> {
        let expected_hash = run
            .target
            .commits
            .checked_sub(1)
            .and_then(|idx| usize::try_from(idx).ok())
            .and_then(|idx| run.tree.get(idx))
            .map(String::as_str);
        self.verify_target_tail_anchor(&run.target, expected_hash)?;
        let quantity = match &run.quantity {
            Some(quantity) => quantity.clone(),
            None => open_checkpoint::quantity_seed()?,
        };
        let checkpoint = self.checkpoint_at(
            &run.target,
            run.target_boundary,
            open_checkpoint::root_hex(run.tree.root().as_bytes()),
            quantity,
        )?;
        if let Some(pins) = &run.verified_pins
            && (checkpoint.export != pins.export || checkpoint.definitions != pins.definitions)
        {
            return Err(defect(
                run.target.commits,
                "export or definition pins changed before checkpoint seed",
            ));
        }
        // S10: never regress a newer authentic checkpoint written by another
        // handle while this scrub ran, while still replacing a stale
        // checkpoint past the database's end (as after a trimmed restore).
        let db_commits: i64 = self.conn.query_row(
            &format!(
                "SELECT COALESCE(MAX(\"commit_seq\"), 0) FROM \"{}\"",
                crate::system::RECEIPT
            ),
            &[],
            |row| row.get(0),
        )?;
        let db_commits = crate::read::to_u64(db_commits, "receipt head")?;
        if let Loaded::Authentic(existing) = open_checkpoint::load(&self.conn)?
            && existing.head.commits <= db_commits
            && existing.head.commits >= checkpoint.head.commits
        {
            return Ok(StepOutcome::Seeded(Some(existing)));
        }
        let recorded = open_checkpoint::record(&self.conn, &checkpoint)?;
        Ok(StepOutcome::Seeded(recorded.then_some(checkpoint)))
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
        let mut receipts_at = run.receipts_at;
        let mut leaves = Vec::with_capacity(read);
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
            let loaded = usize::try_from(commit.saturating_sub(1))
                .ok()
                .and_then(|index| log.get(index));
            if loaded != Some(&receipt.hash) {
                return Err(defect(
                    commit,
                    "a stored receipt digest is not the one the open loaded",
                ));
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
            receipts_at,
            quantity,
            done,
        })
    }

    /// Phase 2: the next [`HISTORY_STEP`] commits, as a genesis open
    /// verifies them. Stages progress into [`StepOutcome::HistoryChunk`].
    fn scrub_history(&self, run: &Run) -> Result<StepOutcome, CellError> {
        let reader = self.verifier();
        let versions = reader.installed_definition_versions()?;
        let mut walk = match &run.walk {
            Some(walk) => walk.clone(),
            None => match &run.start {
                // A catch-up from a checkpoint re-runs the pins an open runs
                // on it (S3): its export record, its definition versions.
                Some(start) => {
                    crate::cell_identity::verify_export_pin(&self.conn, start)?;
                    self.definitions_at_checkpoint(&reader, &versions, start)?
                }
                None => DefinitionWalk::default(),
            },
        };
        let after = run.ledger.commits;
        let through = run.target.commits.min(after.saturating_add(HISTORY_STEP));
        let (head, boundary) = reader.verify_ledger_span(&run.ledger, run.boundary, through)?;
        if head.commits != through {
            return Err(defect(
                head.commits,
                "the ledger ends before the head the open verified",
            ));
        }
        crate::cell_identity::verify_records_between(&self.conn, after, through)?;
        for receipt in reader.receipt_stream_from(after) {
            let receipt = receipt?;
            if receipt.body.commit_seq > through {
                break;
            }
            self.definition_walk_step(&reader, &versions, &mut walk, &receipt)?;
        }
        reader.verify_value_history_range(after, through)?;
        let done = through == run.target.commits;
        let verified_pins = if done {
            if head != run.target || boundary != run.target_boundary {
                return Err(defect(
                    through,
                    "the verified history does not end at the head the open verified",
                ));
            }
            let installed = versions
                .iter()
                .filter(|version| version.commit_seq <= through)
                .count();
            if walk.version_index != installed {
                return Err(defect(
                    through,
                    "the definition history and the receipts disagree",
                ));
            }
            Some(VerifiedPins {
                export: crate::cell_identity::last_record_through(&self.conn, through)?,
                definitions: open_checkpoint::definitions_digest(&versions, through)?,
            })
        } else {
            None
        };
        Ok(StepOutcome::HistoryChunk {
            head,
            boundary,
            walk,
            verified_pins,
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
    (step, data_version): (StepOutcome, Option<u64>),
) -> Result<StepProgress, CellError> {
    if let StepOutcome::Restart(dv) = step {
        run.reset_from_genesis(dv);
        return Ok(StepProgress::Continue);
    }
    if run.data_version.is_none() {
        run.data_version = data_version;
    }
    match step {
        StepOutcome::Restart(_) => Ok(StepProgress::Continue),
        StepOutcome::TreeBatch(leaves) => {
            run.push_leaves(leaves)?;
            Ok(StepProgress::Continue)
        }
        StepOutcome::ReceiptsBatch {
            leaves,
            receipts_at,
            quantity,
            done,
        } => {
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
            head,
            boundary,
            walk,
            verified_pins,
            done,
        } => {
            run.ledger = head;
            run.boundary = boundary;
            run.walk = Some(walk);
            if done {
                run.verified_pins = verified_pins;
                run.phase = Phase::Seed;
            }
            Ok(StepProgress::Continue)
        }
        StepOutcome::Seeded(checkpoint) => Ok(StepProgress::Done(checkpoint)),
    }
}
