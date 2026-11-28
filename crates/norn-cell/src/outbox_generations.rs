//! Outbox-generation lifecycle orchestration on [`CellStore`] and [`Writer`],
//! plus export-restore replay for consumed result generations (`D1`, `D3`).

use norn_ir::RuntimeValue as Value;
use norn_kernel::world::outbox::Queue;
use norn_kernel::world::outbox_ledger::{
    OutboxDigest, OutboxLedgerRefusal, ReconciliationDecision,
};

use crate::codec::canonical_json;
use crate::engine::{MAX_SAFE_INTEGER, SqlEngine, SqlValue, params};
use crate::error::CellError;
use crate::export::ExportRecord;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::store::{
    At, CellStore, CommitOutcome, EffectRow, MAIN_WORLD, OutboxCommand, SystemWrite,
};
use crate::system::{META, OUTBOX, PERFORM_CLAIM, RUN};
use crate::write::Writer;

/// Which generation tier a durable reconciliation record applied to (`D3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconciliationTarget {
    /// Reconciled the active head generation.
    ActiveHead,
    /// Reconciled a superseded prior generation.
    PriorGeneration,
}

/// Whether attaching a dispatch-proof-verified outcome settled the active head (`D3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProofOutcomeTarget {
    /// Newly settled the active unfenced head; project onto `_norn_outbox`.
    SettledActiveHead,
    /// Duplicate or prior/terminal generation; leave `_norn_outbox` unchanged.
    NoHeadProjection,
}

/// Whether a compensation attempt completed the operation (`D3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompensationProgress {
    /// Compensation failed and remains pending retry.
    StillPending,
    /// Compensation completed; mark the terminal head consumed in `_norn_outbox`.
    Compensated,
}

/// Whether export-restore replay should also record the active generation's run (`D3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveRunReplay {
    /// Active head was already settled; attach prior-generation late runs only.
    PriorOnly,
    /// Attach prior-generation late runs and record the active generation's run.
    IncludeActive,
}

/// Durable restore-fence state for one outbox `command_id` (`D3`).
///
/// Decoded fail-closed from `_norn_meta` (`restore:perform_fence:<command_id>`)
/// or reconstructed from a journaled commit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreFence {
    /// No restore fence is recorded for this command.
    Unfenced,
    /// Restored with a fence blocking automatic execution through `generation`
    /// (inclusive).
    FencedAt { generation: u64 },
}

impl RestoreFence {
    /// Lift an optional fence generation into a closed [`RestoreFence`].
    pub(crate) const fn from_optional(generation: Option<u64>) -> Self {
        match generation {
            Some(generation) => Self::FencedAt { generation },
            None => Self::Unfenced,
        }
    }

    /// The fenced generation, when fenced.
    pub(crate) const fn generation(self) -> Option<u64> {
        match self {
            Self::Unfenced => None,
            Self::FencedAt { generation } => Some(generation),
        }
    }

    /// Whether a restore fence is active.
    pub(crate) const fn is_fenced(self) -> bool {
        matches!(self, Self::FencedAt { .. })
    }

    /// Prefer `self` when fenced; otherwise fall back to `fallback`.
    pub(crate) const fn or(self, fallback: Self) -> Self {
        match self {
            Self::FencedAt { generation } => Self::FencedAt { generation },
            Self::Unfenced => fallback,
        }
    }

    /// Whether `generation` is at or below this fence.
    pub(crate) const fn blocks(self, generation: u64) -> bool {
        match self {
            Self::Unfenced => false,
            Self::FencedAt {
                generation: fenced_at,
            } => generation <= fenced_at,
        }
    }

    /// Parse a canonical decimal generation string (`0..=MAX_SAFE_INTEGER`).
    pub(crate) fn parse(raw: &str) -> Result<Self, CellError> {
        let canonical = !raw.is_empty()
            && raw.bytes().all(|byte| byte.is_ascii_digit())
            && (raw.len() == 1 || !raw.starts_with('0'));
        if !canonical {
            return Err(CellError::LedgerCorrupt {
                seq: 0,
                reason: format!("invalid restore fence generation: {raw:?}"),
            });
        }
        let generation = raw
            .parse::<u64>()
            .map_err(|error| CellError::LedgerCorrupt {
                seq: 0,
                reason: format!("invalid restore fence generation: {error}"),
            })?;
        let max_safe = u64::try_from(MAX_SAFE_INTEGER).unwrap_or(u64::MAX);
        if generation > max_safe {
            return Err(CellError::LedgerCorrupt {
                seq: 0,
                reason: format!("restore fence generation out of safe integer range: {generation}"),
            });
        }
        Ok(Self::FencedAt { generation })
    }

