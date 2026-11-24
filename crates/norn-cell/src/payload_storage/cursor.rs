//! Resumable, row-budget-bounded packed payload & export-journal readers (S10).

use std::collections::BTreeSet;

use norn_ir::RuntimeValue as Value;

use super::{
    PREFIX, Packed, Reference, check_scope, corrupt, load, normalized, placeholder, target,
};
use crate::CellError;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::export::{EXPORT_FORMAT, ExportRecord};
use crate::read::{to_i64, to_u64};
use crate::scrub::RowBudget;

/// Decode one ledger event, charging exact SQL rows read (`0` inline, `3` packed)
/// to `budget`.
pub(crate) fn decode_event_budgeted(
    engine: &impl SqlEngine,
    seq: u64,
    world: &str,
    encoded: &str,
    budget: &mut RowBudget,
) -> Result<Value, CellError> {
    let reference: Option<Reference> = engine.query_optional(
        r#"SELECT "scope","world","hash" FROM "_norn_ledger_payload" WHERE "seq"=?1"#,
        params![to_i64(seq, "payload event seq")?],
        |row| {
            Ok(Reference {
                scope: row.get(0)?,
                world: row.get(1)?,
                hash: row.get(2)?,
            })
        },
    )?;
    let mut event: Value = serde_json::from_str(encoded)?;
    if let Some(reference) = reference {
        budget.charge(1);
        if reference.world != normalized(world)?
            || event.get("type").and_then(Value::as_str) != Some("command.result")
            || event.get("result") != Some(&placeholder(&reference))
        {
            return Err(corrupt());
        }
        let body = load(engine, &reference)?;
        budget.charge(2);
        event
            .as_object_mut()
            .ok_or_else(corrupt)?
            .insert("result".to_owned(), body);
    }
    Ok(event)
}

#[derive(Clone)]
struct PackedExportState {
    commit_seq: u64,
    stored_hash: String,
    packed: Packed,
    value: Value,
    scope_checked: bool,
    ordinals_done: bool,
    last_ordinal: Option<i64>,
    ordinal_idx: usize,
    blobs_loaded: usize,
}

#[derive(Clone)]
enum ExportDecodeState {
    Ready(Option<Box<ExportRecord>>),
    Packed(Box<PackedExportState>),
}

/// Resumable reader for one `_norn_export_journal` row and all of its packed
/// `_norn_export_payload` / `_norn_payload` rows under a shared [`RowBudget`].
#[derive(Clone)]
pub(crate) struct ExportDecodeCursor(ExportDecodeState);

impl ExportDecodeCursor {
    /// Wrap an already-decoded record so a caller can resume after yielding.
    pub(crate) fn ready(record: ExportRecord) -> Self {
        Self(ExportDecodeState::Ready(Some(Box::new(record))))
    }

