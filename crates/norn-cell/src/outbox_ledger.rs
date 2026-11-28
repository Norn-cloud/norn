//! D1: immutable per-generation outbox ledger and reconciliation store.
//!
//! Persists append-only [`OutboxRecord`] rows in `_norn_outbox_record` keyed by
//! `(command_id, epoch, seq)` and validates every transition through
//! [`OperationLedger`]. The mutable `_norn_outbox` table acts strictly as the
//! projected queue head ([`HeadProjection`]):
//!
//! - Old results never settle a newer active generation.
//! - Late outcomes attach immutably to their original generation via a verified
//!   [`DispatchProof`] without mutating the active head.
//! - Same-generation payload or deduplication-contract divergence fails closed
//!   with [`OutboxLedgerRefusal::PayloadDiverged`].
//! - Ambiguous dispatches and restore fences require durable reconciliation
//!   ([`ReconciliationDecision::AuthorizeReissue`]) or a declared
//!   [`ProviderDedupContract::OperationKeyIdempotent`] contract with an
//!   unchanged payload digest (and no active restore fence) before reissue.

use norn_ir::RuntimeValue as Value;
use norn_ir::canonical::HashProfile;
use norn_ir::cell_id::CellId;
use norn_kernel::CommandId;
use norn_kernel::world::outbox::{OperationKey, Queue};
use norn_kernel::world::outbox_ledger::{
    ApplyStatus, DispatchProof, OperationLedger, OutboxDigest, OutboxLedgerRefusal, OutboxRecord,
    OutboxRecordKind, OutcomeEvidence, ProviderDedupContract, ReconciliationDecision,
};

use crate::codec::canonical_json;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::outbox_generations::{
    ActiveRunReplay, CompensationProgress, ProofOutcomeTarget, ReconciliationTarget, RestoreFence,
};
use crate::read::{RunRecord, to_i64, to_u64};
use crate::store::OutboxCommand;
use crate::system::OUTBOX_RECORD;

/// Deterministic local-only fallback identity for legacy cells without a
/// persisted `binding:cell_id`.
const LOCAL_FALLBACK_CELL_ID: &str = "00000000000000000000000000000000";

/// DDL creating the immutable append-only outbox record ledger (storage v11).
pub(crate) const DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_outbox_record" (
  "command_id" TEXT NOT NULL,
  "epoch" INTEGER NOT NULL,
  "seq" INTEGER NOT NULL,
  "generation" INTEGER NOT NULL,
  "record" TEXT NOT NULL,
  PRIMARY KEY ("command_id", "epoch", "seq")
)"#;

/// What `_norn_outbox` must do to project an admitted [`OutboxRecordKind::Requested`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueAction {
    /// Identical same-generation duplicate or older never-requested generation.
    Noop,
    /// First enqueue, or a fresh epoch after a consumed prior epoch.
    InsertNew,
    /// Newer generation replacing an active pending head in place.
    ReplaceInPlace,
}

/// Whether replaying an epoch against an effective [`RestoreFence`] synthesized
/// an unpersisted `RestoreFenced` record (`D3`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum FenceProjection {
    None,
    Synthesized(Box<OutboxRecord>),
}

/// Replayed epoch ledger paired with any in-memory synthesized fence record.
struct ReplayedEpoch {
    ledger: OperationLedger,
    pending_fence: FenceProjection,
}

/// Derive the generation-free [`OperationKey`] for `command_id` in `cell_id`.
///
/// Legacy stores without a persisted `binding:cell_id` use a fixed local-only
/// fallback identity so outbox operations still get a deterministic key.
///
/// # Errors
///
/// Canonical encoding or digest derivation failures.
pub fn operation_key_for_cell(
    cell_id: Option<&CellId>,
    command_id: &str,
) -> Result<OperationKey, CellError> {
    let fallback = CellId::try_from(LOCAL_FALLBACK_CELL_ID.to_owned())
        .map_err(|reason| CellError::Encoding(reason.to_owned()))?;
    let effective = cell_id.unwrap_or(&fallback);
    OperationKey::derive(effective, &CommandId(command_id.to_owned())).map_err(CellError::Encoding)
}

/// Canonical SHA-256 [`OutboxDigest`] of an enqueued command payload.
///
/// Generation-varying top-level envelope fields (`generation`,
/// `idempotency_key`, `idempotencyKey`) are excluded so
/// [`ProviderDedupContract::OperationKeyIdempotent`] retries across generations
/// compare equal iff their business payload and policy fields match.
///
/// # Errors
///
/// Canonical JSON encoding or digest decoding failures.
pub fn payload_digest(command: &Value) -> Result<OutboxDigest, CellError> {
    let canonical = match command {
        Value::Object(map) => {
            let mut normalized = map.clone();
            normalized.remove("generation");
            normalized.remove("idempotency_key");
            normalized.remove("idempotencyKey");
            canonical_json(&Value::Object(normalized))?
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            canonical_json(command)?
        }
    };
    digest_canonical_str(&canonical)
}

