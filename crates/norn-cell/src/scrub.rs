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
//! A separate read connection over a WAL snapshot was the alternative. A
//! Durable Object has one storage connection and no threads, and an
//! in-memory cell cannot be opened twice, so it would need a second
//! implementation for those hosts; one stepped scrub serves all of them.
//! The cost is that a step and a commit never overlap: a step is a pause in
//! commits, bounded by the step size (measured on #1551).
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
//! 3. **Seed**: the checkpoint at that head, built only from what the scrub
//!    verified (its rebuilt root, its quantity digest), MAC'd and recorded.
//!
//! Until step 2 ends, the history reads a genesis open would have proven
//! complete are refused as retryable `cell.verifying` ([`Gate`]): as-of
//! reads and version lists (metric compute over history), and ledger pages
//! over the covered range. Until step 1 ends, a commit whose idempotency key
//! finds no receipt is refused the same way (a miss cannot be dated). Live
//! reads and other writes proceed.
//!
//! A defect fences the cell: every commit and gated read is refused as
//! `cell.fenced`, the finding is logged and kept for the host
//! ([`ScrubStatus::Fenced`]), and the checkpoint row is dropped, so the next
//! open verifies from genesis and refuses the same defect.
//!
//! The scrub also closes long checkpoint gaps. A commit advances the
//! checkpoint itself only over [`crate::open_checkpoint::MAX_ADVANCE`]
//! commits from one this handle validated; a longer gap (a seed recorded
//! behind a busy head, a trimmed restore, a genesis open) is verified here
//! instead, from that checkpoint or from genesis, and seeds the next one.
//! So no commit runs a catch-up (S10).
use crate::engine::SqlEngine;
use crate::open_checkpoint::{self, OpenCheckpoint};
use crate::quantity::FormatBoundary;
use crate::store::DefinitionWalk;
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

/// Which history reads wait for the scrub; copied into every reader the
/// store serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gate {
    /// The ledger sequence of the trusted checkpoint, while the history
    /// through it is unverified.
    covered_seq: Option<u64>,
    fenced: bool,
}

impl Gate {
    /// Nothing is held back.
    pub(crate) const OPEN: Self = Self {
        covered_seq: None,
        fenced: false,
    };

    /// A read that needs the whole history complete.
    ///
    /// # Errors
    /// [`CellError::Verifying`] while the scrub runs; [`CellError::Fenced`].
    pub(crate) fn history(self, what: &'static str) -> Result<(), CellError> {
        if self.fenced {
            return Err(fenced());
        }
        if self.covered_seq.is_some() {
            return Err(CellError::Verifying(what));
        }
        Ok(())
    }

    /// A ledger page of the entries after `after`.
    ///
    /// # Errors
    /// [`CellError::Verifying`] when the page reaches an entry the checkpoint
    /// covers and the scrub has not verified; [`CellError::Fenced`].
    pub(crate) fn ledger(self, after: u64) -> Result<(), CellError> {
        if self.fenced {
            return Err(fenced());
        }
        if self.covered_seq.is_some_and(|seq| after < seq) {
            return Err(CellError::Verifying("ledger pages the open checkpoint covers"));
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
    transient: u32,
    fork: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Receipts,
    History,
    Seed,
}

/// One scrub run: verify from `start` (genesis with none) to `target`, then
/// seed the checkpoint at `target`.
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
        }
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
}