    /// Fetch the journal row for `commit_seq` (`<= 1` row charged to `budget`)
    /// and prepare resumable decoding. Returns `Ok(None)` when absent.
    pub(crate) fn open(
        engine: &impl SqlEngine,
        commit_seq: u64,
        budget: &mut RowBudget,
    ) -> Result<Option<Self>, CellError> {
        let row: Option<(String, String)> = engine.query_optional(
            r#"SELECT "hash", "record" FROM "_norn_export_journal" WHERE "commit_seq" = ?1"#,
            params![to_i64(commit_seq, "commit_seq")?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let Some((stored_hash, serialized)) = row else {
            return Ok(None);
        };
        budget.charge(1);
        let Some(encoded) = serialized.strip_prefix(PREFIX) else {
            let record: ExportRecord = serde_json::from_str(&serialized)?;
            verify_export_envelope(&record, commit_seq, &stored_hash)?;
            return Ok(Some(Self::ready(record)));
        };
        let packed: Packed = serde_json::from_str(encoded)?;
        if packed.commit_seq != commit_seq {
            return Err(corrupt());
        }
        let mut value: Value = serde_json::from_str(&packed.inline)?;
        let mut seen = BTreeSet::new();
        for at in &packed.references {
            if at.reference.scope != packed.scope
                || at.reference.world != packed.world
                || !seen.insert(at.path.clone())
                || *target(&mut value, &at.path)? != Value::Null
            {
                return Err(corrupt());
            }
        }
        Ok(Some(Self(ExportDecodeState::Packed(Box::new(
            PackedExportState {
                commit_seq,
                stored_hash,
                packed,
                value,
                scope_checked: false,
                ordinals_done: false,
                last_ordinal: None,
                ordinal_idx: 0,
                blobs_loaded: 0,
            },
        )))))
    }

    /// Advance decoding within `budget`; returns `Ok(Some(record))` once
    /// verified, or `Ok(None)` when the wake budget is exhausted.
    pub(crate) fn step(
        &mut self,
        engine: &impl SqlEngine,
        budget: &mut RowBudget,
    ) -> Result<Option<ExportRecord>, CellError> {
        match &mut self.0 {
            ExportDecodeState::Ready(record) => record.take().map(|boxed| Ok(*boxed)).transpose(),
            ExportDecodeState::Packed(state) => step_packed(engine, state, budget),
        }
    }
}

fn step_packed(
    engine: &impl SqlEngine,
    state: &mut PackedExportState,
    budget: &mut RowBudget,
) -> Result<Option<ExportRecord>, CellError> {
    if !state.scope_checked {
        if budget.exhausted() {
            return Ok(None);
        }
        check_scope(engine, &state.packed.scope)?;
        budget.charge(1);
        state.scope_checked = true;
    }
    while !state.ordinals_done {
        if budget.exhausted() {
            return Ok(None);
        }
        let limit = budget.remaining();
        let limit_i64 = to_i64(
            u64::try_from(limit).unwrap_or(u64::MAX),
            "payload ordinal limit",
        )?;
        let seq_i64 = to_i64(state.commit_seq, "payload export seq")?;
        let rows = match state.last_ordinal {
            None => engine.query(
                r#"SELECT "ordinal", "hash" FROM "_norn_export_payload" WHERE "commit_seq"=?1 ORDER BY "ordinal" LIMIT ?2"#,
                params![seq_i64, limit_i64],
            )?,
            Some(last) => engine.query(
                r#"SELECT "ordinal", "hash" FROM "_norn_export_payload" WHERE "commit_seq"=?1 AND "ordinal">?2 ORDER BY "ordinal" LIMIT ?3"#,
                params![seq_i64, last, limit_i64],
            )?,
        };
        let count = rows.len();
        budget.charge(count);
        for raw_row in rows {
            let row = SqlRow(raw_row);
            let raw_ordinal: i64 = row.get(0)?;
            let _ = to_u64(raw_ordinal, "payload export ordinal")?;
            let hash: String = row.get(1)?;
            let expected = state
                .packed
                .references
                .get(state.ordinal_idx)
                .ok_or_else(corrupt)?;
            if expected.reference.hash != hash {
                return Err(corrupt());
            }
            state.ordinal_idx = state.ordinal_idx.saturating_add(1);
            state.last_ordinal = Some(raw_ordinal);
        }
        if count < limit {
            if state.ordinal_idx != state.packed.references.len() {
                return Err(corrupt());
            }
            state.ordinals_done = true;
        }
    }
    while state.blobs_loaded < state.packed.references.len() {
        if budget.remaining() < 2 {
            return Ok(None);
        }
        let at = state
            .packed
            .references
            .get(state.blobs_loaded)
            .ok_or_else(corrupt)?;
        let body = load(engine, &at.reference)?;
        budget.charge(2);
        *target(&mut state.value, &at.path)? = body;
        state.blobs_loaded = state.blobs_loaded.saturating_add(1);
    }
    let record: ExportRecord =
        norn_ir::runtime::decode(&state.value).map_err(CellError::Encoding)?;
    if record.batch.scope != state.packed.scope
        || record.batch.world != state.packed.world
        || record.receipt.body.commit_seq != state.packed.commit_seq
    {
        return Err(corrupt());
    }
    verify_export_envelope(&record, state.commit_seq, &state.stored_hash)?;
    Ok(Some(record))
}

fn verify_export_envelope(
    record: &ExportRecord,
    commit_seq: u64,
    stored_hash: &str,
) -> Result<(), CellError> {
    if record.format != EXPORT_FORMAT
        || record.receipt.body.commit_seq != commit_seq
        || record.hash()? != stored_hash
    {
        return Err(CellError::LedgerCorrupt {
            seq: commit_seq,
            reason: "export journal record does not verify".to_owned(),
        });
    }
    Ok(())
}