/// Canonical SHA-256 [`OutboxDigest`] of a recorded outcome payload.
///
/// # Errors
///
/// Canonical JSON encoding or digest decoding failures.
pub fn outcome_digest(outcome: &Value) -> Result<OutboxDigest, CellError> {
    let canonical = canonical_json(outcome)?;
    digest_canonical_str(&canonical)
}

/// Extract the declared [`ProviderDedupContract`] from a command value.
///
/// Defaults fail-closed to [`ProviderDedupContract::None`]. An unrecognized
/// top-level `dedupContract` value is refused rather than silently downgraded.
///
/// # Errors
///
/// [`CellError::Encoding`] if top-level `dedupContract` is present and not a
/// recognized contract string.
pub fn dedup_contract(command: &Value) -> Result<ProviderDedupContract, CellError> {
    if let Some(top) = command.get("dedupContract") {
        return parse_dedup_contract(top, true);
    }
    if let Some(nested) = command
        .get("payload")
        .and_then(|payload| payload.get("dedupContract"))
    {
        return parse_dedup_contract(nested, false);
    }
    Ok(ProviderDedupContract::None)
}

fn parse_dedup_contract(value: &Value, strict: bool) -> Result<ProviderDedupContract, CellError> {
    match value {
        Value::Null => Ok(ProviderDedupContract::None),
        Value::String(text) if text == "operationKeyIdempotent" => {
            Ok(ProviderDedupContract::OperationKeyIdempotent)
        }
        Value::String(text) if text == "none" => Ok(ProviderDedupContract::None),
        Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => {
            if strict {
                Err(CellError::Encoding(
                    "unknown outbox dedupContract value".to_owned(),
                ))
            } else {
                Ok(ProviderDedupContract::None)
            }
        }
    }
}

/// Classify whether a recorded outcome value represents success (`true`) or
/// failure (`false`).
#[must_use]
pub fn outcome_is_ok(outcome: &Value) -> bool {
    if let Some(ok) = outcome.get("ok").and_then(Value::as_bool) {
        return ok;
    }
    if outcome
        .get("error")
        .is_some_and(|error| !error.is_null() && error != &Value::Bool(false))
    {
        return false;
    }
    if let Some(status) = outcome.get("status").and_then(Value::as_str)
        && matches!(status, "failed" | "error" | "rejected" | "denied")
    {
        return false;
    }
    true
}

fn digest_canonical_str(canonical: &str) -> Result<OutboxDigest, CellError> {
    let hex = HashProfile::Norn1.hash_canonical_bytes(canonical.as_bytes());
    OutboxDigest::from_hex(&hex)
        .ok_or_else(|| CellError::Encoding(format!("invalid outbox digest hex: {hex}")))
}

pub(crate) fn operation_key<E: SqlEngine>(
    conn: &E,
    command_id: &str,
) -> Result<OperationKey, CellError> {
    let cell_id = crate::cell_identity::read(conn)?;
    operation_key_for_cell(cell_id.as_ref(), command_id)
}

pub(crate) fn load_epochs<E: SqlEngine>(
    conn: &E,
    command_id: &str,
) -> Result<Vec<(u64, Vec<OutboxRecord>)>, CellError> {
    let rows = conn.query(
        &format!(
            "SELECT \"epoch\", \"seq\", \"generation\", \"record\" FROM \"{OUTBOX_RECORD}\" WHERE \"command_id\" = ?1 ORDER BY \"epoch\", \"seq\""
        ),
        params![command_id],
    )?;
    let corrupt = |reason: String| CellError::LedgerCorrupt { seq: 0, reason };
    let mut epochs: Vec<(u64, Vec<OutboxRecord>)> = Vec::new();
    let mut expected_seq: u64 = 1;
    for values in rows {
        let row = SqlRow(values);
        let epoch = to_u64(row.get::<i64>(0)?, "outbox epoch")?;
        let seq = to_u64(row.get::<i64>(1)?, "outbox record seq")?;
        let col_gen = to_u64(row.get::<i64>(2)?, "generation")?;
        let text: String = row.get(3)?;
        let record: OutboxRecord = serde_json::from_str(&text)?;
        if col_gen != record.generation {
            return Err(corrupt(format!(
                "outbox record generation column ({col_gen}) disagrees with payload ({})",
                record.generation
            )));
        }
        match epochs.last_mut() {
            Some((current, list)) if *current == epoch => {
                if seq != expected_seq {
                    return Err(corrupt(format!(
                        "outbox record seq gap in epoch {epoch}: expected {expected_seq}, got {seq}"
                    )));
                }
                list.push(record);
                expected_seq = seq
                    .checked_add(1)
                    .ok_or(CellError::OutOfRange("outbox record seq"))?;
            }
            last => {
                let expected_epoch = match last {
                    Some((current, _)) => current
                        .checked_add(1)
                        .ok_or(CellError::OutOfRange("outbox epoch"))?,
                    None => 0,
                };
                if epoch != expected_epoch || seq != 1 {
                    return Err(corrupt(format!(
                        "outbox record epoch/seq discontinuity: expected ({expected_epoch}, 1), got ({epoch}, {seq})"
                    )));
                }
                epochs.push((epoch, vec![record]));
                expected_seq = 2;
            }
        }
    }
    Ok(epochs)
}