    /// Decode one optional `_norn_meta.value` SQL cell fail-closed.
    pub(crate) fn decode_sql(value: Option<SqlValue>) -> Result<Self, CellError> {
        match value {
            None => Ok(Self::Unfenced),
            Some(SqlValue::Text(raw)) => Self::parse(&raw),
            Some(SqlValue::Null | SqlValue::Integer(_) | SqlValue::Real(_) | SqlValue::Blob(_)) => {
                Err(CellError::LedgerCorrupt {
                    seq: 0,
                    reason: "invalid restore fence generation: non-text metadata value".to_owned(),
                })
            }
        }
    }

    /// Load the durable restore fence for `command_id` from `_norn_meta`.
    pub(crate) fn load<E: SqlEngine>(conn: &E, command_id: &str) -> Result<Self, CellError> {
        let value = conn.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![format!("restore:perform_fence:{command_id}")],
            |row| row.get_ref(0).cloned(),
        )?;
        Self::decode_sql(value)
    }
}

/// Closed lifecycle state of one `_norn_outbox` generation row paired with its
/// `_norn_perform_claim` row (`D3`).
///
/// Construction through [`Self::from_row_parts`] rejects impossible/contradictory
/// column combinations fail-closed with [`CellError::LedgerCorrupt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerationLifecycle {
    /// Pending FIFO command that has not been claimed or settled yet.
    PendingUnclaimed,
    /// Pending FIFO command that was durably claimed and has no result yet.
    PendingClaimed,
    /// Pending parked command awaiting cell-side resolution (never claimed).
    ParkedUnsettled,
    /// Pending command whose result has been recorded and awaits consumption.
    ResultRecorded { queue: Queue },
    /// Terminal command row already consumed (or compensated).
    Consumed,
}

impl GenerationLifecycle {
    /// Validate and classify one `_norn_outbox` + `_norn_perform_claim` row pair.
    pub(crate) fn from_row_parts(
        queue: Queue,
        commit_seq: u64,
        claim_commit_seq: Option<u64>,
        has_result: bool,
        consumed_commit_seq: Option<u64>,
    ) -> Result<Self, CellError> {
        if queue == Queue::Parked && claim_commit_seq.is_some() {
            return Err(CellError::LedgerCorrupt {
                seq: commit_seq,
                reason: "parked outbox generation cannot carry a perform claim".to_owned(),
            });
        }
        if let Some(claim_seq) = claim_commit_seq
            && claim_seq < commit_seq
        {
            return Err(CellError::LedgerCorrupt {
                seq: commit_seq,
                reason: "outbox perform claim predates command enqueue".to_owned(),
            });
        }
        if let Some(consumed_seq) = consumed_commit_seq {
            if consumed_seq < commit_seq {
                return Err(CellError::LedgerCorrupt {
                    seq: commit_seq,
                    reason: "outbox consumption predates command enqueue".to_owned(),
                });
            }
            if let Some(claim_seq) = claim_commit_seq
                && consumed_seq < claim_seq
            {
                return Err(CellError::LedgerCorrupt {
                    seq: commit_seq,
                    reason: "outbox consumption predates perform claim".to_owned(),
                });
            }
            return Ok(Self::Consumed);
        }
        if has_result {
            return Ok(Self::ResultRecorded { queue });
        }
        match (queue, claim_commit_seq) {
            (Queue::Parked, None) => Ok(Self::ParkedUnsettled),
            (Queue::Parked, Some(_)) => Err(CellError::LedgerCorrupt {
                seq: commit_seq,
                reason: "parked outbox generation cannot carry a perform claim".to_owned(),
            }),
            (Queue::Fifo, Some(_)) => Ok(Self::PendingClaimed),
            (Queue::Fifo, None) => Ok(Self::PendingUnclaimed),
        }
    }
}

