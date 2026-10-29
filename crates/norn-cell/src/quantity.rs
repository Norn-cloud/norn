//! Normalized quantity persistence inside the cell commit transaction. This is
//! a trusted storage boundary, like `CellStore::commit`: ingress authorization
//! belongs to the caller; tenant scope must match the cell file binding.
use std::collections::{BTreeMap, BTreeSet};

use norn_ir::quantity_ledger::{QuantityOperation, QuantityRequest};
use norn_kernel::quantity::{
    QuantityBalance, QuantityError, QuantityKey, QuantityPosting, QuantityReservation,
    QuantityState, preflight_quantity, prepare_quantity, quantity_legs,
};
use serde::de::DeserializeOwned;

use crate::codec::{canonical_json, hash_json};
use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::read::{LedgerEntry, StoreReader, entry_hash, to_i64, to_u64};
use crate::{CellError, CommitBatch, Model, Receipt, ReceiptBody};

mod replay;
pub(crate) use replay::verify as verify_projections;

pub(crate) const EXPIRY_TIMER_PREFIX: &str = "norn.quantity.expiry/";

pub(crate) fn expiry_timer_id(
    world: &str,
    ledger: &str,
    reservation: &str,
) -> Result<String, CellError> {
    Ok(format!(
        "{EXPIRY_TIMER_PREFIX}{}",
        hash_json(&(world, ledger, reservation))?
    ))
}

pub(crate) fn expiry_timer(
    world: &str,
    ledger: &str,
    reservation: &str,
    due: u64,
) -> Result<norn_ir::RuntimeValue, CellError> {
    norn_ir::runtime::encode(&serde_json::json!({
        "world":world, "ledger":ledger, "reservation":reservation, "due":due,
    }))
    .map_err(CellError::Encoding)
}

/// Verify and read the cell-wide clock projection against the latest receipt.
///
/// # Errors
/// Projection loss, mismatch or storage errors.
pub(crate) fn clock_floor<S: SqlEngine>(engine: &S) -> Result<u64, CellError> {
    // One statement gives readers outside a commit transaction the same atomic
    // snapshot as writers. MAX on the receipt primary key is a bounded seek.
    let floor = engine.query_optional(
        r#"SELECT c.logical_time,c.commit_seq,c.receipt_hash,r.world,r.commit_seq,r.receipt_hash,r.receipt
        FROM "_norn_clock" AS c LEFT JOIN "_norn_receipt" AS r
          ON r.commit_seq=(SELECT MAX(commit_seq) FROM "_norn_receipt") WHERE c.id=1"#,
        &[], |row| {
            let projected = (to_u64(row.get(0)?, "cell clock")?, to_u64(row.get(1)?, "clock commit")?, row.get::<String>(2)?);
            let Some(record) = row.get::<Option<String>>(6)? else {
                if projected != (0,0,String::new()) { return Err(QuantityError::Corrupt.into()); }
                return Ok(0);
            };
            let body: ReceiptBody = serde_json::from_str(&record)?;
            let world: String = row.get(3)?;
            let seq = to_u64(row.get(4)?, "clock receipt")?;
            let hash: String = row.get(5)?;
            if body.world != world || body.commit_seq != seq || hash_json(&body)? != hash
                || projected != (body.logical_time, seq, hash) {
                return Err(QuantityError::Corrupt.into());
            }
            Ok(projected.0)
        },
    )?;
    floor.ok_or_else(|| QuantityError::Corrupt.into())
}

/// First reserved quantity event sequence and its establishing commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FormatBoundary {
    pub seq: u64,
    pub commit_seq: u64,
}

fn boundary_event(boundary: FormatBoundary) -> Result<norn_ir::RuntimeValue, CellError> {
    norn_ir::runtime::encode(&serde_json::json!({
        "type": crate::system::STORE_FORMAT, "quantityBoundary": boundary,
    }))
    .map_err(CellError::Encoding)
}

// The first typed receipt establishes the boundary. This indexed seek never
// walks ordinary events, even if a legacy cell has many marker-shaped events.
const BOUNDARY_SQL: &str = r#"SELECT json_extract("receipt", '$.firstEventSeq'), "commit_seq" FROM "_norn_receipt"
    WHERE json_extract("receipt", '$.quantity.version')='norn.quantity.commit/2'
    AND json_extract("receipt", '$.firstEventSeq')>0
    ORDER BY json_extract("receipt", '$.firstEventSeq') LIMIT 1"#;