pub(crate) fn replay_records(records: &[OutboxRecord]) -> Result<OperationLedger, CellError> {
    OperationLedger::replay(records).map_err(|error| CellError::LedgerCorrupt {
        seq: 0,
        reason: format!("outbox ledger replay failed: {error}"),
    })
}

fn apply_meta_fence_in_memory(
    ledger: &mut OperationLedger,
    meta_fence: RestoreFence,
) -> Result<FenceProjection, CellError> {
    let RestoreFence::FencedAt {
        generation: fenced_at,
    } = meta_fence
    else {
        return Ok(FenceProjection::None);
    };
    if ledger.is_terminal()
        || ledger.fence().is_some_and(|existing| existing >= fenced_at)
        || !meta_fence.blocks(ledger.active_generation())
    {
        return Ok(FenceProjection::None);
    }
    let fence_record = OutboxRecord {
        operation: ledger.operation(),
        generation: ledger.active_generation(),
        kind: OutboxRecordKind::RestoreFenced {
            commit_seq: ledger.active().enqueue_commit_seq,
        },
    };
    ledger
        .apply(&fence_record)
        .map_err(|error| CellError::LedgerCorrupt {
            seq: 0,
            reason: format!("failed to apply restore fence to outbox ledger: {error}"),
        })?;
    Ok(FenceProjection::Synthesized(Box::new(fence_record)))
}

fn replay_epoch_with_fence(
    records: &[OutboxRecord],
    meta_fence: RestoreFence,
) -> Result<ReplayedEpoch, CellError> {
    let mut ledger = replay_records(records)?;
    let pending_fence = apply_meta_fence_in_memory(&mut ledger, meta_fence)?;
    Ok(ReplayedEpoch {
        ledger,
        pending_fence,
    })
}

pub(crate) fn append_record<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    epoch: u64,
    record: &OutboxRecord,
) -> Result<u64, CellError> {
    let epoch_sql = to_i64(epoch, "outbox epoch")?;
    let max_seq: i64 = conn.query_row(
        &format!(
            "SELECT COALESCE(MAX(\"seq\"), 0) FROM \"{OUTBOX_RECORD}\" WHERE \"command_id\" = ?1 AND \"epoch\" = ?2"
        ),
        params![command_id, epoch_sql],
        |row| row.get(0),
    )?;
    let next_seq = to_u64(max_seq, "outbox record seq")?
        .checked_add(1)
        .ok_or(CellError::OutOfRange("outbox record seq"))?;
    let serialized = canonical_json(record)?;
    conn.execute(
        &format!(
            "INSERT INTO \"{OUTBOX_RECORD}\" (\"command_id\", \"epoch\", \"seq\", \"generation\", \"record\") VALUES (?1, ?2, ?3, ?4, ?5)"
        ),
        params![
            command_id,
            epoch_sql,
            to_i64(next_seq, "outbox record seq")?,
            to_i64(record.generation, "generation")?,
            serialized
        ],
    )?;
    Ok(next_seq)
}

/// Read and replay the active (latest epoch) [`OperationLedger`] for `command_id`.
pub(crate) fn load_active_ledger<E: SqlEngine>(
    conn: &E,
    command_id: &str,
) -> Result<Option<OperationLedger>, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((_, records)) = epochs.last() else {
        return Ok(None);
    };
    let meta_fence = RestoreFence::load(conn, command_id)?;
    Ok(Some(replay_epoch_with_fence(records, meta_fence)?.ledger))
}

/// Read every persisted immutable [`OutboxRecord`] of `command_id` across all
/// epochs, in append order.
pub(crate) fn load_all_records<E: SqlEngine>(
    conn: &E,
    command_id: &str,
) -> Result<Vec<OutboxRecord>, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    Ok(epochs.into_iter().flat_map(|(_, list)| list).collect())
}

/// Look up the immutable [`DispatchProof`] of `(command_id, generation)` across
/// all epochs.
pub(crate) fn load_dispatch_proof<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
) -> Result<Option<DispatchProof>, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    for (_, records) in epochs.iter().rev() {
        let ledger = replay_records(records)?;
        if let Some(gen_rec) = ledger.generation(generation) {
            return Ok(gen_rec.dispatch);
        }
    }
    Ok(None)
}

