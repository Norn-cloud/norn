//! Closed handle-level lifecycle state for the post-open history scrub (`D5`, OPEN-CHECKPOINT-1).
//!
//! Replaces parallel `Option`/boolean fields on [`Gate`] and [`Scrub`] with
//! closed enums so impossible combinations (such as unproven idempotency
//! without a covered prefix, persisted fence without a finding, or a fork
//! running a scrub) are unrepresentable at compile time.

use super::{PostReceipts, Run, fenced};
use crate::open_checkpoint::{self, OpenCheckpoint};
use crate::quantity::FormatBoundary;
use crate::{CellError, LedgerHead};

/// Consecutive engine failures a step retries before it fences the cell.
const TRANSIENT_RETRIES: u32 = 3;

/// Which history reads wait for the scrub; copied into every reader the
/// store serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Nothing is held back.
    Open,
    /// Open-checkpoint history scrub is verifying the prefix through `covered_seq`.
    Verifying {
        /// Highest ledger event sequence covered by the unverified prefix.
        covered_seq: u64,
        /// Whether covered receipt lookup columns are still unproven (Phase 1, S4).
        idempotency_unproven: bool,
    },
    /// The cell has been fenced by the history scrub (S12).
    Fenced,
}

impl Gate {
    /// Nothing is held back.
    pub(crate) const OPEN: Self = Self::Open;

    /// Refuse if the cell has been fenced by the history scrub (S12).
    ///
    /// # Errors
    /// [`CellError::Fenced`] on a fenced cell.
    pub(crate) fn refuse_fenced(self) -> Result<(), CellError> {
        match self {
            Self::Fenced => Err(fenced()),
            Self::Open | Self::Verifying { .. } => Ok(()),
        }
    }

    /// A read that needs the whole history complete (S7, S8).
    ///
    /// # Errors
    /// [`CellError::Verifying`] while the scrub runs; [`CellError::Fenced`].
    pub(crate) fn history(self, what: &'static str) -> Result<(), CellError> {
        match self {
            Self::Fenced => Err(fenced()),
            Self::Verifying { .. } => Err(CellError::Verifying(what)),
            Self::Open => Ok(()),
        }
    }

    /// A ledger page after `after` (S8).
    ///
    /// # Errors
    /// [`CellError::Verifying`] or [`CellError::Fenced`].
    pub(crate) fn ledger(self, after: u64) -> Result<(), CellError> {
        match self {
            Self::Fenced => Err(fenced()),
            Self::Verifying { covered_seq, .. } if after < covered_seq => Err(
                CellError::Verifying("ledger pages the open checkpoint covers"),
            ),
            Self::Open | Self::Verifying { .. } => Ok(()),
        }
    }

    /// An idempotency lookup missed before receipt scrub completed (S4).
    ///
    /// # Errors
    /// [`CellError::Verifying`] or [`CellError::Fenced`].
    pub(crate) fn receipt_miss(self) -> Result<(), CellError> {
        match self {
            Self::Fenced => Err(fenced()),
            Self::Verifying {
                idempotency_unproven: true,
                ..
            } => Err(CellError::Verifying(
                "idempotency keys the open checkpoint covers",
            )),
            Self::Open
            | Self::Verifying {
                idempotency_unproven: false,
                ..
            } => Ok(()),
        }
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

/// Whether a local fence finding has been durably persisted to `_norn_meta` (S12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FenceDurability {
    PendingPersist,
    Persisted,
}

/// Which reads are held back while a scrub run is active.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScrubCoverage {
    /// History is already verified (genesis seed-only or catch-up run).
    VerifiedHistory,
    /// Open started from a checkpoint whose covered prefix is still unverified.
    UnverifiedPrefix {
        covered: LedgerHead,
        /// True while Phase 1 (`Receipts`) has not yet proven covered receipt
        /// lookup columns (S4).
        idempotency_unproven: bool,
    },
}

impl ScrubCoverage {
    const fn history_ready(&self) -> bool {
        matches!(self, Self::VerifiedHistory)
    }

    const fn gate(&self) -> Gate {
        match self {
            Self::VerifiedHistory => Gate::Open,
            Self::UnverifiedPrefix {
                covered,
                idempotency_unproven,
            } => Gate::Verifying {
                covered_seq: covered.seq,
                idempotency_unproven: *idempotency_unproven,
            },
        }
    }
}

#[derive(Debug)]
struct ActiveScrub {
    run: Box<Run>,
    coverage: ScrubCoverage,
    validated: Option<Box<OpenCheckpoint>>,
    transient: u32,
}