/// Settlement state of the replayed outbox head row during export restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadSettlement {
    /// Head row has not recorded its result yet; restore must write it and
    /// replay the active generation run into `_norn_outbox_record`.
    Unsettled,
    /// Head row already holds the matching result; restore replays only
    /// prior-generation runs into `_norn_outbox_record`.
    AlreadySettled,
}

/// Validated reconciliation request pairing a [`ReconciliationDecision`] with
/// only the payload combinations legal for that decision.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReconciliationRequest<'a> {
    /// `ConfirmedOutcome` carrying a verified payload to project if active.
    ConfirmedWithPayload { outcome: &'a Value },
    /// `ConfirmedOutcome` referencing either an already-stored active result
    /// or a superseded prior generation.
    ConfirmedStoredOrPrior {
        ok: bool,
        outcome_digest: OutboxDigest,
    },
    /// Non-outcome reconciliation decision (`AuthorizeReissue` or
    /// `RequireCompensation`), never paired with a confirmed outcome payload.
    Control,
}

impl<'a> ReconciliationRequest<'a> {
    fn validate(
        command_id: &str,
        generation: u64,
        decision: ReconciliationDecision,
        confirmed_outcome: Option<&'a Value>,
    ) -> Result<Self, CellError> {
        match (decision, confirmed_outcome) {
            (ReconciliationDecision::ConfirmedOutcome { ok, outcome_digest }, Some(outcome)) => {
                if crate::outbox_ledger::outcome_digest(outcome)? != outcome_digest
                    || crate::outbox_ledger::outcome_is_ok(outcome) != ok
                {
                    return Err(CellError::OutboxLedgerRefused {
                        command_id: command_id.to_owned(),
                        generation,
                        refusal: OutboxLedgerRefusal::OutcomeDiverged,
                    });
                }
                Ok(Self::ConfirmedWithPayload { outcome })
            }
            (ReconciliationDecision::ConfirmedOutcome { ok, outcome_digest }, None) => {
                Ok(Self::ConfirmedStoredOrPrior { ok, outcome_digest })
            }
            (
                ReconciliationDecision::AuthorizeReissue { .. }
                | ReconciliationDecision::RequireCompensation,
                _,
            ) => Ok(Self::Control),
        }
    }
}

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
    let result = result.ok_or_else(mismatch)?;
    if head.position != position || head.commit_seq != commit_seq || head.command != *command {
        return Err(mismatch());
    }
    let settlement = match head.result.as_ref() {
        None => HeadSettlement::Unsettled,
        Some(existing) if existing == result => HeadSettlement::AlreadySettled,
        Some(_) => return Err(mismatch()),
    };
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
    let matches_attempt_one = current.len() == 1
        && current.first().is_some_and(|run| {
            run.attempt == 1 && run.consumed_commit_seq.is_none() && &run.outcome == result
        });
    let valid_current = match settlement {
        HeadSettlement::Unsettled => matches_attempt_one,
        HeadSettlement::AlreadySettled => current.is_empty() || matches_attempt_one,
    };
    if !valid_current {
        return Err(mismatch());
    }
    let replay_mode = match settlement {
        HeadSettlement::Unsettled => {
            let world = crate::payload_storage::receipt_world(&store.conn, head.commit_seq)?;
            let encoded_result = crate::payload_storage::encode_result(
                &store.conn,
                &record.batch.scope,
                &world,
                result,
            )?;
            store.conn.execute(
                "UPDATE \"_norn_outbox\" SET \"result\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3",
                params![encoded_result, command.command_id, to_i64(command.generation, "generation")?],
            )?;
            ActiveRunReplay::IncludeActive
        }
        HeadSettlement::AlreadySettled => ActiveRunReplay::PriorOnly,
    };
    crate::outbox_ledger::restore_consumed_runs(
        &store.conn,
        &command.command_id,
        command.generation,
        runs,
        replay_mode,
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
        decision: ReconciliationDecision,
        confirmed_outcome: Option<&Value>,
    ) -> Result<CommitOutcome, CellError> {
        let fenced_at = self.restore_fence(command_id)?.generation();
        let confirmed_outcome = match confirmed_outcome {
            Some(outcome) => Some(outcome.clone()),
            None if matches!(decision, ReconciliationDecision::ConfirmedOutcome { .. }) => self
                .read()
                .pending_command(command_id)?
                .filter(|row| row.command.generation == generation)
                .and_then(|row| row.result),
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
        let fenced_at = self.restore_fence(command_id)?.generation();
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
            let Some(head) = StoreReader::new(&self.conn, &self.model)
                .with_source_disputes(&self.source_disputes)
                .head_command()?
                .filter(|head| {
                    head.command.command_id == command_id
                        && head.command.generation == generation
                        && head.result.is_none()
                })
            else {
                return Ok(false);
            };
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
        let Some(parked) = StoreReader::new(&self.conn, &self.model)
            .with_source_disputes(&self.source_disputes)
            .pending_command(command_id)?
            .filter(|row| {
                row.queue == Queue::Parked
                    && row.command.generation == generation
                    && row.result.is_none()
            })
        else {
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
        let reader = StoreReader::new(self.conn, self.model);
        let row = match reader.head_command()? {
            Some(head)
                if head.command.command_id == command_id
                    && head.command.generation == generation =>
            {
                head
            }
            Some(_) | None => {
                let Some(parked) = reader.pending_command(command_id)?.filter(|row| {
                    row.queue == Queue::Parked && row.command.generation == generation
                }) else {
                    return Err(CellError::PerformClaimNotPending {
                        command_id: command_id.to_owned(),
                        generation,
                    });
                };
                parked
            }
        };
        let claim_commit_seq = self.conn.query_optional(
            &format!(
                "SELECT \"commit_seq\" FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
            ),
            params![command_id, generation_sql],
            |claim| to_u64(claim.get::<i64>(0)?, "claim commit_seq"),
        )?;
        match GenerationLifecycle::from_row_parts(
            row.queue,
            row.commit_seq,
            claim_commit_seq,
            row.result.is_some(),
            None,
        )? {
            GenerationLifecycle::PendingUnclaimed => {}
            GenerationLifecycle::PendingClaimed => {
                return Err(CellError::PerformAlreadyClaimed {
                    command_id: command_id.to_owned(),
                    generation,
                });
            }
            GenerationLifecycle::ParkedUnsettled
            | GenerationLifecycle::ResultRecorded {
                queue: Queue::Parked,
            } => {
                return Err(CellError::ParkedClaimUnsupported {
                    command_id: command_id.to_owned(),
                    generation,
                });
            }
            GenerationLifecycle::ResultRecorded { queue: Queue::Fifo }
            | GenerationLifecycle::Consumed => {
                return Err(CellError::PerformClaimNotPending {
                    command_id: command_id.to_owned(),
                    generation,
                });
            }
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
        decision: ReconciliationDecision,
        confirmed_outcome: Option<&Value>,
        fenced_at: Option<u64>,
    ) -> Result<(), CellError> {
        let request =
            ReconciliationRequest::validate(command_id, generation, decision, confirmed_outcome)?;
        if let ReconciliationRequest::ConfirmedStoredOrPrior { ok, outcome_digest } = request
            && let Some(pending) = StoreReader::new(self.conn, self.model)
                .pending_command(command_id)?
                .filter(|row| row.command.generation == generation)
        {
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
        let target = crate::outbox_ledger::record_reconciliation(
            self.conn,
            command_id,
            generation,
            decision,
            self.commit_seq,
            RestoreFence::from_optional(fenced_at),
        )?;
        if let (
            ReconciliationTarget::ActiveHead,
            ReconciliationRequest::ConfirmedWithPayload { outcome },
        ) = (target, request)
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
        match crate::outbox_ledger::record_outcome_with_proof(
            self.conn, command_id, proof, ok, outcome,
        )? {
            ProofOutcomeTarget::SettledActiveHead => {
                self.project_active_outcome(command_id, proof.generation, outcome)?;
            }
            ProofOutcomeTarget::NoHeadProjection => {}
        }
        Ok(())
    }

    fn project_active_outcome(
        &self,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<(), CellError> {
        let Some(pending) = StoreReader::new(self.conn, self.model)
            .pending_command(command_id)?
            .filter(|row| row.command.generation == generation && row.result.is_none())
        else {
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
        match crate::outbox_ledger::record_compensation(
            self.conn,
            command_id,
            generation,
            ok,
            self.commit_seq,
            RestoreFence::from_optional(fenced_at),
        )? {
            CompensationProgress::Compensated => {
                self.conn.execute(
                    &format!(
                        "UPDATE \"{OUTBOX}\" SET \"consumed_commit_seq\" = ?1 WHERE \"command_id\" = ?2 AND \"consumed_commit_seq\" IS NULL"
                    ),
                    params![self.commit_seq_sql()?, command_id],
                )?;
            }
            CompensationProgress::StillPending => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use norn_ir::RuntimeValue as Value;
    use norn_kernel::world::outbox::Queue;
    use norn_kernel::world::outbox_ledger::{OutboxLedgerRefusal, ReconciliationDecision};

    use super::{GenerationLifecycle, ReconciliationRequest, RestoreFence};
    use crate::engine::{MAX_SAFE_INTEGER, SqlValue};
    use crate::error::CellError;

    #[test]
    fn restore_fence_parses_canonical_and_rejects_corrupt_values() -> Result<(), CellError> {
        assert_eq!(RestoreFence::decode_sql(None)?, RestoreFence::Unfenced);
        assert!(!RestoreFence::Unfenced.is_fenced());
        assert!(!RestoreFence::Unfenced.blocks(0));

        let zero = RestoreFence::decode_sql(Some(SqlValue::Text("0".to_owned())))?;
        assert_eq!(zero, RestoreFence::FencedAt { generation: 0 });
        assert!(zero.is_fenced());
        assert!(zero.blocks(0));
        assert!(!zero.blocks(1));

        let max_safe = RestoreFence::parse(&MAX_SAFE_INTEGER.to_string())?;
        assert_eq!(max_safe.generation(), u64::try_from(MAX_SAFE_INTEGER).ok());
        assert_eq!(RestoreFence::Unfenced.or(zero), zero);
        assert_eq!(max_safe.or(zero), max_safe);

        for bad_text in [
            "",
            "-1",
            "+1",
            "00",
            "01",
            " 1",
            "1 ",
            "abc",
            "9007199254740993",
            "18446744073709551616",
        ] {
            let err = RestoreFence::parse(bad_text);
            assert!(
                matches!(err, Err(CellError::LedgerCorrupt { seq: 0, .. })),
                "expected LedgerCorrupt for {bad_text:?}, got {err:?}"
            );
        }
        for bad_sql in [
            SqlValue::Null,
            SqlValue::Integer(1),
            SqlValue::Real(1.0),
            SqlValue::Blob(vec![b'1']),
        ] {
            let err = RestoreFence::decode_sql(Some(bad_sql));
            assert!(matches!(err, Err(CellError::LedgerCorrupt { seq: 0, .. })));
        }
        Ok(())
    }

    #[test]
    fn generation_lifecycle_admits_valid_states_and_rejects_contradictions() -> Result<(), CellError>
    {
        assert_eq!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, None, false, None)?,
            GenerationLifecycle::PendingUnclaimed
        );
        assert_eq!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, Some(6), false, None)?,
            GenerationLifecycle::PendingClaimed
        );
        assert_eq!(
            GenerationLifecycle::from_row_parts(Queue::Parked, 5, None, false, None)?,
            GenerationLifecycle::ParkedUnsettled
        );
        assert_eq!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, Some(6), true, None)?,
            GenerationLifecycle::ResultRecorded { queue: Queue::Fifo }
        );
        assert_eq!(
            GenerationLifecycle::from_row_parts(Queue::Parked, 5, None, true, None)?,
            GenerationLifecycle::ResultRecorded {
                queue: Queue::Parked
            }
        );
        assert_eq!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, Some(6), false, Some(7))?,
            GenerationLifecycle::Consumed
        );

        // Parked command with a perform claim is impossible.
        assert!(matches!(
            GenerationLifecycle::from_row_parts(Queue::Parked, 5, Some(6), false, None),
            Err(CellError::LedgerCorrupt { seq: 5, .. })
        ));
        // Perform claim predating enqueue is impossible.
        assert!(matches!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, Some(4), false, None),
            Err(CellError::LedgerCorrupt { seq: 5, .. })
        ));
        // Consumption predating enqueue is impossible.
        assert!(matches!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, None, true, Some(4)),
            Err(CellError::LedgerCorrupt { seq: 5, .. })
        ));
        // Consumption predating perform claim is impossible.
        assert!(matches!(
            GenerationLifecycle::from_row_parts(Queue::Fifo, 5, Some(7), true, Some(6)),
            Err(CellError::LedgerCorrupt { seq: 5, .. })
        ));
        Ok(())
    }

    #[test]
    fn reconciliation_request_rejects_contradictory_payloads() -> Result<(), CellError> {
        let ok_payload = Value::Bool(true);
        let digest = crate::outbox_ledger::outcome_digest(&ok_payload)?;

        let confirmed = ReconciliationRequest::validate(
            "cmd-1",
            0,
            ReconciliationDecision::ConfirmedOutcome {
                ok: true,
                outcome_digest: digest,
            },
            Some(&ok_payload),
        )?;
        assert_eq!(
            confirmed,
            ReconciliationRequest::ConfirmedWithPayload {
                outcome: &ok_payload
            }
        );

        let diverged_ok = ReconciliationRequest::validate(
            "cmd-1",
            0,
            ReconciliationDecision::ConfirmedOutcome {
                ok: false,
                outcome_digest: digest,
            },
            Some(&ok_payload),
        );
        assert!(matches!(
            diverged_ok,
            Err(CellError::OutboxLedgerRefused {
                refusal: OutboxLedgerRefusal::OutcomeDiverged,
                ..
            })
        ));

        // Control decisions ignore any carried outcome payload so previously
        // accepted exports that recorded one still restore identically.
        for control in [
            ReconciliationDecision::AuthorizeReissue { next_generation: 1 },
            ReconciliationDecision::RequireCompensation,
        ] {
            assert_eq!(
                ReconciliationRequest::validate("cmd-1", 0, control, None)?,
                ReconciliationRequest::Control
            );
            assert_eq!(
                ReconciliationRequest::validate("cmd-1", 0, control, Some(&ok_payload))?,
                ReconciliationRequest::Control
            );
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "native"))]
mod native_tests {
    use super::RestoreFence;
    use crate::engine::{NativeEngine, SqlEngine as _, SqlValue};
    use crate::error::CellError;