/// Validate and append an [`OutboxRecordKind::Requested`] record for `command`.
///
/// When `legacy_outbox` is `true` (replaying an already-committed pre-v11 export
/// record), pre-v11 same-generation payload divergence degrades to a no-op and
/// pre-v11 unreconciled reissues over a claimed or locally-succeeded prior
/// generation record a replay-time `AuthorizeReissue` transition so the
/// reconstructed ledger still satisfies every [`OperationLedger`] invariant.
pub(crate) fn record_enqueue<E: SqlEngine>(
    conn: &E,
    command: &OutboxCommand,
    queue: Queue,
    commit_seq: u64,
    legacy_outbox: bool,
) -> Result<EnqueueAction, CellError> {
    let op = operation_key(conn, &command.command_id)?;
    let digest = payload_digest(&command.command)?;
    let dedup = dedup_contract(&command.command)?;
    let record = OutboxRecord {
        operation: op,
        generation: command.generation,
        kind: OutboxRecordKind::Requested {
            queue,
            payload_digest: digest,
            dedup_contract: dedup,
            commit_seq,
        },
    };
    let refused = |reason: String| CellError::OutboxReissueRefused {
        command_id: command.command_id.clone(),
        generation: command.generation,
        reason,
    };
    let epochs = load_epochs(conn, &command.command_id)?;
    if epochs.is_empty() {
        let _started = OperationLedger::start(&record).map_err(|err| refused(err.to_string()))?;
        append_record(conn, &command.command_id, 0, &record)?;
        return Ok(EnqueueAction::InsertNew);
    }
    if let Some(action) = check_existing_generation_replays(
        &epochs,
        &record,
        &command.command_id,
        command.generation,
        legacy_outbox,
    )? {
        return Ok(action);
    }
    let Some((last_epoch, last_records)) = epochs.last() else {
        return Err(refused("missing outbox epoch".to_owned()));
    };
    let meta_fence = RestoreFence::load(conn, &command.command_id)?;
    let mut ledger = replay_epoch_with_fence(last_records, meta_fence)?.ledger;
    if command.generation < ledger.active_generation() {
        return Ok(EnqueueAction::Noop);
    }
    // A newer generation after a cleanly consumed prior epoch opens a fresh
    // epoch while keeping the fixed queue on v11+ live writes (legacy pre-v11
    // retained journals classified each post-consumption row independently).
    if ledger.active().consumed_commit_seq.is_some() && ledger.compensation().is_none() {
        if queue != ledger.queue() && !legacy_outbox {
            return Err(refused(OutboxLedgerRefusal::QueueChanged.to_string()));
        }
        let next_epoch = last_epoch
            .checked_add(1)
            .ok_or(CellError::OutOfRange("outbox epoch"))?;
        let _started = OperationLedger::start(&record).map_err(|err| refused(err.to_string()))?;
        append_record(conn, &command.command_id, next_epoch, &record)?;
        return Ok(EnqueueAction::InsertNew);
    }
    match ledger.apply(&record) {
        Ok(ApplyStatus::Applied) => {
            append_record(conn, &command.command_id, *last_epoch, &record)?;
            Ok(EnqueueAction::ReplaceInPlace)
        }
        Ok(ApplyStatus::IdempotentDuplicate) => Ok(EnqueueAction::Noop),
        Err(
            OutboxLedgerRefusal::QueueChanged
            | OutboxLedgerRefusal::AmbiguousDispatchRequiresReconciliation { .. },
        ) if legacy_outbox => {
            let legacy_record = OutboxRecord {
                operation: op,
                generation: command.generation,
                kind: OutboxRecordKind::Requested {
                    queue: ledger.queue(),
                    payload_digest: digest,
                    dedup_contract: dedup,
                    commit_seq,
                },
            };
            crate::outbox_ledger_migrate::apply_legacy_replacement(
                conn,
                &mut ledger,
                *last_epoch,
                &command.command_id,
                &legacy_record,
                commit_seq,
            )
        }
        Err(error) => Err(refused(error.to_string())),
    }
}

fn check_existing_generation_replays(
    epochs: &[(u64, Vec<OutboxRecord>)],
    record: &OutboxRecord,
    command_id: &str,
    generation: u64,
    legacy_outbox: bool,
) -> Result<Option<EnqueueAction>, CellError> {
    let refused = |reason: String| CellError::OutboxReissueRefused {
        command_id: command_id.to_owned(),
        generation,
        reason,
    };
    // Check same-generation replays across every epoch (active, superseded, or
    // consumed) so payload divergence always fails closed on v11+ commits.
    for (_, records) in epochs {
        let mut ledger = replay_records(records)?;
        if ledger.generation(generation).is_some() {
            return match ledger.apply(record) {
                Ok(ApplyStatus::IdempotentDuplicate) => Ok(Some(EnqueueAction::Noop)),
                Err(OutboxLedgerRefusal::PayloadDiverged | OutboxLedgerRefusal::QueueChanged)
                    if legacy_outbox =>
                {
                    Ok(Some(EnqueueAction::Noop))
                }
                Ok(ApplyStatus::Applied) => Err(refused("unexpected duplicate state".to_owned())),
                Err(error) => Err(refused(error.to_string())),
            };
        }
    }
    Ok(None)
}