// Called only after the event chain and receipt tiling have verified. Neither
// the metadata pointer nor surviving quantity projections choose this boundary.
pub(crate) fn boundary_marker(
    entry: &LedgerEntry,
    receipt: &ReceiptBody,
) -> Result<Option<FormatBoundary>, CellError> {
    if receipt.quantity.is_none() {
        return Ok(None);
    }
    if entry
        .event
        .get("type")
        .and_then(norn_ir::RuntimeValue::as_str)
        != Some(crate::system::STORE_FORMAT)
    {
        return Ok(None);
    }
    let boundary = FormatBoundary {
        seq: entry.seq,
        commit_seq: entry.commit_seq,
    };
    if entry.event != boundary_event(boundary)? || receipt.first_event_seq != entry.seq {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(Some(boundary))
}

// A legacy handle may observe another handle's first quantity commit. Discover
// that immutable marker with a bounded indexed seek, never a metadata fallback.
// The selected event and its receipt are verified by validate_boundary below.
pub(crate) fn admission_boundary<S: SqlEngine>(
    engine: &S,
    cached: Option<FormatBoundary>,
) -> Result<Option<FormatBoundary>, CellError> {
    let boundary = if cached.is_some() {
        cached
    } else {
        let rows = engine.query(BOUNDARY_SQL, &[])?;
        match rows.as_slice() {
            [] => None,
            [values] => {
                let row = SqlRow(values.clone());
                Some(FormatBoundary {
                    seq: to_u64(row.get(0)?, "quantity boundary")?,
                    commit_seq: to_u64(row.get(1)?, "quantity boundary commit")?,
                })
            }
            _ => return Err(QuantityError::Corrupt.into()),
        }
    };
    validate_boundary(engine, boundary)?;
    Ok(boundary)
}

pub(crate) fn format_boundary<S: SqlEngine>(
    engine: &S,
) -> Result<Option<FormatBoundary>, CellError> {
    admission_boundary(engine, None)
}

// Mutable metadata must agree with the chain-derived fact. It can only refuse,
// never select or move the quantity interpretation boundary.
pub(crate) fn validate_boundary<S: SqlEngine>(
    engine: &S,
    boundary: Option<FormatBoundary>,
) -> Result<(), CellError> {
    let format: String = engine.query_row(
        r#"SELECT "value" FROM "_norn_meta" WHERE "key"='format'"#,
        &[],
        |row| row.get(0),
    )?;
    let pointer: Option<String> = engine.query_optional(
        r#"SELECT "value" FROM "_norn_meta" WHERE "key"='quantity:boundary'"#,
        &[],
        |row| row.get(0),
    )?;
    let Some(boundary) = boundary else {
        if format != crate::system::STORE_FORMAT || pointer.is_some() {
            return Err(QuantityError::Corrupt.into());
        }
        if engine
            .query_optional(
                r#"SELECT 1 FROM "_norn_quantity_ledger" LIMIT 1"#,
                &[],
                |row| row.get::<i64>(0),
            )?
            .is_some()
        {
            return Err(QuantityError::Corrupt.into());
        }
        return Ok(());
    };
    if format != crate::system::STORE_FORMAT
        || decode::<FormatBoundary>(&pointer.ok_or(QuantityError::Corrupt)?)? != boundary
    {
        return Err(QuantityError::Corrupt.into());
    }
    let evidence = engine.query_optional(
        r#"SELECT "world", "event", "prev_hash", "hash", "commit_seq" FROM "_norn_ledger" WHERE "seq"=?1"#,
        params![to_i64(boundary.seq, "quantity boundary")?],
        |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?, row.get::<String>(2)?, row.get::<String>(3)?, row.get::<i64>(4)?)),
    )?.ok_or(QuantityError::Corrupt)?;
    let event = norn_ir::RuntimeValue::from_json_text(&evidence.1)?;
    if boundary.seq == 0
        || boundary.commit_seq == 0
        || evidence.4 != to_i64(boundary.commit_seq, "quantity boundary commit")?
        || event != boundary_event(boundary)?
        || entry_hash(
            boundary.seq,
            boundary.commit_seq,
            &evidence.0,
            &event,
            &evidence.2,
        )? != evidence.3
    {
        return Err(QuantityError::Corrupt.into());
    }
    let receipt = receipt_evidence(engine, &evidence.0, boundary.commit_seq)?;
    if receipt.first_event_seq != boundary.seq
        || receipt.event_count < 2
        || receipt.quantity.is_none()
    {
        return Err(QuantityError::Corrupt.into());
    }
    transition_request(engine, &evidence.0, evidence.4)?;
    Ok(())
}

pub(crate) fn establish_boundary<S: SqlEngine>(
    engine: &S,
    boundary: FormatBoundary,
) -> Result<(), CellError> {
    engine.execute(
        r#"INSERT INTO "_norn_meta" ("key", "value") VALUES ('quantity:boundary', ?1)"#,
        params![canonical_json(&boundary)?],
    )?;
    engine.execute(
        r#"UPDATE "_norn_meta" SET "value"=?1 WHERE "key"='format'"#,
        params![crate::system::STORE_FORMAT],
    )?;
    Ok(())
}

fn check_expiry_timer_effects(batch: &CommitBatch) -> Result<(), CellError> {
    if batch.effects.iter().any(|effect| match effect {
        crate::EffectRow::Schedule { timer } => timer.timer_id.starts_with(EXPIRY_TIMER_PREFIX),
        crate::EffectRow::CancelTimer { timer_id } => timer_id.starts_with(EXPIRY_TIMER_PREFIX),
        crate::EffectRow::Enqueue { .. } | crate::EffectRow::ClaimPerform { .. } => false,
    }) {
        return Err(QuantityError::InvalidContract.into());
    }
    if let Some(crate::Consumption::Timer { timer_id, .. }) = &batch.consumes
        && timer_id.starts_with(EXPIRY_TIMER_PREFIX)
        && batch
            .quantity
            .as_ref()
            .is_none_or(|request| match &request.operation {
                QuantityOperation::Expire { reservation_id } => {
                    expiry_timer_id(&batch.world, &request.ledger_id, reservation_id)
                        .ok()
                        .as_ref()
                        != Some(timer_id)
                }
                QuantityOperation::Define { .. }
                | QuantityOperation::Post { .. }
                | QuantityOperation::Reserve { .. }
                | QuantityOperation::PostReservation { .. }
                | QuantityOperation::Void { .. }
                | QuantityOperation::Reverse { .. } => true,
            })
    {
        return Err(QuantityError::InvalidContract.into());
    }
    Ok(())
}

// Host time and the store-generated intent are not caller request inputs.
pub(crate) fn retry_input(batch: &CommitBatch) -> Result<norn_ir::RuntimeValue, CellError> {
    let mut input = batch.clone();
    input.logical_time = 0;
    input.quantity_intent = None;
    norn_ir::runtime::encode(&input).map_err(CellError::Encoding)
}

pub(crate) fn retry_identity(
    input: &norn_ir::RuntimeValue,
    definitions_version: u64,
) -> Result<String, CellError> {
    crate::codec::hash_json(&("norn.quantity.retry/2", definitions_version, input))
}

