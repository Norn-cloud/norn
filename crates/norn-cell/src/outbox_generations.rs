//! Outbox-generation lifecycle orchestration on [`CellStore`] and [`Writer`],
//! plus export-restore replay for consumed result generations (`D1`).

use norn_ir::RuntimeValue as Value;
use norn_kernel::world::outbox::Queue;

use crate::codec::canonical_json;
use crate::engine::{SqlEngine, params};
use crate::error::CellError;
use crate::export::ExportRecord;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::store::{
    At, CellStore, CommitOutcome, EffectRow, MAIN_WORLD, OutboxCommand, SystemWrite,
};
use crate::system::{OUTBOX, PERFORM_CLAIM, RUN};
use crate::write::Writer;

impl EffectRow {
    /// Timer id targeted by a timer scheduling or cancellation effect, if any.
    pub(crate) fn expiry_timer_id(&self) -> Option<&str> {
        match self {
            Self::Schedule { timer } => Some(&timer.timer_id),
            Self::CancelTimer { timer_id } => Some(timer_id),
            Self::Enqueue { .. }
            | Self::ClaimPerform { .. }
            | Self::ReconcileOutbox { .. }
            | Self::RecordCompensation { .. }
            | Self::RecordLateOutcome { .. } => None,
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "replays the decomposed ConsumedInput::Result fields from restore_input"
)]
pub(crate) fn restore_consumed_result<E: SqlEngine>(
    store: &mut CellStore<E>,
    record: &ExportRecord,
    position: u64,
    commit_seq: u64,
    command: &OutboxCommand,
    result: Option<&Value>,
    runs: &[crate::RunRecord],
    mismatch: &dyn Fn() -> CellError,
) -> Result<(), CellError> {
    let head = crate::history::replayed_result(store, position)?.ok_or_else(mismatch)?;
    let already_settled = head.result.is_some();
    if head.position != position
        || head.commit_seq != commit_seq
        || head.command != *command
        || result.is_none()
        || (already_settled && head.result.as_ref() != result)
    {
        return Err(mismatch());
    }
    // Older generation runs can survive command replacement without
    // ever having been consumed. Recover the recorded history, without IO.
    let existing = store.read().runs(&command.command_id)?;
    if existing.iter().any(|run| !runs.contains(run)) {
        return Err(mismatch());
    }
    crate::history::restore_run_history(store, record, command, runs, &existing)?;
    let current: Vec<_> = runs
        .iter()
        .filter(|run| run.generation == command.generation)
        .collect();
    let valid_current = if already_settled {
        current.is_empty()
            || (current.len() == 1
                && current.first().is_some_and(|run| {
                    run.attempt == 1
                        && run.consumed_commit_seq.is_none()
                        && Some(&run.outcome) == result
                }))
    } else {
        current.len() == 1
            && current.first().is_some_and(|run| {
                run.attempt == 1
                    && run.consumed_commit_seq.is_none()
                    && Some(&run.outcome) == result
            })
    };
    if !valid_current {
        return Err(mismatch());
    }
    if !already_settled {
        let world = crate::payload_storage::receipt_world(&store.conn, head.commit_seq)?;
        let encoded_result = crate::payload_storage::encode_result(
            &store.conn,
            &record.batch.scope,
            &world,
            result.ok_or_else(mismatch)?,
        )?;
        store.conn.execute(
            "UPDATE \"_norn_outbox\" SET \"result\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3",
            params![encoded_result, command.command_id, to_i64(command.generation, "generation")?],
        )?;
    }
    crate::outbox_ledger::restore_consumed_runs(
        &store.conn,
        &command.command_id,
        command.generation,
        runs,
        !already_settled,
    )?;
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Durably attach an external outcome verified by `proof` to its original
    /// generation (`D1`), committing a zero-event `SystemWrite::RecordLateOutcome`
    /// so the evidence survives export and restore.
    ///
    /// Fails closed with [`CellError::OutboxLedgerRefused`] on an invalid
    /// [`norn_kernel::world::outbox_ledger::DispatchProof`] or conflicting
    /// outcome. A late outcome for a prior generation attaches only to that
    /// generation and never mutates the active outbox head.
    ///
    /// # Errors
    ///
    /// `core.world.perform_forbidden` in a fork; [`CellError::OutboxLedgerRefused`]
    /// on proof/state refusal; SQL engine or encoding errors.
    pub fn record_late_outcome(
        &mut self,
        scope: &str,
        logical_time: u64,
        command_id: &str,
        proof: norn_kernel::world::outbox_ledger::DispatchProof,
        ok: bool,
        outcome: &Value,
    ) -> Result<CommitOutcome, CellError> {
        let write = SystemWrite::RecordLateOutcome {
            command_id: command_id.to_owned(),
            proof,
            ok,
            outcome: outcome.clone(),
        };
        let place = At {
            world: MAIN_WORLD,
            scope,
            logical_time,
        };
        self.apply_system(&place, write)
    }

    /// Durably record an outbox reconciliation decision for `(command_id, generation)` (`D1`).
    ///
    /// Commits a zero-event `SystemWrite::ReconcileOutbox` recorded in the
    /// export journal and replayed across restores.
    ///
    /// # Errors
    ///
    /// `core.world.perform_forbidden` in a fork; [`CellError::OutboxLedgerRefused`]
    /// if the decision does not apply; store or SQL errors.
    pub fn reconcile_outbox(
        &mut self,
        scope: &str,
        logical_time: u64,
        command_id: &str,
        generation: u64,
        decision: norn_kernel::world::outbox_ledger::ReconciliationDecision,
        confirmed_outcome: Option<&Value>,
    ) -> Result<CommitOutcome, CellError> {
        let fenced_at = self.perform_fence_generation(command_id)?;
        let confirmed_outcome = match confirmed_outcome {
            Some(outcome) => Some(outcome.clone()),
            None if matches!(
                decision,
                norn_kernel::world::outbox_ledger::ReconciliationDecision::ConfirmedOutcome { .. }
            ) =>
            {
                self.read()
                    .pending_command(command_id)?
                    .filter(|row| row.command.generation == generation)
                    .and_then(|row| row.result)
            }
            None => None,
        };
        let write = SystemWrite::ReconcileOutbox {
            command_id: command_id.to_owned(),
            generation,
            decision,
            confirmed_outcome,
            fenced_at,
        };
        let place = At {
            world: MAIN_WORLD,
            scope,
            logical_time,
        };
        self.apply_system(&place, write)
    }

    /// Durably record a compensation attempt outcome for `(command_id, generation)` (`D1`).
    ///
    /// # Errors
    ///
    /// `core.world.perform_forbidden` in a fork; [`CellError::OutboxLedgerRefused`]
    /// if compensation is not pending; store or SQL errors.
    pub fn record_outbox_compensation(
        &mut self,
        scope: &str,
        logical_time: u64,
        command_id: &str,
        generation: u64,
        ok: bool,
    ) -> Result<CommitOutcome, CellError> {
        let fenced_at = self.perform_fence_generation(command_id)?;
        let write = SystemWrite::RecordCompensation {
            command_id: command_id.to_owned(),
            generation,
            ok,
            fenced_at,
        };
        let place = At {
            world: MAIN_WORLD,
            scope,
            logical_time,
        };
        self.apply_system(&place, write)
    }

    /// Return the [`norn_kernel::world::outbox_ledger::DispatchProof`] of a
    /// superseded prior generation that was durably claimed and has no
    /// attached outcome yet (`D1`).
    pub(crate) fn pending_late_dispatch_proof(
        &self,
        command_id: &str,
        generation: u64,
    ) -> Result<Option<norn_kernel::world::outbox_ledger::DispatchProof>, CellError> {
        crate::outbox_ledger::pending_late_dispatch_proof(&self.conn, command_id, generation)
    }

    /// Record `outcome` as the head command's result when it has none.
    pub(crate) fn record_head_result(
        &self,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<bool, CellError> {
        self.write_tx(|| {
            let head = StoreReader::new(&self.conn, &self.model)
                .with_source_disputes(&self.source_disputes)
                .head_command()?;
            let is_head = head.as_ref().is_some_and(|head| {
                head.command.command_id == command_id
                    && head.command.generation == generation
                    && head.result.is_none()
            });
            if !is_head {
                return Ok(false);
            }
            let head = head.ok_or_else(|| {
                CellError::Encoding("pending result command disappeared".to_owned())
            })?;
            self.store_result(&head, command_id, generation, outcome)
        })
    }

    /// Fault fixture: record a parked command's result in its own
    /// transaction, apart from any consuming commit, as an engine before P3
    /// did; a file such an engine wrote can still hold one, and recovery
    /// consumes it with `Settlement::Recorded`. Production records a parked
    /// result only inside the commit that consumes it.
    ///
    /// # Errors
    ///
    /// Serialization or SQL engine errors.
    #[cfg(any(test, feature = "fault-injection"))]
    #[doc(hidden)]
    pub fn record_parked_apart(
        &mut self,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<bool, CellError> {
        self.write_tx(|| self.record_parked_in_tx(command_id, generation, outcome))
    }

    /// PARKED-1: record the result of a parked command (one the cell resolves
    /// itself, such as a `norn.decision@1` the cell issued a receipt for) in
    /// the open transaction. A parked command may complete out of FIFO order,
    /// but at most once: a second record of the same generation, or of a
    /// command that is not parked and pending, returns `false` and writes
    /// nothing.
    pub(crate) fn record_parked_in_tx(
        &self,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<bool, CellError> {
        let parked = StoreReader::new(&self.conn, &self.model)
            .with_source_disputes(&self.source_disputes)
            .pending_command(command_id)?
            .filter(|row| {
                row.queue == Queue::Parked
                    && row.command.generation == generation
                    && row.result.is_none()
            });
        let Some(parked) = parked else {
            return Ok(false);
        };
        self.store_result(&parked, command_id, generation, outcome)
    }

    /// Record `outcome` in `_norn_outbox_record` and project it onto `_norn_outbox` and `_norn_run`.
    fn store_result(
        &self,
        head: &crate::read::PendingCommand,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<bool, CellError> {
        let _status = crate::outbox_ledger::record_active_outcome(
            &self.conn, command_id, generation, outcome,
        )?;
        self.project_result(head, command_id, generation, outcome)?;
        Ok(true)
    }

    /// Project an already-admitted active-head `outcome` onto `_norn_outbox` and `_norn_run`.
    fn project_result(
        &self,
        head: &crate::read::PendingCommand,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<(), CellError> {
        crate::payload_storage::remember_command(
            &self.conn,
            command_id,
            generation,
            head.commit_seq,
        )?;
        let outcome = match self.local_export_record(head.commit_seq)? {
            Some(record) => crate::payload_storage::encode_result(
                &self.conn,
                &record.batch.scope,
                &record.batch.world,
                outcome,
            )?,
            None => canonical_json(outcome)?,
        };
        let generation = to_i64(generation, "generation")?;
        self.conn.execute(
            &format!(
                "UPDATE \"{OUTBOX}\" SET \"result\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3"
            ),
            params![outcome, command_id, generation],
        )?;
        let attempts: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM \"{RUN}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
            ),
            params![command_id, generation],
            |row| row.get::<i64>(0),
        )?;
        let attempt = to_u64(attempts, "attempt")?
            .checked_add(1)
            .ok_or(CellError::OutOfRange("attempt"))?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{RUN}\" (\"command_id\", \"generation\", \"attempt\", \"outcome\", \"consumed_commit_seq\") VALUES (?1, ?2, ?3, ?4, NULL)"
            ),
            params![command_id, generation, to_i64(attempt, "attempt")?, outcome],
        )?;
        Ok(())
    }
}

impl<E: SqlEngine> Writer<'_, E> {
    pub(crate) fn claim_perform(&self, command_id: &str, generation: u64) -> Result<(), CellError> {
        let generation_sql = to_i64(generation, "generation")?;
        let head = StoreReader::new(self.conn, self.model).head_command()?;
        let is_head = head.is_some_and(|head| {
            head.command.command_id == command_id
                && head.command.generation == generation
                && head.result.is_none()
        });
        if !is_head {
            let parked = StoreReader::new(self.conn, self.model)
                .pending_command(command_id)?
                .is_some_and(|row| {
                    row.queue == Queue::Parked && row.command.generation == generation
                });
            if parked {
                return Err(CellError::ParkedClaimUnsupported {
                    command_id: command_id.to_owned(),
                    generation,
                });
            }
            return Err(CellError::PerformClaimNotPending {
                command_id: command_id.to_owned(),
                generation,
            });
        }
        let known: Option<i64> = self
            .conn
            .query_optional(
                &format!(
                    "SELECT 1 FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
                ),
                params![command_id, generation_sql],
                |row| row.get::<i64>(0),
            )?;
        if known.is_some() {
            return Err(CellError::PerformAlreadyClaimed {
                command_id: command_id.to_owned(),
                generation,
            });
        }
        let _proof = crate::outbox_ledger::record_dispatched(
            self.conn,
            command_id,
            generation,
            self.commit_seq,
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{PERFORM_CLAIM}\" (\"command_id\", \"generation\", \"commit_seq\") VALUES (?1, ?2, ?3)"
            ),
            params![command_id, generation_sql, self.commit_seq_sql()?],
        )?;
        Ok(())
    }