/// Validate and append an [`OutboxRecordKind::Dispatched`] claim record.
pub(crate) fn record_dispatched<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    claim_commit_seq: u64,
) -> Result<DispatchProof, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, last_records)) = epochs.last() else {
        return Err(CellError::PerformClaimNotPending {
            command_id: command_id.to_owned(),
            generation,
        });
    };
    let meta_fence = RestoreFence::load(conn, command_id)?;
    let mut ledger = replay_epoch_with_fence(last_records, meta_fence)?.ledger;
    let record = OutboxRecord {
        operation: ledger.operation(),
        generation,
        kind: OutboxRecordKind::Dispatched { claim_commit_seq },
    };
    match ledger.apply(&record) {
        Ok(ApplyStatus::Applied) => {
            append_record(conn, command_id, *last_epoch, &record)?;
            Ok(DispatchProof {
                operation: ledger.operation(),
                generation,
                claim_commit_seq,
            })
        }
        Ok(ApplyStatus::IdempotentDuplicate) | Err(OutboxLedgerRefusal::AlreadyClaimed) => {
            Err(CellError::PerformAlreadyClaimed {
                command_id: command_id.to_owned(),
                generation,
            })
        }
        Err(OutboxLedgerRefusal::ParkedCannotDispatch) => Err(CellError::ParkedClaimUnsupported {
            command_id: command_id.to_owned(),
            generation,
        }),
        Err(refusal) => Err(CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal,
        }),
    }
}

/// Validate and append an [`OutboxRecordKind::Outcome`] for the active head
/// (either via its [`DispatchProof`] if claimed, or [`OutcomeEvidence::Resolved`]
/// for parked/simulated/unclaimed outcomes).
pub(crate) fn record_active_outcome<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    outcome: &Value,
) -> Result<ApplyStatus, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, last_records)) = epochs.last() else {
        return Err(CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal: OutboxLedgerRefusal::NotInitialized,
        });
    };
    let meta_fence = RestoreFence::load(conn, command_id)?;
    let mut ledger = replay_epoch_with_fence(last_records, meta_fence)?.ledger;
    let (evidence, commit_seq) = if let Some(proof) = ledger.active().dispatch {
        (
            OutcomeEvidence::DispatchProof { proof },
            proof.claim_commit_seq,
        )
    } else {
        (
            OutcomeEvidence::Resolved,
            ledger.active().enqueue_commit_seq,
        )
    };
    let record = OutboxRecord {
        operation: ledger.operation(),
        generation,
        kind: OutboxRecordKind::Outcome {
            evidence,
            ok: outcome_is_ok(outcome),
            outcome_digest: outcome_digest(outcome)?,
            commit_seq,
        },
    };
    let status = ledger
        .apply(&record)
        .map_err(|refusal| CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal,
        })?;
    if status == ApplyStatus::Applied {
        append_record(conn, command_id, *last_epoch, &record)?;
    }
    Ok(status)
}

/// Best-effort immutable attachment of a late outcome to a previously
/// dispatched generation when `record_performed` receives a non-head generation.
///
/// Returns `Ok(Some(ApplyStatus::Applied))` when a newly attached late outcome
/// was persisted to its original generation (never modifying `_norn_outbox`).
pub(crate) fn try_attach_late_outcome<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    outcome: &Value,
) -> Result<Option<ApplyStatus>, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, _)) = epochs.last() else {
        return Ok(None);
    };
    for (epoch, records) in epochs.iter().rev() {
        let mut ledger = replay_records(records)?;
        let Some(gen_rec) = ledger.generation(generation) else {
            continue;
        };
        let is_prior = *epoch < *last_epoch || generation < ledger.active_generation();
        if !is_prior {
            return Ok(None);
        }
        let Some(proof) = gen_rec.dispatch else {
            return Ok(None);
        };
        let record = OutboxRecord {
            operation: ledger.operation(),
            generation,
            kind: OutboxRecordKind::Outcome {
                evidence: OutcomeEvidence::DispatchProof { proof },
                ok: outcome_is_ok(outcome),
                outcome_digest: outcome_digest(outcome)?,
                commit_seq: proof.claim_commit_seq,
            },
        };
        return match ledger.apply(&record) {
            Ok(status) => {
                if status == ApplyStatus::Applied {
                    append_record(conn, command_id, *epoch, &record)?;
                }
                Ok(Some(status))
            }
            Err(_) => Ok(None),
        };
    }
    Ok(None)
}