// Ordinary commits cannot claim either internal discriminator; restore uses
// the same current typed commit path.
pub(crate) fn admit_events<S: SqlEngine>(
    engine: &S,
    model: &Model,
    batch: &CommitBatch,
    boundary: Option<FormatBoundary>,
    prior: Option<&Receipt>,
) -> Result<(CommitBatch, Option<FormatBoundary>), CellError> {
    let internal = |event: &norn_ir::RuntimeValue| {
        event.get("type").and_then(norn_ir::RuntimeValue::as_str) == Some("quantity.transition")
    };
    let marker = |event: &norn_ir::RuntimeValue| {
        event.get("type").and_then(norn_ir::RuntimeValue::as_str)
            == Some(crate::system::STORE_FORMAT)
    };
    let mut candidate = batch.clone();
    check_expiry_timer_effects(batch)?;
    let Some(request) = &batch.quantity else {
        if batch.quantity_intent.is_some() {
            return Err(QuantityError::InvalidContract.into());
        }
        if candidate
            .events
            .iter()
            .any(|event| internal(event) || marker(event))
        {
            return Err(QuantityError::InvalidContract.into());
        }
        return Ok((candidate, None));
    };
    check_request(engine, batch, request)?;
    let new_boundary = if boundary.is_none() {
        let reader = StoreReader::new(engine, model);
        Some(FormatBoundary {
            seq: reader
                .ledger_tail()?
                .0
                .checked_add(1)
                .ok_or(QuantityError::Corrupt)?,
            commit_seq: reader.next_commit_seq()?,
        })
    } else {
        None
    };
    let establishing = new_boundary.or_else(|| {
        boundary.filter(|boundary| {
            prior.is_some_and(|receipt| receipt.body.commit_seq == boundary.commit_seq)
        })
    });
    let event = transition_event(request)?;
    if candidate
        .events
        .iter()
        .enumerate()
        .any(|(index, supplied)| {
            internal(supplied)
                && (index.checked_add(1) != Some(candidate.events.len()) || supplied != &event)
        })
    {
        return Err(QuantityError::InvalidContract.into());
    }
    if let Some(boundary) = establishing {
        let expected = boundary_event(boundary)?;
        if candidate
            .events
            .iter()
            .enumerate()
            .any(|(index, supplied)| marker(supplied) && (index != 0 || supplied != &expected))
        {
            return Err(QuantityError::InvalidContract.into());
        }
        if candidate.events.first() != Some(&expected) {
            candidate.events.insert(0, expected);
        }
    } else if candidate.events.iter().any(marker) {
        return Err(QuantityError::InvalidContract.into());
    }
    if candidate.events.last() != Some(&event) {
        candidate.events.push(event);
    }
    Ok((candidate, new_boundary))
}

pub(crate) fn transition_event(
    request: &QuantityRequest,
) -> Result<norn_ir::RuntimeValue, CellError> {
    norn_ir::runtime::encode(&serde_json::json!({"type":"quantity.transition", "request":request}))
        .map_err(CellError::Encoding)
}

fn receipt_evidence<S: SqlEngine>(
    engine: &S,
    world: &str,
    commit_seq: u64,
) -> Result<ReceiptBody, CellError> {
    let (text, hash): (String, String) = engine.query_optional(
        r#"SELECT "receipt", "receipt_hash" FROM "_norn_receipt" WHERE "commit_seq"=?1 AND "world"=?2"#,
        params![to_i64(commit_seq, "quantity receipt")?, world], |row| Ok((row.get(0)?, row.get(1)?)),
    )?.ok_or(QuantityError::Corrupt)?;
    let body: ReceiptBody = serde_json::from_str(&text)?;
    if body.commit_seq != commit_seq || body.world != world || hash_json(&body)? != hash {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(body)
}

/// Maximum serialized request or persisted pack/vector bytes for one turn.
const MAX_RECORD_BYTES: usize = 256 * 1024;

const REVERSAL_PROJECTION_SQL: &str = r#"SELECT "id" FROM "_norn_quantity_posting" WHERE "world"=?1 AND "ledger"=?2 AND "reverses"=?3"#;
const LATEST_TRANSITION_SQL: &str = r#"SELECT "commit_seq" FROM "_norn_ledger" WHERE "world"=?1
    AND json_extract("event", '$.request.ledgerId')=?2
    AND json_extract("event", '$.type')='quantity.transition'
    AND EXISTS (SELECT 1 FROM "_norn_receipt" AS r WHERE r."commit_seq"="_norn_ledger"."commit_seq"
      AND json_extract(r."receipt", '$.quantity.version')='norn.quantity.commit/2')
    AND "seq">?3
    ORDER BY "seq" DESC LIMIT 1"#;
const REVERSAL_EVIDENCE_SQL: &str = r#"SELECT "commit_seq" FROM "_norn_ledger" WHERE "world"=?1
    AND json_extract("event", '$.request.ledgerId')=?2
    AND json_extract("event", '$.request.operation.original_id')=?3
    AND json_extract("event", '$.type')='quantity.transition'
    AND json_extract("event", '$.request.operation.kind')='reverse' AND "seq">?4
    AND EXISTS (SELECT 1 FROM "_norn_receipt" AS r WHERE r."commit_seq"="_norn_ledger"."commit_seq"
      AND json_extract(r."receipt", '$.quantity.version')='norn.quantity.commit/2') LIMIT 2"#;

// SQL identities and immutable Norn1 events must retain the same exact strings.
// Refuse a request that canonical serialization would reinterpret.
pub(crate) fn check_request<S: SqlEngine>(
    engine: &S,
    batch: &CommitBatch,
    request: &QuantityRequest,
) -> Result<(), CellError> {
    for identity in [
        batch.world.as_str(),
        batch.scope.as_str(),
        batch.idempotency_key.as_deref().unwrap_or(""),
    ] {
        if serde_json::from_str::<String>(&canonical_json(identity)?)? != identity {
            return Err(QuantityError::InvalidContract.into());
        }
    }
    if batch.world.is_empty()
        || batch.scope.is_empty()
        || batch
            .idempotency_key
            .as_ref()
            .is_none_or(|id| id.is_empty() || id.len() > 128)
    {
        return Err(QuantityError::InvalidContract.into());
    }
    let scope = engine.query_optional(
        r#"SELECT "value" FROM "_norn_meta" WHERE "key"='binding:scope'"#,
        &[],
        |row| row.get::<String>(0),
    )?;
    if scope.as_ref() != Some(&batch.scope) {
        return Err(QuantityError::InvalidContract.into());
    }
    if let QuantityOperation::Reserve { expires_at, .. } = &request.operation {
        to_i64(*expires_at, "quantity expiry").map_err(|_range| QuantityError::InvalidContract)?;
    }
    let canonical = canonical_json(request)?;
    if canonical.len() > MAX_RECORD_BYTES
        || serde_json::from_str::<QuantityRequest>(&canonical)? != *request
    {
        return Err(QuantityError::InvalidContract.into());
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(text: &str) -> Result<T, CellError> {
    if text.len() > MAX_RECORD_BYTES.saturating_add(128) {
        return Err(QuantityError::InvalidContract.into());
    }
    Ok(serde_json::from_str(text)?)
}

/// Read one exact quantity balance. Missing heads mean zero posted and held.
///
/// # Errors
/// SQL errors or corrupt exact text.
pub fn balance<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    key: &QuantityKey,
) -> Result<QuantityBalance, CellError> {
    Ok(engine.query_optional(
        r#"SELECT "posted", "held", "version" FROM "_norn_quantity_balance" WHERE "world"=?1 AND "ledger"=?2 AND "account"=?3 AND "unit"=?4 AND "layer"=?5"#,
        params![world, ledger, key.account, key.unit, key.layer],
        |row| Ok(QuantityBalance { posted: row.get(0)?, held: row.get(1)?, version: to_u64(row.get(2)?, "quantity balance version")? }),
    )?.unwrap_or_default())
}

/// Read one complete reservation including its terminal status.
///
/// # Errors
/// SQL, decoding or record budget errors.
pub fn reservation<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    id: &str,
) -> Result<Option<QuantityReservation>, CellError> {
    engine.query_optional(
        r#"SELECT "record" FROM "_norn_quantity_reservation" WHERE "world"=?1 AND "ledger"=?2 AND "id"=?3"#,
        params![world, ledger, id], |row| decode(&row.get::<String>(0)?),
    )
}

/// Read one immutable posting and its reversal link.
///
/// # Errors
/// SQL, decoding or record budget errors.
pub fn posting<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    id: &str,
) -> Result<Option<QuantityPosting>, CellError> {
    engine.query_optional(
        r#"SELECT "record", "reverses", "commit_seq", "version" FROM "_norn_quantity_posting" WHERE "world"=?1 AND "ledger"=?2 AND "id"=?3"#,
        params![world, ledger, id], |row| {
            let stored: QuantityPosting = decode(&row.get::<String>(0)?)?;
            let link: Option<String> = row.get(1)?;
            let request = transition_request(engine, world, row.get(2)?)?;
            let (posting_id, expected_link) = posting_identity(&request).ok_or(QuantityError::Corrupt)?;
            if request.ledger_id != ledger
                || posting_id != id
                || receipt_evidence(engine, world, to_u64(row.get(2)?, "quantity posting commit")?)?.quantity_result.as_ref().map(|result| result.version) != Some(to_u64(row.get(3)?, "quantity posting version")?)
                || stored.reverses.as_deref() != expected_link
                || stored.reverses != link
            {
                return Err(QuantityError::Corrupt.into());
            }
            Ok(stored)
        },
    )
}