#[derive(Debug)]
struct SteppingScrub {
    coverage: ScrubCoverage,
    validated: Option<Box<OpenCheckpoint>>,
    transient: u32,
}

/// Closed lifecycle state of [`Scrub`] on one store handle (`D5`).
#[derive(Debug)]
enum ScrubState {
    /// Unfenced main handle with all served history verified.
    Idle {
        validated: Option<Box<OpenCheckpoint>>,
    },
    /// Unfenced main handle with an active scrub run awaiting its next step.
    Running(Box<ActiveScrub>),
    /// Unfenced main handle while `scrub_step` executes a step transaction.
    Stepping(Box<SteppingScrub>),
    /// Unfenced fork handle: never runs a scrub or advances checkpoints.
    Fork,
    /// Fenced handle (main or fork): refuses commits and history reads (S12).
    Fenced {
        finding: String,
        durability: FenceDurability,
    },
}

/// The scrub's state on one store handle.
#[derive(Debug)]
pub(crate) struct Scrub {
    state: ScrubState,
    /// Last `PRAGMA data_version` at which no sibling durable fence existed (S12).
    fence_checked_dv: Option<u64>,
    /// `PRAGMA data_version` at open or last verified checkpoint (S10 catch-up baseline).
    baseline_dv: Option<u64>,
}