/// Explicitly attach an outcome carrying a caller-supplied [`DispatchProof`],
/// failing closed on any proof mismatch or conflicting outcome.
///
/// Returns [`ProofOutcomeTarget::SettledActiveHead`] iff it newly settled the
/// active unfenced head; late outcomes for prior generations return
/// [`ProofOutcomeTarget::NoHeadProjection`] and never alter `_norn_outbox`.
pub(crate) fn record_outcome_with_proof<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    proof: DispatchProof,
    ok: bool,
    outcome: &Value,
) -> Result<ProofOutcomeTarget, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, _)) = epochs.last() else {
        return Err(CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation: proof.generation,
            refusal: OutboxLedgerRefusal::UnknownGeneration,
        });
    };
    let meta_fence = RestoreFence::load(conn, command_id)?;
    let digest = outcome_digest(outcome)?;
    for (epoch, records) in epochs.iter().rev() {
        let mut ledger = if *epoch == *last_epoch {
            replay_epoch_with_fence(records, meta_fence)?.ledger
        } else {
            replay_records(records)?
        };
        if ledger.generation(proof.generation).is_none() {
            continue;
        }
        let record = OutboxRecord {
            operation: proof.operation,
            generation: proof.generation,
            kind: OutboxRecordKind::Outcome {
                evidence: OutcomeEvidence::DispatchProof { proof },
                ok,
                outcome_digest: digest,
                commit_seq: proof.claim_commit_seq,
            },
        };
        let status = ledger
            .apply(&record)
            .map_err(|refusal| CellError::OutboxLedgerRefused {
                command_id: command_id.to_owned(),
                generation: proof.generation,
                refusal,
            })?;
        if status == ApplyStatus::Applied {
            append_record(conn, command_id, *epoch, &record)?;
        }
        let settled_active = status == ApplyStatus::Applied
            && *epoch == *last_epoch
            && proof.generation == ledger.active_generation()
            && !ledger.is_terminal();
        return Ok(if settled_active {
            ProofOutcomeTarget::SettledActiveHead
        } else {
            ProofOutcomeTarget::NoHeadProjection
        });
    }
    Err(CellError::OutboxLedgerRefused {
        command_id: command_id.to_owned(),
        generation: proof.generation,
        refusal: OutboxLedgerRefusal::UnknownGeneration,
    })
}

/// Validate and append an [`OutboxRecordKind::Consumed`] record.
pub(crate) fn record_consumed<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    commit_seq: u64,
) -> Result<(), CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, last_records)) = epochs.last() else {
        return Err(CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal: OutboxLedgerRefusal::NotInitialized,
        });
    };
    let meta_fence = RestoreFence::load(conn, command_id)?;
    let mut ledger = replay_epoch_with_fence(last_records, meta_fence)?.ledger;
    let record = OutboxRecord {
        operation: ledger.operation(),
        generation,
        kind: OutboxRecordKind::Consumed { commit_seq },
    };
    let status = ledger
        .apply(&record)
        .map_err(|refusal| CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal,
        })?;
    if status == ApplyStatus::Applied {
        append_record(conn, command_id, *last_epoch, &record)?;
    }
    Ok(())
}

/// Persist an [`OutboxRecordKind::RestoreFenced`] record when restore fences a
/// pending command.
#[cfg(feature = "native")]
pub(crate) fn record_restore_fence<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    commit_seq: u64,
) -> Result<(), CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, last_records)) = epochs.last() else {
        return Ok(());
    };
    let mut ledger = replay_records(last_records)?;
    let record = OutboxRecord {
        operation: ledger.operation(),
        generation,
        kind: OutboxRecordKind::RestoreFenced { commit_seq },
    };
    let status = ledger
        .apply(&record)
        .map_err(|refusal| CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal,
        })?;
    if status == ApplyStatus::Applied {
        append_record(conn, command_id, *last_epoch, &record)?;
    }
    Ok(())
}