fn posting_identity(request: &QuantityRequest) -> Option<(&str, Option<&str>)> {
    match &request.operation {
        QuantityOperation::Post { posting_id, .. }
        | QuantityOperation::PostReservation { posting_id, .. } => Some((posting_id, None)),
        QuantityOperation::Reverse {
            original_id,
            posting_id,
        } => Some((posting_id, Some(original_id))),
        QuantityOperation::Define { .. }
        | QuantityOperation::Reserve { .. }
        | QuantityOperation::Void { .. }
        | QuantityOperation::Expire { .. } => None,
    }
}

// Called after hash/receipt verification, including on reopen. Walking the
// immutable events also finds deleted projections; scanning surviving rows
// alone cannot establish correspondence.
pub(crate) fn verify_transition_projection<S: SqlEngine>(
    engine: &S,
    world: &str,
    commit_seq: u64,
    request: &QuantityRequest,
) -> Result<(), CellError> {
    let projected =
        ledger_projection(engine, world, &request.ledger_id)?.ok_or(QuantityError::Corrupt)?;
    let latest =
        latest_transition(engine, world, &request.ledger_id)?.ok_or(QuantityError::Corrupt)?;
    if projected.0 != latest.2 {
        return Err(QuantityError::Corrupt.into());
    }

    if matches!(request.operation, QuantityOperation::Define { .. })
        && projected.3 != to_i64(commit_seq, "quantity commit seq")?
    {
        return Err(QuantityError::Corrupt.into());
    }
    verify_pack(
        engine,
        world,
        &request.ledger_id,
        projected.3,
        &projected.1,
        &projected.2,
    )?;
    let reservation_id = match &request.operation {
        QuantityOperation::Reserve { reservation_id, .. }
        | QuantityOperation::PostReservation { reservation_id, .. }
        | QuantityOperation::Void { reservation_id, .. }
        | QuantityOperation::Expire { reservation_id } => Some(reservation_id),
        QuantityOperation::Define { .. }
        | QuantityOperation::Post { .. }
        | QuantityOperation::Reverse { .. } => None,
    };
    if let Some(id) = reservation_id {
        let state = QuantityState {
            version: projected.0,
            pack: Some(projected.1),
            reservation: reservation(engine, world, &request.ledger_id, id)?,
            ..QuantityState::default()
        };
        if state.reservation.is_none() {
            return Err(QuantityError::Corrupt.into());
        }
        verify_reservation(engine, world, &request.ledger_id, id, &state)?;
    }
    let Some((id, expected_link)) = posting_identity(request) else {
        return Ok(());
    };
    let stored = posting(engine, world, &request.ledger_id, id)?.ok_or(QuantityError::Corrupt)?;
    let recorded_at: i64 = engine.query_row(
        r#"SELECT "commit_seq" FROM "_norn_quantity_posting" WHERE "world"=?1 AND "ledger"=?2 AND "id"=?3"#,
        params![world, request.ledger_id, id], |row| row.get(0),
    )?;
    if recorded_at != to_i64(commit_seq, "quantity commit seq")?
        || stored.reverses.as_deref() != expected_link
    {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(())
}

fn latest_transition<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
) -> Result<Option<(QuantityRequest, u64, u64)>, CellError> {
    let Some(boundary) = format_boundary(engine)? else {
        return Ok(None);
    };
    let seq = engine.query_optional(
        LATEST_TRANSITION_SQL,
        params![world, ledger, to_i64(boundary.seq, "quantity boundary")?],
        |row| row.get::<i64>(0),
    )?;
    seq.map(|seq| {
        Ok((
            transition_request(engine, world, seq)?,
            receipt_evidence(engine, world, to_u64(seq, "quantity receipt")?)?.logical_time,
            receipt_evidence(engine, world, to_u64(seq, "quantity receipt")?)?
                .quantity_result
                .ok_or(QuantityError::Corrupt)?
                .version,
        ))
    })
    .transpose()
}