/// What one step did.
enum Stepped {
    Continue,
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
            ));
            scrub.validated = Some(checkpoint);
        } else if let Some(checkpoint) = recorded {
            scrub.validated = Some(checkpoint);
        } else if head.commits > open_checkpoint::MAX_ADVANCE {
            // A shorter history is checkpointed by the next commit that is
            // due one, from genesis, within the commit's bound.
            scrub.run = Some(Run::new(None, None, head.clone(), boundary, false));
        }
        scrub
    }

    /// A fork's: it holds back what its base holds back and never runs a
    /// scrub or records a checkpoint of its own.
    pub(crate) fn for_fork(&self) -> Self {
        Self {
            covered: self.covered.clone(),
            idempotency_unproven: self.idempotency_unproven,
            finding: self.finding.clone(),
            fork: true,
            ..Self::default()
        }
    }

    /// The gate the served reader carries.
    pub(crate) fn gate(&self) -> Gate {
        Gate {
            covered_seq: self.covered.as_ref().map(|head| head.seq),
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
        ));
    }

    pub(crate) fn status(&self) -> ScrubStatus {
        if let Some(finding) = &self.finding {
            return ScrubStatus::Fenced {
                finding: finding.clone(),
            };
        }
        self.run.as_ref().map_or(ScrubStatus::Idle, |run| {
            run.status(self.covered.is_none())
        })
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Where the history scrub stands (OPEN-CHECKPOINT-1, #1551 round 4).
    #[must_use]
    pub fn scrub_status(&self) -> ScrubStatus {
        self.scrub.borrow().status()
    }

    /// Whether the scrub has work left: the host runs [`Self::scrub_step`]
    /// again at once (its due-work clock reports the cell due now).
    #[must_use]
    pub fn scrub_pending(&self) -> bool {
        matches!(self.scrub_status(), ScrubStatus::Verifying { .. })
    }

    /// Run one bounded step of the history scrub in its own transaction,
    /// between commits. A defect fences the cell; the returned status says
    /// so and carries the finding.
    pub fn scrub_step(&self) -> ScrubStatus {
        let Some(mut run) = ({
            let mut scrub = self.scrub.borrow_mut();
            if scrub.finding.is_some() {
                None
            } else {
                scrub.run.take()
            }
        }) else {
            return self.scrub_status();
        };
        let stepped = self.conn.transaction(|| self.scrub_run_step(&mut run));
        let mut scrub = self.scrub.borrow_mut();
        match stepped {
            Ok(Stepped::Continue) => {
                scrub.transient = 0;
                if run.start.is_none() && run.phase != Phase::Receipts {
                    scrub.idempotency_unproven = false;
                }
                scrub.run = Some(run);
            }
            Ok(Stepped::Seeded(checkpoint)) => {
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
                    && scrub.transient < TRANSIENT_RETRIES =>
            {
                scrub.transient = scrub.transient.saturating_add(1);
                tracing::warn!(%error, "history scrub: step failed; retrying");
                scrub.run = Some(run);
            }
            Err(error) => {
                let finding = error.to_string();
                tracing::error!(%finding, "history scrub: the cell is fenced");
                scrub.finding = Some(finding);
                drop(scrub);
                // The durable half: the next open verifies from genesis.
                if let Err(error) = self
                    .conn
                    .transaction(|| open_checkpoint::drop_row(&self.conn))
                {
                    tracing::warn!(%error, "history scrub: the checkpoint row stays");
                }
                return self.scrub_status();
            }
        }
        drop(scrub);
        self.scrub_status()
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

    fn scrub_run_step(&self, run: &mut Run) -> Result<Stepped, CellError> {
        match run.phase {
            Phase::Receipts => self.scrub_receipts(run).map(|()| Stepped::Continue),
            Phase::History => self.scrub_history(run).map(|()| Stepped::Continue),
            Phase::Seed => {
                let quantity = match run.quantity.clone() {
                    Some(quantity) => quantity,
                    None => open_checkpoint::quantity_seed()?,
                };
                let checkpoint = self.checkpoint_at(
                    &run.target,
                    run.target_boundary,
                    open_checkpoint::root_hex(run.tree.root().as_bytes()),
                    quantity,
                )?;
                let recorded = open_checkpoint::record(&self.conn, &checkpoint)?;
                Ok(Stepped::Seeded(recorded.then_some(checkpoint)))
            }
        }
    }

    /// Phase 1: the receipts after `run.receipts_at`, a page at a time; for
    /// a run from a checkpoint, first the digests it covers, copied from the
    /// loaded log into the rebuilt tree.
    fn scrub_receipts(&self, run: &mut Run) -> Result<(), CellError> {
        let log = self.transparency.borrow();
        let start = run.start.as_ref().map_or(0, |start| start.head.commits);
        if run.tree.len() < start {
            let to = start.min(run.tree.len().saturating_add(TREE_STEP));
            while run.tree.len() < to {
                let leaf = usize::try_from(run.tree.len())
                    .ok()
                    .and_then(|index| log.get(index))
                    .ok_or_else(|| defect(run.tree.len(), "the loaded receipt log is short"))?;
                run.tree.push(leaf.clone());
            }
            return run.check_anchor();
        }
        let mut quantity = match run.quantity.take() {
            Some(quantity) => quantity,
            None => open_checkpoint::quantity_seed()?,
        };
        let page = self.verifier().receipts_after(run.receipts_at, RECEIPT_STEP)?;
        let read = page.len();
        for receipt in page {
            let commit = receipt.body.commit_seq;
            if commit > run.target.commits {
                break;
            }
            let expected = run
                .receipts_at
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
            run.tree.push(receipt.hash.clone());
            if receipt.body.quantity.is_some() {
                quantity = open_checkpoint::quantity_extend(&quantity, commit, &receipt.hash)?;
            }
            run.receipts_at = commit;
            run.check_anchor()?;
        }
        run.quantity = Some(quantity);
        if run.receipts_at == run.target.commits {
            run.phase = if run.history {
                Phase::History
            } else {
                Phase::Seed
            };
        } else if read == 0 {
            return Err(defect(
                run.receipts_at,
                "the history holds fewer receipts than the head the open verified",
            ));
        }
        Ok(())
    }

    /// Phase 2: the next [`HISTORY_STEP`] commits, as a genesis open
    /// verifies them.
    fn scrub_history(&self, run: &mut Run) -> Result<(), CellError> {
        let reader = self.verifier();
        let versions = reader.installed_definition_versions()?;
        let mut walk = match run.walk.take() {
            Some(walk) => walk,
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
        let through = run
            .target
            .commits
            .min(after.saturating_add(HISTORY_STEP));
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
        run.ledger = head;
        run.boundary = boundary;
        if through == run.target.commits {
            if run.ledger != run.target || run.boundary != run.target_boundary {
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
            run.phase = Phase::Seed;
        }
        run.walk = Some(walk);
        Ok(())
    }
}
