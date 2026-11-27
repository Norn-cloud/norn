//! Storage v11 migration: create `_norn_outbox_record` and backfill validated
//! [`norn_kernel::world::outbox_ledger::OperationLedger`] epochs from legacy
//! `_norn_outbox`, `_norn_perform_claim`, `_norn_run`, `_norn_export_journal`,
//! and restore fences.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::RuntimeValue as Value;
use norn_kernel::world::outbox::{OperationKey, Queue};
use norn_kernel::world::outbox_ledger::{
    ApplyStatus, DispatchProof, OperationLedger, OutboxDigest, OutboxLedgerRefusal, OutboxRecord,
    OutboxRecordKind, OutcomeEvidence, ProviderDedupContract, ReconciliationDecision,
};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{to_i64, to_u64};
use crate::system::{OUTBOX, OUTBOX_RECORD, PERFORM_CLAIM, RUN};

/// Provable historical enqueue metadata recovered from `_norn_export_journal`.
#[derive(Debug, Clone, Copy)]
struct JournalEnqueue {
    commit_seq: u64,
    queue: Queue,
    payload_digest: OutboxDigest,
    dedup_contract: ProviderDedupContract,
}

type JournalIndex = BTreeMap<String, BTreeMap<u64, JournalEnqueue>>;

/// Context carried while backfilling prior generations of one legacy outbox row.
struct PriorBackfillContext<'a> {
    command_id: &'a str,
    op: OperationKey,
    head_queue: Queue,
    head_generation: u64,
    head_commit_seq: u64,
    journal: Option<&'a BTreeMap<u64, JournalEnqueue>>,
}

/// Outcome of backfilling all `< head_generation` generations for one operation.
struct PriorBackfill {
    epoch: u64,
    epoch_queue: Option<Queue>,
    records: Vec<OutboxRecord>,
    deferred_outcomes: Vec<OutboxRecord>,
}

/// D1: create `_norn_outbox_record` and backfill validated epochs for every
/// existing `_norn_outbox` row from `_norn_outbox`, `_norn_perform_claim`,
/// `_norn_run`, `_norn_export_journal`, and `restore:perform_fence:*`.
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
    if rows.is_empty() {
        return Ok(());
    }
    let journal = index_journal_enqueues(engine)?;
    for values in rows {
        backfill_v11_outbox_row(engine, SqlRow(values), &journal)?;
    }
    Ok(())
}

/// Scan `_norn_export_journal` once to recover provable historical queue,
/// payload digest, and dedup contract for every enqueued `(command_id, gen)`.
fn index_journal_enqueues<E: SqlEngine>(engine: &E) -> Result<JournalIndex, CellError> {
    let mut index: JournalIndex = BTreeMap::new();
    let has_journal = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if !has_journal {
        return Ok(index);
    }
    let seqs = engine.query(
        "SELECT \"commit_seq\" FROM \"_norn_export_journal\" ORDER BY \"commit_seq\"",
        params![],
    )?;
    for values in seqs {
        let seq = to_u64(SqlRow(values).get::<i64>(0)?, "commit_seq")?;
        let Ok(Some(record)) = crate::export_journal::read_record(engine, seq) else {
            continue;
        };
        for effect in &record.batch.effects {
            let crate::store::EffectRow::Enqueue { command } = effect else {
                continue;
            };
            let entry = index.entry(command.command_id.clone()).or_default();
            if entry.contains_key(&command.generation) {
                continue;
            }
            entry.insert(
                command.generation,
                JournalEnqueue {
                    commit_seq: seq,
                    queue: crate::read::classify(&command.command),
                    payload_digest: crate::outbox_ledger::payload_digest(&command.command)?,
                    dedup_contract: crate::outbox_ledger::dedup_contract(&command.command)?,
                },
            );
        }
    }
    Ok(index)
}