type LedgerProjection = (
    u64,
    norn_ir::quantity_ledger::LedgerPackIr,
    Vec<String>,
    i64,
);

fn ledger_projection<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
) -> Result<Option<LedgerProjection>, CellError> {
    engine.query_optional(
        r#"SELECT "version", "pack", "unrestricted", "defined_at" FROM "_norn_quantity_ledger" WHERE "world"=?1 AND "ledger"=?2"#,
        params![world, ledger],
        |row| Ok((to_u64(row.get(0)?, "quantity version")?, decode(&row.get::<String>(1)?)?, decode(&row.get::<String>(2)?)?, row.get(3)?)),
    )
}

// Keep the normalized reversal lookup indexed, but require it to agree with
// independently indexed immutable evidence. A NULL link or a deleted reversal
// row must never make an already reversed original eligible again.
fn verified_reversed<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    original_id: &str,
) -> Result<bool, CellError> {
    let projected: Option<String> = engine.query_optional(
        REVERSAL_PROJECTION_SQL,
        params![world, ledger, original_id],
        |row| row.get(0),
    )?;
    let boundary = format_boundary(engine)?.ok_or(QuantityError::Corrupt)?;
    let evidence = engine.query(
        REVERSAL_EVIDENCE_SQL,
        params![
            world,
            ledger,
            original_id,
            to_i64(boundary.seq, "quantity boundary")?
        ],
    )?;
    match evidence.as_slice() {
        [] if projected.is_none() => Ok(false),
        [values] => {
            let commit_seq: i64 = SqlRow(values.clone()).get(0)?;
            let request = transition_request(engine, world, commit_seq)?;
            let Some((id, Some(link))) = posting_identity(&request) else {
                return Err(QuantityError::Corrupt.into());
            };
            if request.ledger_id != ledger
                || link != original_id
                || projected.as_deref() != Some(id)
            {
                return Err(QuantityError::Corrupt.into());
            }
            verify_transition_projection(
                engine,
                world,
                to_u64(commit_seq, "quantity commit seq")?,
                &request,
            )?;
            Ok(true)
        }
        _ => Err(QuantityError::Corrupt.into()),
    }
}

/// Read the ledger ordering revision; balance partitions own their CAS fences.
///
/// # Errors
/// SQL or integer range errors.
pub fn version<S: SqlEngine>(engine: &S, world: &str, ledger: &str) -> Result<u64, CellError> {
    let found = engine.query_optional(
        r#"SELECT "version" FROM "_norn_quantity_ledger" WHERE "world"=?1 AND "ledger"=?2"#,
        params![world, ledger],
        |row| row.get::<i64>(0),
    )?;
    found.map_or(Ok(0), |v| to_u64(v, "quantity version"))
}