/// Validate and append a durable [`OutboxRecordKind::Reconciliation`] record.
///
/// Returns [`ReconciliationTarget`] so the commit writer can synchronize
/// `_norn_outbox` iff a confirmed outcome settled the active head.
pub(crate) fn record_reconciliation<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    decision: ReconciliationDecision,
    commit_seq: u64,
    recorded_fence: RestoreFence,
) -> Result<ReconciliationTarget, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, _)) = epochs.last() else {
        return Err(CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal: OutboxLedgerRefusal::UnknownGeneration,
        });
    };
    let effective_fence = RestoreFence::load(conn, command_id)?.or(recorded_fence);
    for (epoch, records) in epochs.iter().rev() {
        let is_latest = *epoch == *last_epoch;
        let mut replayed = if is_latest {
            replay_epoch_with_fence(records, effective_fence)?
        } else {
            ReplayedEpoch {
                ledger: replay_records(records)?,
                pending_fence: FenceProjection::None,
            }
        };
        if replayed.ledger.generation(generation).is_none() {
            continue;
        }
        let record = OutboxRecord {
            operation: replayed.ledger.operation(),
            generation,
            kind: OutboxRecordKind::Reconciliation {
                decision,
                commit_seq,
            },
        };
        let status =
            replayed
                .ledger
                .apply(&record)
                .map_err(|refusal| CellError::OutboxLedgerRefused {
                    command_id: command_id.to_owned(),
                    generation,
                    refusal,
                })?;
        if status == ApplyStatus::Applied {
            if let FenceProjection::Synthesized(fence_record) = replayed.pending_fence {
                append_record(conn, command_id, *epoch, &fence_record)?;
            }
            append_record(conn, command_id, *epoch, &record)?;
        }
        return Ok(
            if is_latest && generation == replayed.ledger.active_generation() {
                ReconciliationTarget::ActiveHead
            } else {
                ReconciliationTarget::PriorGeneration
            },
        );
    }
    Err(CellError::OutboxLedgerRefused {
        command_id: command_id.to_owned(),
        generation,
        refusal: OutboxLedgerRefusal::UnknownGeneration,
    })
}

/// Validate and append an [`OutboxRecordKind::Compensation`] record.
///
/// Returns [`CompensationProgress::Compensated`] when the writer must mark the
/// terminal compensated command row consumed in `_norn_outbox`.
pub(crate) fn record_compensation<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
    ok: bool,
    commit_seq: u64,
    recorded_fence: RestoreFence,
) -> Result<CompensationProgress, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, last_records)) = epochs.last() else {
        return Err(CellError::OutboxLedgerRefused {
            command_id: command_id.to_owned(),
            generation,
            refusal: OutboxLedgerRefusal::UnknownGeneration,
        });
    };
    let effective_fence = RestoreFence::load(conn, command_id)?.or(recorded_fence);
    let mut replayed = replay_epoch_with_fence(last_records, effective_fence)?;
    let record = OutboxRecord {
        operation: replayed.ledger.operation(),
        generation,
        kind: OutboxRecordKind::Compensation { ok, commit_seq },
    };
    let status =
        replayed
            .ledger
            .apply(&record)
            .map_err(|refusal| CellError::OutboxLedgerRefused {
                command_id: command_id.to_owned(),
                generation,
                refusal,
            })?;
    if status == ApplyStatus::Applied {
        if let FenceProjection::Synthesized(fence_record) = replayed.pending_fence {
            append_record(conn, command_id, *last_epoch, &fence_record)?;
        }
        append_record(conn, command_id, *last_epoch, &record)?;
    }
    Ok(
        if matches!(
            replayed.ledger.head_projection().state,
            norn_kernel::world::outbox::OutboxState::Compensated
        ) {
            CompensationProgress::Compensated
        } else {
            CompensationProgress::StillPending
        },
    )
}

/// Return the [`DispatchProof`] of a superseded prior generation that was
/// durably claimed and does not yet have an attached outcome (`D1`), so
/// `complete_perform` can journal late evidence durably via `RecordLateOutcome`
/// without creating redundant commits on duplicate deliveries.
pub(crate) fn pending_late_dispatch_proof<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    generation: u64,
) -> Result<Option<DispatchProof>, CellError> {
    let epochs = load_epochs(conn, command_id)?;
    let Some((last_epoch, _)) = epochs.last() else {
        return Ok(None);
    };
    for (epoch, records) in epochs.iter().rev() {
        let ledger = replay_records(records)?;
        let Some(gen_rec) = ledger.generation(generation) else {
            continue;
        };
        let is_prior = *epoch < *last_epoch || generation < ledger.active_generation();
        if !is_prior || gen_rec.outcome.is_some() {
            return Ok(None);
        }
        return Ok(gen_rec.dispatch);
    }
    Ok(None)
}

