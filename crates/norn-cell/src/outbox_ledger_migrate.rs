//! Storage v11 migration: create `_norn_outbox_record` and backfill validated
//! [`norn_kernel::world::outbox_ledger::OperationLedger`] epochs from legacy
//! `_norn_outbox`, `_norn_perform_claim`, `_norn_run`, and restore fences.

use norn_ir::RuntimeValue as Value;
use norn_kernel::world::outbox::{OperationKey, Queue};
use norn_kernel::world::outbox_ledger::{
    DispatchProof, OutboxDigest, OutboxRecord, OutboxRecordKind, OutcomeEvidence,
    ProviderDedupContract, ReconciliationDecision,
};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{to_i64, to_u64};
use crate::system::{OUTBOX, OUTBOX_RECORD, PERFORM_CLAIM, RUN};

/// Context carried while backfilling prior generations of one legacy outbox row.
struct PriorBackfillContext<'a> {
    command_id: &'a str,
    op: OperationKey,
    queue: Queue,
    head_generation: u64,
    head_commit_seq: u64,
}

/// Outcome of backfilling all `< head_generation` claims for one operation.
struct PriorBackfill {
    epoch: u64,
    records: Vec<OutboxRecord>,
    deferred_outcomes: Vec<OutboxRecord>,
}