/// Read the exact optimistic fences for an operation. Callers retain these
/// inputs with their retry identity; admission checks them transactionally.
///
/// # Errors
/// Missing definitions or records, invalid vectors, or storage errors.
pub fn balance_versions<S: SqlEngine>(
    engine: &S,
    world: &str,
    request: &QuantityRequest,
) -> Result<Vec<norn_ir::quantity_ledger::QuantityBalanceVersion>, CellError> {
    if matches!(request.operation, QuantityOperation::Define { .. }) {
        return Ok(Vec::new());
    }
    let state = state(engine, world, request)?;
    let keys = norn_kernel::quantity::quantity_balance_keys(request, &state)?;
    keys.into_iter()
        .map(|key| {
            let version = state
                .balances
                .get(&key)
                .ok_or(QuantityError::Corrupt)?
                .version;
            Ok(norn_ir::quantity_ledger::QuantityBalanceVersion {
                account: key.account,
                unit: key.unit,
                layer: key.layer,
                version,
            })
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded closed-operation read set is reconstructed under one transaction snapshot"
)]
fn state<S: SqlEngine>(
    engine: &S,
    world: &str,
    request: &QuantityRequest,
) -> Result<QuantityState, CellError> {
    let latest = latest_transition(engine, world, &request.ledger_id)?;
    let mut state = engine.query_optional(
        r#"SELECT "version", "pack", "unrestricted", "defined_at" FROM "_norn_quantity_ledger" WHERE "world"=?1 AND "ledger"=?2"#,
        params![world, request.ledger_id], |row| {
            let pack = decode(&row.get::<String>(1)?)?;
            let unrestricted_accounts: Vec<String> = decode(&row.get::<String>(2)?)?;
            verify_pack(engine, world, &request.ledger_id, row.get(3)?, &pack, &unrestricted_accounts)?;
            Ok(QuantityState {
                version: to_u64(row.get(0)?, "quantity version")?,
                pack: Some(pack),
                unrestricted_accounts,
                ..QuantityState::default()
            })
        },
    )?.unwrap_or_default();
    if latest.as_ref().map(|(_, _, version)| *version) != state.pack.as_ref().map(|_| state.version)
    {
        return Err(QuantityError::Corrupt.into());
    }
    match &request.operation {
        QuantityOperation::Define { .. } => {}
        QuantityOperation::Post { posting_id, .. } => {
            state.identity_exists =
                posting(engine, world, &request.ledger_id, posting_id)?.is_some();
        }
        QuantityOperation::Reserve { reservation_id, .. } => {
            state.identity_exists =
                reservation(engine, world, &request.ledger_id, reservation_id)?.is_some();
        }
        QuantityOperation::PostReservation {
            reservation_id,
            posting_id,
            ..
        } => {
            state.reservation = reservation(engine, world, &request.ledger_id, reservation_id)?;
            state.identity_exists =
                posting(engine, world, &request.ledger_id, posting_id)?.is_some();
        }
        QuantityOperation::Void { reservation_id, .. }
        | QuantityOperation::Expire { reservation_id } => {
            state.reservation = reservation(engine, world, &request.ledger_id, reservation_id)?;
        }
        QuantityOperation::Reverse {
            original_id,
            posting_id,
        } => {
            state.original = posting(engine, world, &request.ledger_id, original_id)?;
            if state
                .original
                .as_ref()
                .is_some_and(|original| original.reverses.is_none())
            {
                verify_original(engine, world, &request.ledger_id, original_id, &state)?;
            }
            state.identity_exists =
                posting(engine, world, &request.ledger_id, posting_id)?.is_some();
            state.original_reversed =
                verified_reversed(engine, world, &request.ledger_id, original_id)?;
        }
    }
    if let QuantityOperation::PostReservation { reservation_id, .. }
    | QuantityOperation::Void { reservation_id, .. }
    | QuantityOperation::Expire { reservation_id } = &request.operation
    {
        verify_reservation(engine, world, &request.ledger_id, reservation_id, &state)?;
    }
    preflight_quantity(request, &state)?;
    let mut touched = quantity_legs(request, &state)?;
    if let Some(reservation) = &state.reservation {
        touched.extend(reservation.remaining.clone());
    }
    for leg in touched {
        let key = QuantityKey {
            account: leg.account_id,
            unit: leg.unit_id,
            layer: leg.layer,
        };
        if let std::collections::btree_map::Entry::Vacant(entry) = state.balances.entry(key) {
            let current = verified_balance(
                engine,
                world,
                &request.ledger_id,
                entry.key(),
                state.version,
                false,
            )?;
            entry.insert(current);
        }
    }
    Ok(state)
}

pub(crate) fn apply<S: SqlEngine>(
    engine: &S,
    batch: &CommitBatch,
    request: &QuantityRequest,
    commit_seq: u64,
) -> Result<norn_kernel::quantity::QuantityResult, CellError> {
    let state = state(engine, &batch.world, request)?;
    let plan = prepare_quantity(request, &state, batch.logical_time)?;
    let version = to_i64(plan.version, "quantity version")?;
    let seq = to_i64(commit_seq, "quantity commit seq")?;
    match &request.operation {
        QuantityOperation::Define {
            pack,
            unrestricted_accounts,
            ..
        } => {
            engine.execute(
                r#"INSERT INTO "_norn_quantity_ledger" ("world", "ledger", "version", "pack", "unrestricted", "defined_at") VALUES (?1,?2,?3,?4,?5,?6)"#,
                params![batch.world, request.ledger_id, version, canonical_json(pack)?, canonical_json(unrestricted_accounts)?, seq],
            )?;
        }
        QuantityOperation::Post { .. }
        | QuantityOperation::Reserve { .. }
        | QuantityOperation::PostReservation { .. }
        | QuantityOperation::Void { .. }
        | QuantityOperation::Expire { .. }
        | QuantityOperation::Reverse { .. } => {
            engine.execute(
                r#"UPDATE "_norn_quantity_ledger" SET "version"=?3 WHERE "world"=?1 AND "ledger"=?2"#,
                params![batch.world, request.ledger_id, version],
            )?;
        }
    }
    for (key, value) in &plan.balances {
        engine.execute(
            r#"INSERT INTO "_norn_quantity_balance" ("world","ledger","account","unit","layer","posted","held","version") VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
            ON CONFLICT ("world","ledger","account","unit","layer") DO UPDATE SET "posted"=excluded."posted", "held"=excluded."held", "version"=excluded."version""#,
            params![batch.world, request.ledger_id, key.account, key.unit, key.layer, value.posted, value.held, to_i64(value.version, "quantity balance version")?],
        )?;
        engine.execute(
            r#"INSERT INTO "_norn_quantity_history" ("world","ledger","account","unit","layer","posted","held","version","commit_seq") VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"#,
            params![batch.world, request.ledger_id, key.account, key.unit, key.layer, value.posted, value.held, to_i64(value.version, "quantity balance version")?, seq],
        )?;
    }
    if let Some((id, reservation)) = &plan.reservation {
        if matches!(request.operation, QuantityOperation::Reserve { .. }) {
            engine.execute(
                r#"INSERT INTO "_norn_quantity_reservation" ("world","ledger","id","record","version","commit_seq","created_at") VALUES (?1,?2,?3,?4,?5,?6,?7)"#,
                params![batch.world, request.ledger_id, id, canonical_json(reservation)?, version, seq, seq],
            )?;
        } else {
            engine.execute(
                r#"UPDATE "_norn_quantity_reservation" SET "record"=?4, "version"=?5, "commit_seq"=?6 WHERE "world"=?1 AND "ledger"=?2 AND "id"=?3"#,
                params![batch.world, request.ledger_id, id, canonical_json(reservation)?, version, seq],
            )?;
        }
        let timer_id = expiry_timer_id(&batch.world, &request.ledger_id, id)?;
        if reservation.status == "open" {
            engine.execute(
                r#"INSERT INTO "_norn_timer" ("timer_id","due","timer","commit_seq") VALUES (?1,?2,?3,?4)
                ON CONFLICT ("timer_id") DO UPDATE SET "due"=excluded."due", "timer"=excluded."timer", "commit_seq"=excluded."commit_seq""#,
                params![timer_id, to_i64(reservation.expires_at, "quantity expiry")?, canonical_json(&expiry_timer(&batch.world, &request.ledger_id, id, reservation.expires_at)?)?, seq],
            )?;
        } else {
            engine.execute(
                r#"DELETE FROM "_norn_timer" WHERE "timer_id"=?1"#,
                params![timer_id],
            )?;
        }
    }
    if let Some((id, posting)) = &plan.posting {
        engine.execute(
            r#"INSERT INTO "_norn_quantity_posting" ("world","ledger","id","record","reverses","version","commit_seq") VALUES (?1,?2,?3,?4,?5,?6,?7)"#,
            params![batch.world, request.ledger_id, id, canonical_json(posting)?, posting.reverses.as_deref(), version, seq],
        )?;
    }
    Ok(norn_kernel::quantity::QuantityResult {
        version: plan.version,
        balance_keys: plan.balances.keys().cloned().collect(),
        reservation: plan.reservation,
        posting: plan.posting,
    })
}

/// Exact receipt replay verifies the immutable named records and touched heads
/// against append-only history; it never re-evaluates availability or expiry.
#[allow(
    clippy::too_many_lines,
    reason = "receipt replay checks each closed operation and its bounded head history together"
)]
pub(crate) fn verify_replay<S: SqlEngine>(
    engine: &S,
    batch: &CommitBatch,
    request: &QuantityRequest,
) -> Result<(), CellError> {
    clock_floor(engine)?;
    let current = ledger_projection(engine, &batch.world, &request.ledger_id)?
        .ok_or(QuantityError::Corrupt)?;
    let latest = latest_transition(engine, &batch.world, &request.ledger_id)?
        .ok_or(QuantityError::Corrupt)?;
    if latest.2 != current.0 {
        return Err(QuantityError::Corrupt.into());
    }
    let mut state = engine.query_optional(
        r#"SELECT "version", "pack", "unrestricted", "defined_at" FROM "_norn_quantity_ledger" WHERE "world"=?1 AND "ledger"=?2"#,
        params![batch.world, request.ledger_id], |row| {
            let pack = decode(&row.get::<String>(1)?)?;
            let unrestricted_accounts: Vec<String> = decode(&row.get::<String>(2)?)?;
            verify_pack(engine, &batch.world, &request.ledger_id, row.get(3)?, &pack, &unrestricted_accounts)?;
            Ok(QuantityState {
                version: to_u64(row.get(0)?, "quantity version")?,
                pack: Some(pack), unrestricted_accounts,
                ..QuantityState::default()
            })
        },
    )?.ok_or(QuantityError::Corrupt)?;
    let prior = crate::read::receipt_by_key(
        engine,
        &batch.world,
        batch
            .idempotency_key
            .as_deref()
            .ok_or(QuantityError::Corrupt)?,
    )?
    .ok_or(QuantityError::Corrupt)?;
    if receipt_evidence(engine, &batch.world, prior.body.commit_seq)? != prior.body
        || prior.body.quantity.as_ref() != Some(request)
    {
        return Err(QuantityError::Corrupt.into());
    }
    let result = prior
        .body
        .quantity_result
        .as_ref()
        .ok_or(QuantityError::Corrupt)?;
    if let Some((id, expected)) = &result.posting
        && posting(engine, &batch.world, &request.ledger_id, id)?.as_ref() != Some(expected)
    {
        return Err(QuantityError::Corrupt.into());
    }
    if let Some((id, _)) = &result.reservation {
        state.reservation = reservation(engine, &batch.world, &request.ledger_id, id)?;
        if state.reservation.is_none() {
            return Err(QuantityError::Corrupt.into());
        }
        verify_reservation(engine, &batch.world, &request.ledger_id, id, &state)?;
    }
    for key in &result.balance_keys {
        verified_balance(
            engine,
            &batch.world,
            &request.ledger_id,
            key,
            state.version,
            true,
        )?;
    }

    Ok(())
}

