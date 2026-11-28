//! D03 (#1699) & D1: perform claims, durable reconciliation, and late-outcome
//! attachment over the immutable per-generation outbox ledger.
//!
//! A claim reads one FIFO row, replays its
//! [`norn_kernel::world::outbox_ledger::OperationLedger`] from
//! `_norn_outbox_record` to derive its
//! [`norn_kernel::world::outbox_ledger::HeadProjection`], and applies
//! [`OutboxEvent::Claim`] through the kernel's total [`step`]. Each answer is
//! its own [`ClaimOutcome`] variant: an empty queue is never confused with a
//! head a restore fenced, and a reconciled claim names its target rather than
//! taking whatever is at the head.

use norn_kernel::world::outbox::{OutboxEvent, OutboxRefusal, OutboxState, Queue, step};
use norn_kernel::world::outbox_ledger::{DispatchProof, ReconciliationDecision};
use norn_kernel::{Command, CommandId, Generation, Performed};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{SqliteCell, at, from_runtime, to_runtime};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::PendingCommand;
use crate::store::{CommitOutcome, MAIN_WORLD, SystemWrite};

/// What a perform claim found at a FIFO outbox command.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimOutcome<C, I> {
    /// No FIFO command is pending.
    Empty,
    /// The head is fenced by a restore at `fenced_at` until ledgered
    /// reconciliation authorizes a later generation; it blocks the queue.
    HeadFenced {
        /// The fenced command.
        command: CommandId,
        /// The generation the restore fenced.
        fenced_at: Generation,
    },
    /// This generation was durably claimed by this call and may be dispatched.
    Fresh(Command<C>),
    /// A result was already recorded and must be redelivered without dispatch.
    Recorded {
        /// The command whose result was recorded.
        command: Command<C>,
        /// The recorded result delivery.
        performed: Performed<I>,
    },
    /// A prior process durably claimed this generation, but no result was
    /// recorded before it stopped. The external outcome must be reconciled.
    OutcomeUnknown(Command<C>),
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    C: DeserializeOwned,
    I: DeserializeOwned,
{
    /// Inspect and durably claim the FIFO outbox head before external
    /// dispatch.
    ///
    /// A restored perform fence takes precedence over both a saved result and
    /// a saved claim unless durable reconciliation confirmed the fenced
    /// generation's outcome. A fresh command is returned only after the claim's
    /// zero-event commit and export-journal record are durable.
    ///
    /// # Errors
    ///
    /// SQLite or decode errors, or an outbox race that invalidates the claim
    /// before its commit.
    pub fn claim_perform(&mut self) -> Result<ClaimOutcome<C, I>, CellError> {
        let Some(head) = self.store.read().head_command()? else {
            return Ok(ClaimOutcome::Empty);
        };
        self.claim_row(head, None)
    }

    /// Claim `target` at exactly `generation`, the later generation the
    /// host's ledgered reconciliation policy authorized past a restore fence.
    /// The fence itself remains intact; a new restore fences this generation
    /// again. Callers must durably reconcile (`AuthorizeReissue`) and enqueue
    /// `generation` before invoking this method. It never claims another command.
    ///
    /// # Errors
    /// [`CellError::PerformClaimNotPending`] unless `target` at `generation`
    /// is the FIFO head; store or decode errors, as for
    /// [`Self::claim_perform`].
    pub fn claim_reconciled_perform(
        &mut self,
        target: &CommandId,
        generation: Generation,
    ) -> Result<ClaimOutcome<C, I>, CellError> {
        let head = self.store.read().head_command()?.filter(|head| {
            head.queue == Queue::Fifo
                && head.command.command_id == target.0
                && head.command.generation == generation.0
        });
        let Some(head) = head else {
            return Err(CellError::PerformClaimNotPending {
                command_id: target.0.clone(),
                generation: generation.0,
            });
        };
        self.claim_row(head, Some(generation))
    }

    fn claim_row(
        &mut self,
        row: PendingCommand,
        reconciled: Option<Generation>,
    ) -> Result<ClaimOutcome<C, I>, CellError> {
        let generation = row.command.generation;
        let ledger = self
            .store
            .read()
            .outbox_ledger(&row.command.command_id)?
            .ok_or_else(|| CellError::PerformClaimNotPending {
                command_id: row.command.command_id.clone(),
                generation,
            })?;
        let projection = ledger.head_projection();
        let fence = self.store.restore_fence(&row.command.command_id)?;
        let mut state = projection.state;
        // Keep a restore fence active for ordinary `claim_perform()` calls
        // even after an authorized newer generation is enqueued, unless
        // durable reconciliation confirmed an outcome or the caller names the
        // authorized generation via `claim_reconciled_perform`.
        if let crate::outbox_generations::RestoreFence::FencedAt {
            generation: fenced_at,
        } = fence
            && !ledger.active().has_reconciled_outcome()
        {
            let lifted = reconciled.is_some_and(|authorized| {
                step(
                    OutboxState::ReconciliationRequired { fenced_at },
                    generation,
                    OutboxEvent::Reconciled {
                        generation: authorized.0,
                    },
                )
                .is_ok()
            });
            if !lifted {
                state = OutboxState::ReconciliationRequired { fenced_at };
            }
        }
        let Err(refusal) = step(state, generation, OutboxEvent::Claim { generation }) else {
            return self.commit_claim(row);
        };
        // The fence is checked before the command is decoded or its claim
        // state exposed.
        if let OutboxRefusal::Fenced { fenced_at } = refusal {
            tracing::warn!(command_id = %row.command.command_id, "restored command fenced pending external-effect reconciliation");
            return Ok(ClaimOutcome::HeadFenced {
                command: CommandId(row.command.command_id),
                fenced_at: Generation(fenced_at),
            });
        }
        let command: Command<C> = from_runtime(&row.command.command)?;
        match (refusal, state) {
            (_, OutboxState::Succeeded | OutboxState::Failed) => {
                let result =
                    row.result
                        .as_ref()
                        .ok_or_else(|| CellError::PerformClaimNotPending {
                            command_id: command.id.0.clone(),
                            generation,
                        })?;
                let performed = Performed {
                    command: command.id.clone(),
                    generation: command.generation,
                    input: from_runtime(result)?,
                };
                Ok(ClaimOutcome::Recorded { command, performed })
            }
            (_, OutboxState::Dispatched { .. } | OutboxState::OutcomeUnknown { .. }) => {
                // WORLD-ISOLATE-1 (Fable S9): a fork inherits main's claim but
                // can never learn or settle its real outcome.
                self.store.refuse_perform_in_fork()?;
                Ok(ClaimOutcome::OutcomeUnknown(command))
            }
            (
                OutboxRefusal::StaleGeneration
                | OutboxRefusal::PayloadDiverged
                | OutboxRefusal::Fenced { .. }
                | OutboxRefusal::NotPending
                | OutboxRefusal::Terminal,
                OutboxState::Requested
                | OutboxState::ReconciliationRequired { .. }
                | OutboxState::CompensationPending { .. }
                | OutboxState::Compensated
                | OutboxState::Consumed { .. },
            ) => Err(CellError::PerformClaimNotPending {
                command_id: command.id.0,
                generation,
            }),
        }
    }

    /// Durably claim `row` with a zero-event commit before it is dispatched.
    fn commit_claim(&mut self, row: PendingCommand) -> Result<ClaimOutcome<C, I>, CellError> {
        let command: Command<C> = from_runtime(&row.command.command)?;
        let write = SystemWrite::ClaimPerform {
            command_id: row.command.command_id,
            generation: row.command.generation,
        };
        self.store
            .apply_system(&at(&self.scope, MAIN_WORLD, self.now.as_millis()), write)?;
        Ok(ClaimOutcome::Fresh(command))
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    I: Serialize,
{
    /// Durably record an outbox reconciliation decision (`D1`).
    ///
    /// # Errors
    ///
    /// `core.world.perform_forbidden` in a fork; [`CellError::OutboxLedgerRefused`]
    /// if the decision does not apply; encoding or SQLite errors.
    pub fn reconcile_outbox(
        &mut self,
        command: &CommandId,
        generation: Generation,
        decision: ReconciliationDecision,
        confirmed_outcome: Option<&I>,
    ) -> Result<CommitOutcome, CellError> {
        let outcome = confirmed_outcome.map(to_runtime).transpose()?;
        self.store.reconcile_outbox(
            &self.scope,
            self.now.as_millis(),
            &command.0,
            generation.0,
            decision,
            outcome.as_ref(),
        )
    }

    /// Durably attach a late or active outcome carrying `proof` to its
    /// original generation (`D1`), surviving export and restore.
    ///
    /// # Errors
    ///
    /// `core.world.perform_forbidden` in a fork; [`CellError::OutboxLedgerRefused`]
    /// on invalid proof or conflicting outcome; encoding or SQLite errors.
    pub fn record_late_outcome(
        &mut self,
        command: &CommandId,
        proof: DispatchProof,
        ok: bool,
        input: &I,
    ) -> Result<CommitOutcome, CellError> {
        self.store.record_late_outcome(
            &self.scope,
            self.now.as_millis(),
            &command.0,
            proof,
            ok,
            &to_runtime(input)?,
        )
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// Durably record a compensation attempt outcome for `(command, generation)` (`D1`).
    ///
    /// # Errors
    ///
    /// `core.world.perform_forbidden` in a fork; [`CellError::OutboxLedgerRefused`]
    /// if compensation is not pending; SQLite errors.
    pub fn record_outbox_compensation(
        &mut self,
        command: &CommandId,
        generation: Generation,
        ok: bool,
    ) -> Result<CommitOutcome, CellError> {
        self.store.record_outbox_compensation(
            &self.scope,
            self.now.as_millis(),
            &command.0,
            generation.0,
            ok,
        )
    }
}