/// D1: create `_norn_outbox_record` and backfill validated epochs for every
/// existing `_norn_outbox` row from `_norn_outbox`, `_norn_perform_claim`,
/// `_norn_run`, and `restore:perform_fence:*`.
pub(crate) fn migrate_v11_outbox_ledger<E: SqlEngine>(engine: &E) -> Result<(), CellError> {
    let has_outbox = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_outbox'",
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if !has_outbox {
        return Ok(());
    }
    engine.execute_batch(crate::outbox_ledger::DDL)?;
    let has_records = engine
        .query_optional(
            &format!("SELECT 1 FROM \"{OUTBOX_RECORD}\" LIMIT 1"),
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if has_records {
        return Ok(());
    }
    let rows = engine.query(
        &format!(
            "SELECT \"command_id\", \"generation\", \"commit_seq\", \"command\", \"result\", \"consumed_commit_seq\", \"queue\" FROM \"{OUTBOX}\" ORDER BY \"position\""
        ),
        params![],
    )?;
    for values in rows {
        backfill_v11_outbox_row(engine, SqlRow(values))?;
    }
    Ok(())
}

fn backfill_v11_outbox_row<E: SqlEngine>(engine: &E, row: SqlRow) -> Result<(), CellError> {
    let command_id: String = row.get(0)?;
    let generation = to_u64(row.get::<i64>(1)?, "generation")?;
    let commit_seq = to_u64(row.get::<i64>(2)?, "commit_seq")?;
    let command_text: String = row.get(3)?;
    let result_text: Option<String> = row.get(4)?;
    let consumed_seq = row
        .get::<Option<i64>>(5)?
        .map(|seq| to_u64(seq, "consumed_commit_seq"))
        .transpose()?;
    let queue_text: String = row.get(6)?;
    let queue = Queue::parse(&queue_text).ok_or_else(|| CellError::CorruptValue {
        table: OUTBOX.to_owned(),
        column: "queue".to_owned(),
    })?;
    let command_value: Value = serde_json::from_str(&command_text)?;
    let op = crate::outbox_ledger::operation_key(engine, &command_id)?;
    let head_digest = crate::outbox_ledger::payload_digest(&command_value)?;
    let head_dedup = crate::outbox_ledger::dedup_contract(&command_value)?;
    let ctx = PriorBackfillContext {
        command_id: &command_id,
        op,
        queue,
        head_generation: generation,
        head_commit_seq: commit_seq,
    };
    let PriorBackfill {
        epoch,
        mut records,
        deferred_outcomes,
    } = backfill_v11_prior_claims(engine, &ctx)?;
    records.push(OutboxRecord {
        operation: op,
        generation,
        kind: OutboxRecordKind::Requested {
            queue,
            payload_digest: head_digest,
            dedup_contract: head_dedup,
            commit_seq,
        },
    });
    records.extend(deferred_outcomes);
    let dispatch = append_head_dispatch(engine, &command_id, op, generation, &mut records)?;
    if let Some((outcome, _)) =
        backfill_v11_outcome(engine, &command_id, generation, commit_seq, result_text)?
    {
        let (evidence, outcome_seq) = if let Some(proof) = dispatch {
            (
                OutcomeEvidence::DispatchProof { proof },
                proof.claim_commit_seq,
            )
        } else {
            (OutcomeEvidence::Resolved, commit_seq)
        };
        records.push(OutboxRecord {
            operation: op,
            generation,
            kind: OutboxRecordKind::Outcome {
                evidence,
                ok: crate::outbox_ledger::outcome_is_ok(&outcome),
                outcome_digest: crate::outbox_ledger::outcome_digest(&outcome)?,
                commit_seq: outcome_seq,
            },
        });
    }
    if consumed_seq.is_none()
        && let Some(fenced_at) = crate::outbox_ledger::read_meta_fence(engine, &command_id)?
        && generation <= fenced_at
    {
        records.push(OutboxRecord {
            operation: op,
            generation,
            kind: OutboxRecordKind::RestoreFenced { commit_seq },
        });
    }
    if let Some(consumed_commit_seq) = consumed_seq {
        records.push(OutboxRecord {
            operation: op,
            generation,
            kind: OutboxRecordKind::Consumed {
                commit_seq: consumed_commit_seq,
            },
        });
    }
    persist_epoch(engine, &command_id, epoch, &records)
}

fn append_head_dispatch<E: SqlEngine>(
    engine: &E,
    command_id: &str,
    op: OperationKey,
    generation: u64,
    records: &mut Vec<OutboxRecord>,
) -> Result<Option<DispatchProof>, CellError> {
    let claim_seq: Option<i64> = engine.query_optional(
        &format!(
            "SELECT \"commit_seq\" FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
        ),
        params![command_id, to_i64(generation, "generation")?],
        |claim| claim.get(0),
    )?;
    let Some(claim_seq) = claim_seq else {
        return Ok(None);
    };
    let claim_commit_seq = to_u64(claim_seq, "claim commit_seq")?;
    records.push(OutboxRecord {
        operation: op,
        generation,
        kind: OutboxRecordKind::Dispatched { claim_commit_seq },
    });
    Ok(Some(DispatchProof {
        operation: op,
        generation,
        claim_commit_seq,
    }))
}

fn persist_epoch<E: SqlEngine>(
    engine: &E,
    command_id: &str,
    epoch: u64,
    records: &[OutboxRecord],
) -> Result<(), CellError> {
    let _replayed = crate::outbox_ledger::replay_records(records)?;
    for record in records {
        crate::outbox_ledger::append_record(engine, command_id, epoch, record)?;
    }
    Ok(())
}

fn backfill_v11_outcome<E: SqlEngine>(
    engine: &E,
    command_id: &str,
    generation: u64,
    commit_seq: u64,
    result_text: Option<String>,
) -> Result<Option<(Value, Option<u64>)>, CellError> {
    if let Some(encoded) = result_text {
        let world = crate::payload_storage::receipt_world(engine, commit_seq)?;
        let decoded = crate::payload_storage::decode_result(engine, &encoded, Some(&world))?;
        return Ok(Some((decoded, None)));
    }
    let run_row: Option<(String, Option<i64>)> = engine.query_optional(
        &format!(
            "SELECT \"outcome\", \"consumed_commit_seq\" FROM \"{RUN}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2 ORDER BY \"attempt\" LIMIT 1"
        ),
        params![command_id, to_i64(generation, "generation")?],
        |run| Ok((run.get(0)?, run.get(1)?)),
    )?;
    let Some((encoded, consumed_sql)) = run_row else {
        return Ok(None);
    };
    let world = crate::payload_storage::command_world(engine, command_id, generation)?;
    let decoded = crate::payload_storage::decode_result(engine, &encoded, world.as_deref())?;
    let consumed = consumed_sql
        .map(|seq| to_u64(seq, "consumed_commit_seq"))
        .transpose()?;
    Ok(Some((decoded, consumed)))
}

fn backfill_v11_prior_claims<E: SqlEngine>(
    engine: &E,
    ctx: &PriorBackfillContext<'_>,
) -> Result<PriorBackfill, CellError> {
    let prior_rows = engine.query(
        &format!(
            "SELECT \"generation\", \"commit_seq\" FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" < ?2 ORDER BY \"generation\""
        ),
        params![ctx.command_id, to_i64(ctx.head_generation, "generation")?],
    )?;
    let mut claims = Vec::with_capacity(prior_rows.len());
    for values in prior_rows {
        let row = SqlRow(values);
        let prior_gen = to_u64(row.get::<i64>(0)?, "generation")?;
        let claim_seq = to_u64(row.get::<i64>(1)?, "claim commit_seq")?;
        claims.push((prior_gen, claim_seq));
    }
    let mut state = PriorBackfill {
        epoch: 0,
        records: Vec::new(),
        deferred_outcomes: Vec::new(),
    };
    for (idx, (prior_gen, claim_seq)) in claims.iter().copied().enumerate() {
        let next_generation = claims
            .get(idx.saturating_add(1))
            .map_or(ctx.head_generation, |(next_gen, _)| *next_gen);
        backfill_v11_one_prior_claim(
            engine,
            ctx,
            prior_gen,
            claim_seq,
            next_generation,
            &mut state,
        )?;
    }
    Ok(state)
}

fn backfill_v11_one_prior_claim<E: SqlEngine>(
    engine: &E,
    ctx: &PriorBackfillContext<'_>,
    prior_gen: u64,
    claim_seq: u64,
    next_generation: u64,
    state: &mut PriorBackfill,
) -> Result<(), CellError> {
    let enqueue_seq = engine
        .query_optional(
            "SELECT \"commit_seq\" FROM \"_norn_command_origin\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2",
            params![ctx.command_id, to_i64(prior_gen, "generation")?],
            |row| row.get::<i64>(0),
        )?
        .map(|seq| to_u64(seq, "origin commit_seq"))
        .transpose()?
        .unwrap_or(claim_seq);
    let (digest, dedup) =
        backfill_v11_prior_command_meta(engine, ctx.command_id, prior_gen, enqueue_seq, claim_seq)?;
    state.records.push(OutboxRecord {
        operation: ctx.op,
        generation: prior_gen,
        kind: OutboxRecordKind::Requested {
            queue: ctx.queue,
            payload_digest: digest,
            dedup_contract: dedup,
            commit_seq: enqueue_seq,
        },
    });
    state.records.push(OutboxRecord {
        operation: ctx.op,
        generation: prior_gen,
        kind: OutboxRecordKind::Dispatched {
            claim_commit_seq: claim_seq,
        },
    });
    match backfill_v11_outcome(engine, ctx.command_id, prior_gen, enqueue_seq, None)? {
        Some((outcome, Some(consumed_commit_seq))) => {
            state.records.push(prior_outcome_record(
                ctx.op, prior_gen, claim_seq, &outcome,
            )?);
            state.records.append(&mut state.deferred_outcomes);
            state.records.push(OutboxRecord {
                operation: ctx.op,
                generation: prior_gen,
                kind: OutboxRecordKind::Consumed {
                    commit_seq: consumed_commit_seq,
                },
            });
            persist_epoch(engine, ctx.command_id, state.epoch, &state.records)?;
            state.records.clear();
            state.epoch = state
                .epoch
                .checked_add(1)
                .ok_or(CellError::OutOfRange("outbox epoch"))?;
        }
        Some((outcome, None)) if !crate::outbox_ledger::outcome_is_ok(&outcome) => {
            state.records.push(prior_outcome_record(
                ctx.op, prior_gen, claim_seq, &outcome,
            )?);
        }
        Some((outcome, None)) => {
            state.records.push(OutboxRecord {
                operation: ctx.op,
                generation: prior_gen,
                kind: OutboxRecordKind::Reconciliation {
                    decision: ReconciliationDecision::AuthorizeReissue { next_generation },
                    commit_seq: ctx.head_commit_seq,
                },
            });
            state.deferred_outcomes.push(prior_outcome_record(
                ctx.op, prior_gen, claim_seq, &outcome,
            )?);
        }
        None => {
            state.records.push(OutboxRecord {
                operation: ctx.op,
                generation: prior_gen,
                kind: OutboxRecordKind::Reconciliation {
                    decision: ReconciliationDecision::AuthorizeReissue { next_generation },
                    commit_seq: ctx.head_commit_seq,
                },
            });
        }
    }
    Ok(())
}

fn prior_outcome_record(
    op: OperationKey,
    generation: u64,
    claim_commit_seq: u64,
    outcome: &Value,
) -> Result<OutboxRecord, CellError> {
    Ok(OutboxRecord {
        operation: op,
        generation,
        kind: OutboxRecordKind::Outcome {
            evidence: OutcomeEvidence::DispatchProof {
                proof: DispatchProof {
                    operation: op,
                    generation,
                    claim_commit_seq,
                },
            },
            ok: crate::outbox_ledger::outcome_is_ok(outcome),
            outcome_digest: crate::outbox_ledger::outcome_digest(outcome)?,
            commit_seq: claim_commit_seq,
        },
    })
}

fn backfill_v11_prior_command_meta<E: SqlEngine>(
    engine: &E,
    command_id: &str,
    generation: u64,
    enqueue_seq: u64,
    claim_seq: u64,
) -> Result<(OutboxDigest, ProviderDedupContract), CellError> {
    if let Some(meta) = find_enqueued_meta_at(engine, command_id, generation, enqueue_seq)? {
        return Ok(meta);
    }
    let has_journal = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if has_journal {
        let seqs = engine.query(
            "SELECT \"commit_seq\" FROM \"_norn_export_journal\" WHERE \"commit_seq\" <= ?1 ORDER BY \"commit_seq\" DESC",
            params![to_i64(claim_seq, "claim commit_seq")?],
        )?;
        for values in seqs {
            let seq = to_u64(SqlRow(values).get::<i64>(0)?, "commit_seq")?;
            if seq == enqueue_seq {
                continue;
            }
            if let Some(meta) = find_enqueued_meta_at(engine, command_id, generation, seq)? {
                return Ok(meta);
            }
        }
    }
    // No provable prior payload exists in the legacy store: bind a
    // domain-separated sentinel digest and fail-closed `ProviderDedupContract::None`
    // rather than fabricating the head generation's payload identity.
    Ok((
        crate::outbox_ledger::legacy_unknown_payload_digest(command_id, generation)?,
        ProviderDedupContract::None,
    ))
}

fn find_enqueued_meta_at<E: SqlEngine>(
    engine: &E,
    command_id: &str,
    generation: u64,
    commit_seq: u64,
) -> Result<Option<(OutboxDigest, ProviderDedupContract)>, CellError> {
    let Ok(Some(record)) = crate::export_journal::read_record(engine, commit_seq) else {
        return Ok(None);
    };
    let found = record.batch.effects.iter().find_map(|effect| match effect {
        crate::store::EffectRow::Enqueue { command }
            if command.command_id == command_id && command.generation == generation =>
        {
            Some(&command.command)
        }
        crate::store::EffectRow::Enqueue { .. }
        | crate::store::EffectRow::ClaimPerform { .. }
        | crate::store::EffectRow::ReconcileOutbox { .. }
        | crate::store::EffectRow::RecordCompensation { .. }
        | crate::store::EffectRow::RecordLateOutcome { .. }
        | crate::store::EffectRow::Schedule { .. }
        | crate::store::EffectRow::CancelTimer { .. } => None,
    });
    match found {
        Some(command) => Ok(Some((
            crate::outbox_ledger::payload_digest(command)?,
            crate::outbox_ledger::dedup_contract(command)?,
        ))),
        None => Ok(None),
    }
}