// The current pack is bound to its hash-chained immutable definition event;
// changing ontology/unit identity must not reinterpret existing exact heads.
fn verify_pack<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    defined_at: i64,
    pack: &norn_ir::quantity_ledger::LedgerPackIr,
    unrestricted: &[String],
) -> Result<(), CellError> {
    let event = transition_request(engine, world, defined_at)?;
    if event.ledger_id != ledger {
        return Err(QuantityError::Corrupt.into());
    }
    match event.operation {
        QuantityOperation::Define {
            pack: original,
            unrestricted_accounts,
            ..
        } if &original == pack && unrestricted_accounts == unrestricted => Ok(()),
        QuantityOperation::Define { .. }
        | QuantityOperation::Post { .. }
        | QuantityOperation::Reserve { .. }
        | QuantityOperation::PostReservation { .. }
        | QuantityOperation::Void { .. }
        | QuantityOperation::Expire { .. }
        | QuantityOperation::Reverse { .. } => Err(QuantityError::Corrupt.into()),
    }
}

fn verified_balance<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    key: &QuantityKey,
    ledger_version: u64,
    require_head: bool,
) -> Result<QuantityBalance, CellError> {
    let head = engine.query_optional(
        r#"SELECT "posted", "held", "version" FROM "_norn_quantity_balance" WHERE "world"=?1 AND "ledger"=?2 AND "account"=?3 AND "unit"=?4 AND "layer"=?5"#,
        params![world, ledger, key.account, key.unit, key.layer],
        |row| Ok((QuantityBalance { posted: row.get(0)?, held: row.get(1)?, version: to_u64(row.get(2)?, "quantity balance version")? }, row.get::<i64>(2)?)),
    )?;
    let history = engine.query_optional(
        r#"SELECT "posted", "held", "version" FROM "_norn_quantity_history" WHERE "world"=?1 AND "ledger"=?2 AND "account"=?3 AND "unit"=?4 AND "layer"=?5 ORDER BY "version" DESC LIMIT 1"#,
        params![world, ledger, key.account, key.unit, key.layer],
        |row| Ok((QuantityBalance { posted: row.get(0)?, held: row.get(1)?, version: to_u64(row.get(2)?, "quantity balance version")? }, row.get::<i64>(2)?)),
    )?;
    match (head, history) {
        (Some(head), Some(history))
            if head == history && to_u64(head.1, "quantity head version")? <= ledger_version =>
        {
            Ok(head.0)
        }
        (None, None) if !require_head => Ok(QuantityBalance::default()),
        (Some(_) | None, Some(_) | None) => Err(QuantityError::Corrupt.into()),
    }
}