    pub(crate) fn reconcile_outbox(
        &self,
        command_id: &str,
        generation: u64,
        decision: norn_kernel::world::outbox_ledger::ReconciliationDecision,
        confirmed_outcome: Option<&Value>,
        fenced_at: Option<u64>,
    ) -> Result<(), CellError> {
        use norn_kernel::world::outbox_ledger::{OutboxLedgerRefusal, ReconciliationDecision};
        if let ReconciliationDecision::ConfirmedOutcome { ok, outcome_digest } = decision {
            let pending = StoreReader::new(self.conn, self.model)
                .pending_command(command_id)?
                .filter(|row| row.command.generation == generation);
            if let Some(outcome) = confirmed_outcome {
                if crate::outbox_ledger::outcome_digest(outcome)? != outcome_digest
                    || crate::outbox_ledger::outcome_is_ok(outcome) != ok
                {
                    return Err(CellError::OutboxLedgerRefused {
                        command_id: command_id.to_owned(),
                        generation,
                        refusal: OutboxLedgerRefusal::OutcomeDiverged,
                    });
                }
            } else if let Some(pending) = &pending {
                match &pending.result {
                    Some(existing) => {
                        if crate::outbox_ledger::outcome_digest(existing)? != outcome_digest
                            || crate::outbox_ledger::outcome_is_ok(existing) != ok
                        {
                            return Err(CellError::OutboxLedgerRefused {
                                command_id: command_id.to_owned(),
                                generation,
                                refusal: OutboxLedgerRefusal::OutcomeDiverged,
                            });
                        }
                    }
                    // Active head has no stored outcome: `ConfirmedOutcome` must
                    // carry the payload so `_norn_outbox.result` stays in lockstep
                    // with `HeadProjection`.
                    None => {
                        return Err(CellError::OutboxLedgerRefused {
                            command_id: command_id.to_owned(),
                            generation,
                            refusal: OutboxLedgerRefusal::NotPending,
                        });
                    }
                }
            }
        }
        let (_status, is_active, _head) = crate::outbox_ledger::record_reconciliation(
            self.conn,
            command_id,
            generation,
            decision,
            self.commit_seq,
            fenced_at,
        )?;
        if is_active
            && matches!(decision, ReconciliationDecision::ConfirmedOutcome { .. })
            && let Some(outcome) = confirmed_outcome
        {
            self.project_active_outcome(command_id, generation, outcome)?;
        }
        Ok(())
    }