fn backfill_v11_outbox_row<E: SqlEngine>(
    engine: &E,
    row: SqlRow,
    journal: &JournalIndex,
) -> Result<(), CellError> {
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
        head_queue: queue,
        head_generation: generation,
        head_commit_seq: commit_seq,
        journal: journal.get(&command_id),
    };
    let PriorBackfill {
        epoch,
        epoch_queue,
        mut records,
        deferred_outcomes,
    } = backfill_v11_prior_generations(engine, &ctx)?;
    let head_record_queue = if records.is_empty() {
        queue
    } else {
        epoch_queue.unwrap_or(queue)
    };
    records.push(OutboxRecord {
        operation: op,
        generation,
        kind: OutboxRecordKind::Requested {
            queue: head_record_queue,
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

fn backfill_v11_prior_generations<E: SqlEngine>(
    engine: &E,
    ctx: &PriorBackfillContext<'_>,
) -> Result<PriorBackfill, CellError> {
    let head_sql = to_i64(ctx.head_generation, "generation")?;
    let claim_rows = engine.query(
        &format!(
            "SELECT \"generation\", \"commit_seq\" FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" < ?2 ORDER BY \"generation\""
        ),
        params![ctx.command_id, head_sql],
    )?;
    let mut claims = BTreeMap::new();
    let mut gens = BTreeSet::new();
    for values in claim_rows {
        let row = SqlRow(values);
        let prior_gen = to_u64(row.get::<i64>(0)?, "generation")?;
        let claim_seq = to_u64(row.get::<i64>(1)?, "claim commit_seq")?;
        claims.insert(prior_gen, claim_seq);
        gens.insert(prior_gen);
    }
    if let Some(entries) = ctx.journal {
        for &prior_gen in entries.keys() {
            if prior_gen < ctx.head_generation {
                gens.insert(prior_gen);
            }
        }
    }
    let run_rows = engine.query(
        &format!(
            "SELECT DISTINCT \"generation\" FROM \"{RUN}\" WHERE \"command_id\" = ?1 AND \"generation\" < ?2"
        ),
        params![ctx.command_id, head_sql],
    )?;
    for values in run_rows {
        gens.insert(to_u64(SqlRow(values).get::<i64>(0)?, "generation")?);
    }
    let ordered: Vec<u64> = gens.into_iter().collect();
    let mut state = PriorBackfill {
        epoch: 0,
        epoch_queue: None,
        records: Vec::new(),
        deferred_outcomes: Vec::new(),
    };
    for (idx, prior_gen) in ordered.iter().copied().enumerate() {
        let next_generation = ordered
            .get(idx.saturating_add(1))
            .copied()
            .unwrap_or(ctx.head_generation);
        backfill_v11_one_prior_gen(
            engine,
            ctx,
            prior_gen,
            claims.get(&prior_gen).copied(),
            next_generation,
            &mut state,
        )?;
    }
    Ok(state)
}

fn backfill_v11_one_prior_gen<E: SqlEngine>(
    engine: &E,
    ctx: &PriorBackfillContext<'_>,
    prior_gen: u64,
    claim_seq: Option<u64>,
    next_generation: u64,
    state: &mut PriorBackfill,
) -> Result<(), CellError> {
    let origin_seq = engine
        .query_optional(
            "SELECT \"commit_seq\" FROM \"_norn_command_origin\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2",
            params![ctx.command_id, to_i64(prior_gen, "generation")?],
            |row| row.get::<i64>(0),
        )?
        .map(|seq| to_u64(seq, "origin commit_seq"))
        .transpose()?;
    let (enqueue_seq, gen_queue, digest, dedup) =
        if let Some(meta) = ctx.journal.and_then(|entries| entries.get(&prior_gen)) {
            (
                origin_seq.unwrap_or(meta.commit_seq),
                meta.queue,
                meta.payload_digest,
                meta.dedup_contract,
            )
        } else {
            let fallback_seq = origin_seq.or(claim_seq).unwrap_or(ctx.head_commit_seq);
            let fallback_queue = if claim_seq.is_some() {
                Queue::Fifo
            } else {
                ctx.head_queue
            };
            (
                fallback_seq,
                fallback_queue,
                legacy_unknown_payload_digest(ctx.command_id, prior_gen)?,
                ProviderDedupContract::None,
            )
        };
    let queue = *state.epoch_queue.get_or_insert(gen_queue);
    state.records.push(OutboxRecord {
        operation: ctx.op,
        generation: prior_gen,
        kind: OutboxRecordKind::Requested {
            queue,
            payload_digest: digest,
            dedup_contract: dedup,
            commit_seq: enqueue_seq,
        },
    });
    if let Some(claim_commit_seq) = claim_seq {
        state.records.push(OutboxRecord {
            operation: ctx.op,
            generation: prior_gen,
            kind: OutboxRecordKind::Dispatched { claim_commit_seq },
        });
    }
    let prior_outcome = backfill_v11_outcome(engine, ctx.command_id, prior_gen, enqueue_seq, None)?;
    finish_prior_gen(
        engine,
        ctx,
        prior_gen,
        enqueue_seq,
        claim_seq,
        next_generation,
        prior_outcome,
        state,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "completes one prior generation transition from backfill_v11_one_prior_gen"
)]
fn finish_prior_gen<E: SqlEngine>(
    engine: &E,
    ctx: &PriorBackfillContext<'_>,
    prior_gen: u64,
    enqueue_seq: u64,
    claim_seq: Option<u64>,
    next_generation: u64,
    prior_outcome: Option<(Value, Option<u64>)>,
    state: &mut PriorBackfill,
) -> Result<(), CellError> {
    match (claim_seq, prior_outcome) {
        (claim, Some((outcome, Some(consumed_commit_seq)))) => {
            state.records.push(prior_outcome_record(
                ctx.op,
                prior_gen,
                enqueue_seq,
                claim,
                &outcome,
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
            state.epoch_queue = None;
            state.epoch = state
                .epoch
                .checked_add(1)
                .ok_or(CellError::OutOfRange("outbox epoch"))?;
        }
        (claim, Some((outcome, None))) if !crate::outbox_ledger::outcome_is_ok(&outcome) => {
            state.records.push(prior_outcome_record(
                ctx.op,
                prior_gen,
                enqueue_seq,
                claim,
                &outcome,
            )?);
        }
        (Some(claim), Some((outcome, None))) => {
            state.records.push(OutboxRecord {
                operation: ctx.op,
                generation: prior_gen,
                kind: OutboxRecordKind::Reconciliation {
                    decision: ReconciliationDecision::AuthorizeReissue { next_generation },
                    commit_seq: ctx.head_commit_seq,
                },
            });
            state.deferred_outcomes.push(prior_outcome_record(
                ctx.op,
                prior_gen,
                enqueue_seq,
                Some(claim),
                &outcome,
            )?);
        }
        (Some(_), None) => {
            state.records.push(OutboxRecord {
                operation: ctx.op,
                generation: prior_gen,
                kind: OutboxRecordKind::Reconciliation {
                    decision: ReconciliationDecision::AuthorizeReissue { next_generation },
                    commit_seq: ctx.head_commit_seq,
                },
            });
        }
        (None, Some((_, None)) | None) => {}
    }
    Ok(())
}

fn prior_outcome_record(
    op: OperationKey,
    generation: u64,
    enqueue_seq: u64,
    claim_seq: Option<u64>,
    outcome: &Value,
) -> Result<OutboxRecord, CellError> {
    let (evidence, commit_seq) = if let Some(claim_commit_seq) = claim_seq {
        (
            OutcomeEvidence::DispatchProof {
                proof: DispatchProof {
                    operation: op,
                    generation,
                    claim_commit_seq,
                },
            },
            claim_commit_seq,
        )
    } else {
        (OutcomeEvidence::Resolved, enqueue_seq)
    };
    Ok(OutboxRecord {
        operation: op,
        generation,
        kind: OutboxRecordKind::Outcome {
            evidence,
            ok: crate::outbox_ledger::outcome_is_ok(outcome),
            outcome_digest: crate::outbox_ledger::outcome_digest(outcome)?,
            commit_seq,
        },
    })
}

/// Domain-separated digest representing an unprovable pre-v11 prior-generation
/// payload whose original enqueue record is absent from the store. Matches no
/// real [`crate::outbox_ledger::payload_digest`] so any future same-generation
/// replay fails closed.
fn legacy_unknown_payload_digest(
    command_id: &str,
    generation: u64,
) -> Result<OutboxDigest, CellError> {
    let hex = norn_ir::canonical::domain_hash_hex(
        b"norn.outbox.legacy_unknown_payload.v1\0",
        &(command_id, generation),
    )
    .map_err(CellError::Encoding)?;
    OutboxDigest::from_hex(&hex)
        .ok_or_else(|| CellError::Encoding(format!("invalid legacy digest hex: {hex}")))
}

/// Apply a retained pre-v11 in-place outbox replacement that changed queue or
/// superseded an ambiguous dispatch before v11 reconciliation existed.
pub(crate) fn apply_legacy_replacement<E: SqlEngine>(
    conn: &E,
    ledger: &mut OperationLedger,
    epoch: u64,
    command_id: &str,
    record: &OutboxRecord,
    commit_seq: u64,
) -> Result<crate::outbox_ledger::EnqueueAction, CellError> {
    let refused = |reason: String| CellError::OutboxReissueRefused {
        command_id: command_id.to_owned(),
        generation: record.generation,
        reason,
    };
    match ledger.apply(record) {
        Ok(ApplyStatus::Applied) => {
            crate::outbox_ledger::append_record(conn, command_id, epoch, record)?;
            Ok(crate::outbox_ledger::EnqueueAction::ReplaceInPlace)
        }
        Ok(ApplyStatus::IdempotentDuplicate) => Ok(crate::outbox_ledger::EnqueueAction::Noop),
        Err(OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation { .. }) => {
            let auth = OutboxRecord {
                operation: ledger.operation(),
                generation: ledger.active_generation(),
                kind: OutboxRecordKind::Reconciliation {
                    decision: ReconciliationDecision::AuthorizeReissue {
                        next_generation: record.generation,
                    },
                    commit_seq,
                },
            };
            ledger
                .apply(&auth)
                .map_err(|err| refused(err.to_string()))?;
            ledger
                .apply(record)
                .map_err(|err| refused(err.to_string()))?;
            crate::outbox_ledger::append_record(conn, command_id, epoch, &auth)?;
            crate::outbox_ledger::append_record(conn, command_id, epoch, record)?;
            Ok(crate::outbox_ledger::EnqueueAction::ReplaceInPlace)
        }
        Err(error) => Err(refused(error.to_string())),
    }
}