fn transition_request<S: SqlEngine>(
    engine: &S,
    world: &str,
    commit_seq: i64,
) -> Result<QuantityRequest, CellError> {
    let receipt = receipt_evidence(engine, world, to_u64(commit_seq, "quantity commit seq")?)?;
    let request = receipt.quantity.as_ref().ok_or(QuantityError::Corrupt)?;
    let seq = receipt
        .first_event_seq
        .checked_add(receipt.event_count)
        .and_then(|end| end.checked_sub(1))
        .ok_or(QuantityError::Corrupt)?;
    let (event, prev, hash): (String, String, String) = engine.query_optional(
        r#"SELECT "event", "prev_hash", "hash" FROM "_norn_ledger" WHERE "seq"=?1 AND "commit_seq"=?2 AND "world"=?3"#,
        params![to_i64(seq, "quantity transition seq")?, commit_seq, world],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?.ok_or(QuantityError::Corrupt)?;
    let event = norn_ir::RuntimeValue::from_json_text(&event)?;
    if receipt.event_count == 0
        || event != transition_event(request)?
        || entry_hash(seq, receipt.commit_seq, world, &event, &prev)? != hash
        || hash != receipt.ledger_head
    {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(request.clone())
}

fn verify_reservation<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    id: &str,
    state: &QuantityState,
) -> Result<(), CellError> {
    let Some(reservation) = &state.reservation else {
        return Ok(());
    };
    let (created_at, updated_at, recorded_version) = engine.query_row(
        r#"SELECT "created_at", "commit_seq", "version" FROM "_norn_quantity_reservation" WHERE "world"=?1 AND "ledger"=?2 AND "id"=?3"#,
        params![world, ledger, id], |row| Ok((row.get::<i64>(0)?, row.get::<i64>(1)?, to_u64(row.get(2)?, "quantity reservation version")?)),
    )?;
    let original = transition_request(engine, world, created_at)?;
    if original.ledger_id != ledger {
        return Err(QuantityError::Corrupt.into());
    }
    if let QuantityOperation::Reserve {
        reservation_id,
        expires_at,
        ..
    } = &original.operation
    {
        if reservation_id != id
            || *expires_at != reservation.expires_at
            || quantity_legs(&original, state)? != reservation.legs
        {
            return Err(QuantityError::Corrupt.into());
        }
    } else {
        return Err(QuantityError::Corrupt.into());
    }
    let latest = receipt_evidence(
        engine,
        world,
        to_u64(updated_at, "quantity reservation commit")?,
    )?;
    if latest
        .quantity
        .as_ref()
        .is_none_or(|request| request.ledger_id != ledger)
        || latest.quantity_result.as_ref().is_none_or(|result| {
            result.version != recorded_version
                || result.reservation.as_ref() != Some(&(id.to_owned(), reservation.clone()))
        })
        || recorded_version > state.version
    {
        return Err(QuantityError::Corrupt.into());
    }

    Ok(())
}

// A correction is linked to immutable posting evidence, never just a mutable
// copy of a balanced vector read from an externally damaged file.
fn verify_original<S: SqlEngine>(
    engine: &S,
    world: &str,
    ledger: &str,
    id: &str,
    state: &QuantityState,
) -> Result<(), CellError> {
    let original = state.original.as_ref().ok_or(QuantityError::Corrupt)?;
    let (commit_seq, recorded_version) = engine.query_row(
        r#"SELECT "commit_seq", "version" FROM "_norn_quantity_posting" WHERE "world"=?1 AND "ledger"=?2 AND "id"=?3"#,
        params![world, ledger, id], |row| Ok((row.get::<i64>(0)?, to_u64(row.get(1)?, "quantity posting version")?)),
    )?;
    let request = transition_request(engine, world, commit_seq)?;
    if request.ledger_id != ledger
        || receipt_evidence(
            engine,
            world,
            to_u64(commit_seq, "quantity posting commit")?,
        )?
        .quantity_result
        .as_ref()
        .map(|result| result.version)
            != Some(recorded_version)
        || recorded_version > state.version
    {
        return Err(QuantityError::Corrupt.into());
    }
    let receipt = receipt_evidence(
        engine,
        world,
        to_u64(commit_seq, "quantity posting commit")?,
    )?;
    if original.reverses.is_some()
        || receipt.quantity_result.as_ref().is_none_or(|result| {
            result.posting.as_ref() != Some(&(id.to_owned(), original.clone()))
        })
    {
        return Err(QuantityError::Corrupt.into());
    }

    Ok(())
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::engine::{NativeEngine, SqlValue};
    use crate::system::SYSTEM_DDL;

    #[test]
    fn quantity_integrity_lookups_seek_indexes() {
        let engine = NativeEngine::open_in_memory().expect("engine");
        for ddl in SYSTEM_DDL {
            engine.execute(ddl, &[]).expect("schema");
        }
        for (sql, index, key, bindings) in [
            (
                BOUNDARY_SQL,
                "_norn_receipt/quantity_boundary",
                "<expr>>?",
                vec![],
            ),
            (
                REVERSAL_PROJECTION_SQL,
                "_norn_quantity_posting/reversal",
                "reverses=?",
                vec!["main".into(), "stock".into(), "a".into()],
            ),
            (
                REVERSAL_EVIDENCE_SQL,
                "_norn_ledger/quantity_reversal",
                "<expr>=?",
                vec!["main".into(), "stock".into(), "a".into(), 0_i64.into()],
            ),
            (
                r#"SELECT commit_seq,timer_id,due,timer FROM "_norn_timer" INDEXED BY "_norn_timer/quantity_due" WHERE timer_id>='norn.quantity.expiry/' AND timer_id<'norn.quantity.expiry0' AND due<=?1 ORDER BY due,timer_id LIMIT 1"#,
                "_norn_timer/quantity_due",
                "due<?",
                vec![100_i64.into()],
            ),
            (
                LATEST_TRANSITION_SQL,
                "_norn_ledger/quantity_latest",
                "<expr>=?",
                vec!["main".into(), "stock".into(), 0_i64.into()],
            ),
        ] {
            let plan = engine
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), &bindings)
                .expect("plan");
            assert!(
                plan.iter()
                    .any(|row| matches!(row.get(3), Some(SqlValue::Text(detail))
                if detail.contains("SEARCH") && detail.contains(index) && detail.contains(key))),
                "{plan:?}"
            );
            assert!(
                !plan
                    .iter()
                    .any(|row| matches!(row.get(3), Some(SqlValue::Text(detail))
                if detail.contains("SCAN"))),
                "{plan:?}"
            );
        }
    }
}