/// Replay consumed run history into `_norn_outbox_record` during export restore
/// (`history::restore_input`), preserving dispatch proofs for prior generations
/// and establishing the active generation's outcome before `Consumed`.
pub(crate) fn restore_consumed_runs<E: SqlEngine>(
    conn: &E,
    command_id: &str,
    active_generation: u64,
    runs: &[RunRecord],
    replay_mode: ActiveRunReplay,
) -> Result<(), CellError> {
    for run in runs {
        if run.attempt != 1 {
            continue;
        }
        if run.generation < active_generation {
            let _attached =
                try_attach_late_outcome(conn, command_id, run.generation, &run.outcome)?;
        } else if replay_mode == ActiveRunReplay::IncludeActive
            && run.generation == active_generation
        {
            let _status = record_active_outcome(conn, command_id, run.generation, &run.outcome)?;
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use norn_ir::RuntimeValue as Value;
    use norn_kernel::world::outbox::Queue;
    use norn_kernel::world::outbox_ledger::{
        OutboxRecord, OutboxRecordKind, ProviderDedupContract,
    };

    use super::{DDL, load_epochs, operation_key_for_cell, payload_digest};
    use crate::codec::canonical_json;
    use crate::engine::{NativeEngine, SqlEngine as _, SqlValue};
    use crate::error::CellError;

    fn requested_record(command_id: &str, generation: u64) -> Result<OutboxRecord, CellError> {
        let operation = operation_key_for_cell(None, command_id)?;
        let digest = payload_digest(&Value::String(format!("{command_id}:{generation}")))?;
        Ok(OutboxRecord {
            operation,
            generation,
            kind: OutboxRecordKind::Requested {
                queue: Queue::Fifo,
                payload_digest: digest,
                dedup_contract: ProviderDedupContract::None,
                commit_seq: generation.saturating_add(1),
            },
        })
    }

    fn insert_raw(
        engine: &NativeEngine,
        command_id: &str,
        epoch: i64,
        seq: i64,
        generation: i64,
        record: &OutboxRecord,
    ) -> Result<(), CellError> {
        engine.execute(
            "INSERT INTO \"_norn_outbox_record\" (\"command_id\", \"epoch\", \"seq\", \"generation\", \"record\") VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                SqlValue::Text(command_id.to_owned()),
                SqlValue::Integer(epoch),
                SqlValue::Integer(seq),
                SqlValue::Integer(generation),
                SqlValue::Text(canonical_json(record)?),
            ],
        )
    }

    #[test]
    fn load_epochs_rejects_corrupt_or_contradictory_rows() -> Result<(), CellError> {
        // 1. Valid contiguous epochs (0: seq 1..=2, 1: seq 1).
        let valid = NativeEngine::open_in_memory()?;
        valid.execute_batch(DDL)?;
        let req0 = requested_record("ok-cmd", 0)?;
        let disp0 = OutboxRecord {
            operation: req0.operation,
            generation: 0,
            kind: OutboxRecordKind::Dispatched {
                claim_commit_seq: 2,
            },
        };
        let req1 = requested_record("ok-cmd", 1)?;
        insert_raw(&valid, "ok-cmd", 0, 1, 0, &req0)?;
        insert_raw(&valid, "ok-cmd", 0, 2, 0, &disp0)?;
        insert_raw(&valid, "ok-cmd", 1, 1, 1, &req1)?;
        let epochs = load_epochs(&valid, "ok-cmd")?;
        assert_eq!(epochs.len(), 2);

        // 2. Column `generation` disagrees with JSON payload `record.generation`.
        let gen_mismatch = NativeEngine::open_in_memory()?;
        gen_mismatch.execute_batch(DDL)?;
        insert_raw(&gen_mismatch, "bad-gen", 0, 1, 9, &req0)?;
        assert!(matches!(
            load_epochs(&gen_mismatch, "bad-gen"),
            Err(CellError::LedgerCorrupt { seq: 0, .. })
        ));

        // 3. Non-zero starting epoch.
        let bad_start_epoch = NativeEngine::open_in_memory()?;
        bad_start_epoch.execute_batch(DDL)?;
        insert_raw(&bad_start_epoch, "bad-epoch", 1, 1, 0, &req0)?;
        assert!(matches!(
            load_epochs(&bad_start_epoch, "bad-epoch"),
            Err(CellError::LedgerCorrupt { seq: 0, .. })
        ));

        // 4. Non-1 starting seq (seq = 0).
        let bad_start_seq = NativeEngine::open_in_memory()?;
        bad_start_seq.execute_batch(DDL)?;
        insert_raw(&bad_start_seq, "bad-seq0", 0, 0, 0, &req0)?;
        assert!(matches!(
            load_epochs(&bad_start_seq, "bad-seq0"),
            Err(CellError::LedgerCorrupt { seq: 0, .. })
        ));

        // 5. Seq gap within epoch (1 -> 3).
        let seq_gap = NativeEngine::open_in_memory()?;
        seq_gap.execute_batch(DDL)?;
        insert_raw(&seq_gap, "seq-gap", 0, 1, 0, &req0)?;
        insert_raw(&seq_gap, "seq-gap", 0, 3, 0, &disp0)?;
        assert!(matches!(
            load_epochs(&seq_gap, "seq-gap"),
            Err(CellError::LedgerCorrupt { seq: 0, .. })
        ));

        // 6. Epoch gap (0 -> 2).
        let epoch_gap = NativeEngine::open_in_memory()?;
        epoch_gap.execute_batch(DDL)?;
        insert_raw(&epoch_gap, "epoch-gap", 0, 1, 0, &req0)?;
        insert_raw(&epoch_gap, "epoch-gap", 2, 1, 1, &req1)?;
        assert!(matches!(
            load_epochs(&epoch_gap, "epoch-gap"),
            Err(CellError::LedgerCorrupt { seq: 0, .. })
        ));
        Ok(())
    }
}