    #[test]
    fn restore_fence_load_fails_closed_on_corrupt_meta_rows() -> Result<(), CellError> {
        let engine = NativeEngine::open_in_memory()?;
        engine.execute_batch(
            r#"CREATE TABLE "_norn_meta" ("key" TEXT PRIMARY KEY, "value" NOT NULL);"#,
        )?;
        assert_eq!(
            RestoreFence::load(&engine, "missing")?,
            RestoreFence::Unfenced
        );
        engine.execute(
            "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES (?1, ?2)",
            &[
                SqlValue::Text("restore:perform_fence:good".to_owned()),
                SqlValue::Text("3".to_owned()),
            ],
        )?;
        assert_eq!(
            RestoreFence::load(&engine, "good")?,
            RestoreFence::FencedAt { generation: 3 }
        );
        for (id, bad_val) in [
            ("non_canonical", SqlValue::Text("03".to_owned())),
            ("signed", SqlValue::Text("+3".to_owned())),
            ("int_storage", SqlValue::Integer(3)),
            ("blob_storage", SqlValue::Blob(vec![b'3'])),
        ] {
            engine.execute(
                "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES (?1, ?2)",
                &[
                    SqlValue::Text(format!("restore:perform_fence:{id}")),
                    bad_val,
                ],
            )?;
            let err = RestoreFence::load(&engine, id);
            assert!(
                matches!(err, Err(CellError::LedgerCorrupt { seq: 0, .. })),
                "expected LedgerCorrupt for {id}, got {err:?}"
            );
        }
        Ok(())
    }
}