impl Default for Scrub {
    fn default() -> Self {
        Self {
            state: ScrubState::Idle { validated: None },
            fence_checked_dv: None,
            baseline_dv: None,
        }
    }
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
        let state = if !keyed {
            ScrubState::Idle { validated: None }
        } else if let Some(checkpoint) = trusted {
            let covered = checkpoint.head.clone();
            let run = Run::new(
                None,
                Some(checkpoint.clone()),
                head.clone(),
                boundary,
                PostReceipts::VerifyHistory,
                data_version,
            );
            ScrubState::Running(Box::new(ActiveScrub {
                run: Box::new(run),
                coverage: ScrubCoverage::UnverifiedPrefix {
                    covered,
                    idempotency_unproven: true,
                },
                validated: Some(Box::new(checkpoint)),
                transient: 0,
            }))
        } else if let Some(checkpoint) = recorded {
            ScrubState::Idle {
                validated: Some(Box::new(checkpoint)),
            }
        } else if head.commits > open_checkpoint::MAX_ADVANCE {
            // A shorter history is checkpointed by the next commit that is
            // due one, from genesis, within the commit's bound.
            let run = Run::new(
                None,
                None,
                head.clone(),
                boundary,
                PostReceipts::SeedOnly,
                data_version,
            );
            ScrubState::Running(Box::new(ActiveScrub {
                run: Box::new(run),
                coverage: ScrubCoverage::VerifiedHistory,
                validated: None,
                transient: 0,
            }))
        } else {
            ScrubState::Idle { validated: None }
        };
        Self {
            state,
            fence_checked_dv: data_version,
            baseline_dv: data_version,
        }
    }

    /// A fork's scrub state: only created from an unfenced, scrub-verified
    /// parent (S13), and never runs a scrub or records a checkpoint of its own.
    pub(crate) fn for_fork(&self) -> Self {
        let state = match &self.state {
            ScrubState::Fenced {
                finding,
                durability,
            } => ScrubState::Fenced {
                finding: finding.clone(),
                durability: *durability,
            },
            ScrubState::Idle { .. }
            | ScrubState::Running(_)
            | ScrubState::Stepping(_)
            | ScrubState::Fork => ScrubState::Fork,
        };
        Self {
            state,
            fence_checked_dv: None,
            baseline_dv: None,
        }
    }

    pub(crate) fn pending_fence_finding(&self) -> Option<String> {
        match &self.state {
            ScrubState::Fenced {
                finding,
                durability: FenceDurability::PendingPersist,
            } => Some(finding.clone()),
            ScrubState::Fenced {
                durability: FenceDurability::Persisted,
                ..
            }
            | ScrubState::Idle { .. }
            | ScrubState::Running(_)
            | ScrubState::Stepping(_)
            | ScrubState::Fork => None,
        }
    }

    pub(crate) fn mark_fence_persisted(&mut self) {
        if let ScrubState::Fenced { durability, .. } = &mut self.state {
            *durability = FenceDurability::Persisted;
        }
    }

    pub(crate) const fn is_fenced(&self) -> bool {
        matches!(self.state, ScrubState::Fenced { .. })
    }

    pub(crate) const fn needs_fence_persist(&self) -> bool {
        matches!(
            self.state,
            ScrubState::Fenced {
                durability: FenceDurability::PendingPersist,
                ..
            }
        )
    }

    pub(crate) const fn fence_checked_dv(&self) -> Option<u64> {
        self.fence_checked_dv
    }

    pub(crate) const fn set_fence_checked_dv(&mut self, dv: u64) {
        self.fence_checked_dv = Some(dv);
    }

    pub(crate) fn record_durable_fence(&mut self, finding: String) {
        self.state = ScrubState::Fenced {
            finding,
            durability: FenceDurability::Persisted,
        };
    }

    pub(super) fn fence_pending(&mut self, finding: String) {
        if matches!(
            self.state,
            ScrubState::Fenced {
                durability: FenceDurability::Persisted,
                ..
            }
        ) {
            return;
        }
        self.state = ScrubState::Fenced {
            finding,
            durability: FenceDurability::PendingPersist,
        };
    }

    /// The gate the served reader carries.
    pub(crate) fn gate(&self) -> Gate {
        match &self.state {
            ScrubState::Fenced { .. } => Gate::Fenced,
            ScrubState::Running(active) => active.coverage.gate(),
            ScrubState::Stepping(stepping) => stepping.coverage.gate(),
            ScrubState::Idle { .. } | ScrubState::Fork => Gate::Open,
        }
    }

    /// # Errors
    /// [`CellError::Fenced`] on a fenced cell.
    pub(crate) fn refuse_fenced(&self) -> Result<(), CellError> {
        match &self.state {
            ScrubState::Fenced { finding, .. } => Err(CellError::Fenced(finding.clone())),
            ScrubState::Idle { .. }
            | ScrubState::Running(_)
            | ScrubState::Stepping(_)
            | ScrubState::Fork => Ok(()),
        }
    }

    /// S13: refuse to fork while a checkpointed cell's history scrub is
    /// incomplete or the cell is fenced.
    ///
    /// # Errors
    /// [`CellError::Fenced`] on a fenced cell; [`CellError::Verifying`] while
    /// covered history or receipts remain unverified.
    pub(crate) fn refuse_fork(&self) -> Result<(), CellError> {
        self.refuse_fenced()?;
        let unverified = match &self.state {
            ScrubState::Running(active) => !active.coverage.history_ready(),
            ScrubState::Stepping(stepping) => !stepping.coverage.history_ready(),
            ScrubState::Idle { .. } | ScrubState::Fork | ScrubState::Fenced { .. } => false,
        };
        if unverified {
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
        let unproven = match &self.state {
            ScrubState::Running(active) => matches!(
                active.coverage,
                ScrubCoverage::UnverifiedPrefix {
                    idempotency_unproven: true,
                    ..
                }
            ),
            ScrubState::Stepping(stepping) => matches!(
                stepping.coverage,
                ScrubCoverage::UnverifiedPrefix {
                    idempotency_unproven: true,
                    ..
                }
            ),
            ScrubState::Idle { .. } | ScrubState::Fork | ScrubState::Fenced { .. } => false,
        };
        if unproven {
            return Err(CellError::Verifying(
                "idempotency keys the open checkpoint covers",
            ));
        }
        Ok(())
    }

    pub(crate) fn advancement(&self) -> Advance {
        match &self.state {
            ScrubState::Idle { validated } => validated
                .as_deref()
                .cloned()
                .map_or(Advance::Seed, Advance::From),
            ScrubState::Running(_)
            | ScrubState::Stepping(_)
            | ScrubState::Fork
            | ScrubState::Fenced { .. } => Advance::Hold,
        }
    }

    pub(crate) const fn baseline_dv(&self) -> Option<u64> {
        self.baseline_dv
    }

    pub(crate) fn validated(&mut self, checkpoint: OpenCheckpoint, genesis_dv: Option<u64>) {
        let boxed = Some(Box::new(checkpoint));
        match &mut self.state {
            ScrubState::Idle { validated } => *validated = boxed,
            ScrubState::Running(active) => active.validated = boxed,
            ScrubState::Stepping(stepping) => stepping.validated = boxed,
            ScrubState::Fork | ScrubState::Fenced { .. } => {}
        }
        if genesis_dv.is_some() {
            self.baseline_dv = genesis_dv;
        }
    }

    pub(crate) fn invalidate(&mut self) {
        match &mut self.state {
            ScrubState::Idle { validated } => *validated = None,
            ScrubState::Running(active) => active.validated = None,
            ScrubState::Stepping(stepping) => stepping.validated = None,
            ScrubState::Fork | ScrubState::Fenced { .. } => {}
        }
    }

    /// Start closing the gap from the validated checkpoint (genesis with
    /// none) to `target` off the commit path, pinned to the open/validated
    /// `PRAGMA data_version` baseline so any external write before wake 1 restarts (S10).
    pub(crate) fn catch_up(&mut self, target: LedgerHead, boundary: Option<FormatBoundary>) {
        if !matches!(self.state, ScrubState::Idle { .. }) {
            return;
        }
        let ScrubState::Idle { validated } =
            std::mem::replace(&mut self.state, ScrubState::Idle { validated: None })
        else {
            return;
        };
        let run = Run::new(
            validated.as_deref().cloned(),
            validated.as_deref().cloned(),
            target,
            boundary,
            PostReceipts::VerifyHistory,
            self.baseline_dv,
        );
        self.state = ScrubState::Running(Box::new(ActiveScrub {
            run: Box::new(run),
            coverage: ScrubCoverage::VerifiedHistory,
            validated,
            transient: 0,
        }));
    }

    pub(crate) fn status(&self) -> ScrubStatus {
        match &self.state {
            ScrubState::Fenced { finding, .. } => ScrubStatus::Fenced {
                finding: finding.clone(),
            },
            ScrubState::Running(active) => active.run.status(active.coverage.history_ready()),
            ScrubState::Idle { .. } | ScrubState::Stepping(_) | ScrubState::Fork => {
                ScrubStatus::Idle
            }
        }
    }

    /// Take the active [`Run`] out for one transaction step while preserving
    /// coverage and advancement hold in [`ScrubState::Stepping`].
    pub(super) fn begin_step(&mut self) -> Option<Box<Run>> {
        if !matches!(self.state, ScrubState::Running(_)) {
            return None;
        }
        let ScrubState::Running(active) =
            std::mem::replace(&mut self.state, ScrubState::Idle { validated: None })
        else {
            return None;
        };
        let ActiveScrub {
            run,
            coverage,
            validated,
            transient,
        } = *active;
        self.state = ScrubState::Stepping(Box::new(SteppingScrub {
            coverage,
            validated,
            transient,
        }));
        Some(run)
    }

    /// Reinstall `run` after a step committed cleanly and has more work.
    pub(super) fn finish_step_continue(&mut self, run: Box<Run>) {
        if !matches!(self.state, ScrubState::Stepping(_)) {
            return;
        }
        let ScrubState::Stepping(stepping) =
            std::mem::replace(&mut self.state, ScrubState::Idle { validated: None })
        else {
            return;
        };
        let SteppingScrub {
            mut coverage,
            validated,
            ..
        } = *stepping;
        if let ScrubCoverage::UnverifiedPrefix {
            covered,
            idempotency_unproven,
        } = &mut coverage
        {
            if run.start.is_none() && !run.is_in_receipts() {
                *idempotency_unproven = false;
            } else if run.is_in_receipts() {
                *covered = run.target.clone();
                *idempotency_unproven = true;
            }
        }
        self.state = ScrubState::Running(Box::new(ActiveScrub {
            run,
            coverage,
            validated,
            transient: 0,
        }));
    }

    /// Complete the active scrub run and transition to [`ScrubState::Idle`].
    pub(super) fn finish_step_done(&mut self, run: &Run, checkpoint: Option<Box<OpenCheckpoint>>) {
        if !matches!(self.state, ScrubState::Stepping(_)) {
            return;
        }
        let ScrubState::Stepping(stepping) =
            std::mem::replace(&mut self.state, ScrubState::Idle { validated: None })
        else {
            return;
        };
        if run.start.is_none() && run.data_version.is_some() {
            self.baseline_dv = run.data_version;
        }
        self.state = ScrubState::Idle {
            validated: checkpoint.or(stepping.validated),
        };
    }

    /// Retry a transient engine failure if retry budget remains; returns `false`
    /// once retries are exhausted so the caller fences the cell fail-closed.
    pub(super) fn try_retry_transient(&mut self, run: Box<Run>) -> bool {
        let can_retry = matches!(
            &self.state,
            ScrubState::Stepping(stepping) if stepping.transient < TRANSIENT_RETRIES
        );
        if !can_retry {
            return false;
        }
        let ScrubState::Stepping(stepping) =
            std::mem::replace(&mut self.state, ScrubState::Idle { validated: None })
        else {
            return false;
        };
        let SteppingScrub {
            coverage,
            validated,
            transient,
        } = *stepping;
        self.state = ScrubState::Running(Box::new(ActiveScrub {
            run,
            coverage,
            validated,
            transient: transient.saturating_add(1),
        }));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Advance, Gate, Scrub, ScrubStatus, TRANSIENT_RETRIES};
    use crate::open_checkpoint::OpenCheckpoint;
    use crate::scrub::{RunPhase, history};
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

    #[test]
    fn gate_enforces_closed_refusal_rules() -> Result<(), CellError> {
        Gate::OPEN.refuse_fenced()?;
        Gate::OPEN.history("h")?;
        Gate::OPEN.ledger(0)?;
        Gate::OPEN.receipt_miss()?;

        let phase1 = Gate::Verifying {
            covered_seq: 10,
            idempotency_unproven: true,
        };
        phase1.refuse_fenced()?;
        assert!(matches!(phase1.history("h"), Err(CellError::Verifying(_))));
        assert!(matches!(phase1.ledger(9), Err(CellError::Verifying(_))));
        phase1.ledger(10)?;
        assert!(matches!(
            phase1.receipt_miss(),
            Err(CellError::Verifying(_))
        ));

        let phase2 = Gate::Verifying {
            covered_seq: 10,
            idempotency_unproven: false,
        };
        assert!(matches!(phase2.history("h"), Err(CellError::Verifying(_))));
        phase2.receipt_miss()?;

        assert!(matches!(
            Gate::Fenced.refuse_fenced(),
            Err(CellError::Fenced(_))
        ));
        assert!(matches!(
            Gate::Fenced.history("h"),
            Err(CellError::Fenced(_))
        ));
        assert!(matches!(
            Gate::Fenced.ledger(100),
            Err(CellError::Fenced(_))
        ));
        assert!(matches!(
            Gate::Fenced.receipt_miss(),
            Err(CellError::Fenced(_))
        ));
        Ok(())
    }

    #[test]
    fn scrub_lifecycle_transitions_and_fail_closed_fence_invariants() -> Result<(), CellError> {
        let cp = checkpoint(128, 300);
        let mut scrub = Scrub::after_open(true, Some(cp), None, &head(130, 310), None, Some(7));

        assert_eq!(
            scrub.gate(),
            Gate::Verifying {
                covered_seq: 300,
                idempotency_unproven: true,
            }
        );
        assert!(matches!(scrub.advancement(), Advance::Hold));
        assert!(matches!(scrub.refuse_fork(), Err(CellError::Verifying(_))));
        assert!(matches!(
            scrub.refuse_unproven_miss(),
            Err(CellError::Verifying(_))
        ));

        let mut run = scrub
            .begin_step()
            .ok_or_else(|| CellError::Encoding("expected active run".to_owned()))?;
        assert!(matches!(scrub.advancement(), Advance::Hold));
        assert!(matches!(scrub.refuse_fork(), Err(CellError::Verifying(_))));
        assert!(scrub.begin_step().is_none());

        run.phase = RunPhase::History {
            cursor: Box::new(history::HistoryCursor::new(None)),
            quantity_scrub: Box::new(crate::quantity::QuantityScrub::new(None, None)),
        };
        scrub.finish_step_continue(run);
        assert_eq!(
            scrub.gate(),
            Gate::Verifying {
                covered_seq: 300,
                idempotency_unproven: false,
            }
        );
        scrub.refuse_unproven_miss()?;
        assert!(matches!(scrub.refuse_fork(), Err(CellError::Verifying(_))));

        for _ in 0..TRANSIENT_RETRIES {
            let active = scrub
                .begin_step()
                .ok_or_else(|| CellError::Encoding("expected retry run".to_owned()))?;
            assert!(scrub.try_retry_transient(active));
        }
        let exhausted = scrub
            .begin_step()
            .ok_or_else(|| CellError::Encoding("expected exhausted run".to_owned()))?;
        assert!(!scrub.try_retry_transient(exhausted));
        scrub.fence_pending("defect detected".to_owned());
        assert!(scrub.is_fenced());
        assert!(scrub.needs_fence_persist());
        assert_eq!(
            scrub.pending_fence_finding().as_deref(),
            Some("defect detected")
        );
        assert_eq!(scrub.gate(), Gate::Fenced);
        assert_eq!(
            scrub.status(),
            ScrubStatus::Fenced {
                finding: "defect detected".to_owned(),
            }
        );
        scrub.mark_fence_persisted();
        assert!(!scrub.needs_fence_persist());
        assert!(scrub.pending_fence_finding().is_none());
        assert!(matches!(scrub.refuse_fenced(), Err(CellError::Fenced(_))));

        let fenced_fork = scrub.for_fork();
        assert!(fenced_fork.is_fenced());
        let mut clean_fork = Scrub::default().for_fork();
        assert_eq!(clean_fork.gate(), Gate::Open);
        assert!(matches!(clean_fork.advancement(), Advance::Hold));
        clean_fork.catch_up(head(500, 900), None);
        assert_eq!(clean_fork.status(), ScrubStatus::Idle);
        Ok(())
    }
}