    pub(crate) fn record_late_outcome(
        &self,
        command_id: &str,
        proof: norn_kernel::world::outbox_ledger::DispatchProof,
        ok: bool,
        outcome: &Value,
    ) -> Result<(), CellError> {
        let (status, is_active) = crate::outbox_ledger::record_outcome_with_proof(
            self.conn, command_id, proof, ok, outcome,
        )?;
        if status == norn_kernel::world::outbox_ledger::ApplyStatus::Applied && is_active {
            self.project_active_outcome(command_id, proof.generation, outcome)?;
        }
        Ok(())
    }

    fn project_active_outcome(
        &self,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<(), CellError> {
        let pending = StoreReader::new(self.conn, self.model)
            .pending_command(command_id)?
            .filter(|row| row.command.generation == generation && row.result.is_none());
        let Some(pending) = pending else {
            return Ok(());
        };
        crate::payload_storage::remember_command(
            self.conn,
            command_id,
            generation,
            pending.commit_seq,
        )?;
        let encoded = crate::payload_storage::encode_result(
            self.conn,
            &self.batch.scope,
            self.world,
            outcome,
        )?;
        let generation_sql = to_i64(generation, "generation")?;
        self.conn.execute(
            &format!(
                "UPDATE \"{OUTBOX}\" SET \"result\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3"
            ),
            params![encoded, command_id, generation_sql],
        )?;
        let attempts: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM \"{RUN}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
            ),
            params![command_id, generation_sql],
            |row| row.get::<i64>(0),
        )?;
        if attempts == 0 {
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{RUN}\" (\"command_id\", \"generation\", \"attempt\", \"outcome\", \"consumed_commit_seq\") VALUES (?1, ?2, 1, ?3, NULL)"
                ),
                params![command_id, generation_sql, encoded],
            )?;
        }
        Ok(())
    }

    pub(crate) fn record_compensation(
        &self,
        command_id: &str,
        generation: u64,
        ok: bool,
        fenced_at: Option<u64>,
    ) -> Result<(), CellError> {
        let (_status, compensated) = crate::outbox_ledger::record_compensation(
            self.conn,
            command_id,
            generation,
            ok,
            self.commit_seq,
            fenced_at,
        )?;
        if compensated {
            self.conn.execute(
                &format!(
                    "UPDATE \"{OUTBOX}\" SET \"consumed_commit_seq\" = ?1 WHERE \"command_id\" = ?2 AND \"consumed_commit_seq\" IS NULL"
                ),
                params![self.commit_seq_sql()?, command_id],
            )?;
        }
        Ok(())
    }
}
