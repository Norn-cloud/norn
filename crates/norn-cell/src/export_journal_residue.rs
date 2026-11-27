//! EXPORT-RESIDUE-1 (#1533 Part 2, storage version 9): the compact local
//! encoding of a steady-state main-world export record.
//!
//! An export record repeats what the same transaction already stores: its
//! receipt is the `_norn_receipt` row, its entries are the commit's
//! `_norn_ledger` span, and its store format, model hashes, DDL and bindings
//! change only with a definitions install or a binding. A residue row keeps
//! the rest (`commitSeq`, `previous`, `batch`, `storageVersion`) and names
//! that context by its content address in `_norn_export_context`, so a cell's
//! whole steady-state history typically shares one context row.
//!
//! The portable record does not change: `_norn_export_journal.hash` is still
//! the Norn1 hash of the complete record, and every reader rebuilds the
//! record and checks it against that hash. Every other record (another world,
//! the first commit's initial model, a definitions install, a quantity,
//! consumed or automation commit, or a batch carrying a `command.result`
//! event whose body the packed encoding shares) keeps the packed encoding.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue as Value;
use norn_ir::canonical::HashProfile;
use serde::{Deserialize, Serialize};

use crate::codec::canonical_json;
use crate::engine::{MAX_SAFE_INTEGER, SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::export::{EXPORT_FORMAT, ExportRecord};
use crate::read::{LedgerEntry, to_i64, to_u64};
use crate::scrub::RowBudget;
use crate::store::{CommitBatch, MAIN_WORLD, Receipt};

/// The version prefix of a residue row's `record` text.
pub(crate) const PREFIX: &str = "norn.cell.export-residue/1:";

/// The domain of a context row's content address.
const CONTEXT_FORMAT: &str = "norn.cell.export-context/1";

/// The most rows one ledger entry may charge: its own row, its payload
/// reference and the referenced body ([`crate::payload_storage::decode_event_budgeted`]).
const ENTRY_ROWS: usize = 4;

/// `_norn_export_context`: one row per distinct context, at its Norn1 address.
pub(crate) const CONTEXT_DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_export_context" (
  "hash" TEXT NOT NULL PRIMARY KEY,
  "store_format" TEXT NOT NULL,
  "ddl_hash" TEXT NOT NULL,
  "model_hash" TEXT NOT NULL,
  "ddl_sql" TEXT NOT NULL,
  "bindings" TEXT NOT NULL
)"#;

/// Create the context table; called with the export journal's own schema.
pub(crate) fn init(engine: &impl SqlEngine) -> Result<(), CellError> {
    engine.execute_batch(CONTEXT_DDL)
}

fn corrupt(seq: u64, reason: &str) -> CellError {
    CellError::LedgerCorrupt {
        seq,
        reason: format!("export residue: {reason}"),
    }
}

/// What a residue row stores of its record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Residue {
    commit_seq: u64,
    context: String,
    previous: String,
    batch: CommitBatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage_version: Option<u32>,
}

/// The record fields a context row holds, in their content-address preimage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Context {
    format: String,
    store_format: String,
    ddl_hash: String,
    model_hash: String,
    ddl_sql: String,
    bindings: BTreeMap<String, String>,
}

impl Context {
    fn of(record: &ExportRecord) -> Self {
        Self {
            format: CONTEXT_FORMAT.to_owned(),
            store_format: record.store_format.clone(),
            ddl_hash: record.ddl_hash.clone(),
            model_hash: record.model_hash.clone(),
            ddl_sql: record.ddl_sql.clone(),
            bindings: record.bindings.clone(),
        }
    }
}

/// Whether `record` is a steady-state main-world commit the residue
/// reconstructs exactly from its receipt, ledger span and context.
pub(crate) fn eligible(record: &ExportRecord) -> bool {
    record.format == EXPORT_FORMAT
        && record.batch.world == MAIN_WORLD
        && record.quantity_input.is_none()
        && record.consumed_input.is_none()
        && record.initial_model.is_none()
        && record.definition_install.is_none()
        && record.automation_continuation.is_none()
        && record.automation_inline.is_none()
        && record.source_evidence.is_empty()
        // A result body is shared with the ledger's payload copy only by
        // the packed encoding; such a batch keeps it.
        && !record
            .batch
            .events
            .iter()
            .any(|event| event.get("type").and_then(Value::as_str) == Some("command.result"))
}

/// Write `record`'s context row (or prove the stored one equal) and return
/// its residue text. `canonical` is the record's Norn1 text: debug builds
/// rebuild the record from what was written and refuse the commit unless its
/// bytes are equal.
pub(crate) fn encode(
    engine: &impl SqlEngine,
    record: &ExportRecord,
    canonical: &str,
) -> Result<String, CellError> {
    let commit_seq = record.receipt.body.commit_seq;
    // The packed encoding checks the batch against the bound scope whenever
    // it stores a body (always its DDL); the residue keeps that refusal.
    crate::payload_storage::check_batch_scope(engine, &record.batch.scope)?;
    let preimage = canonical_json(&Context::of(record))?;
    let hash = HashProfile::Norn1.hash_canonical_bytes(preimage.as_bytes());
    if let Some(stored) = load_context(engine, &hash, commit_seq)? {
        if canonical_json(&stored)? != preimage {
            return Err(corrupt(
                commit_seq,
                "a context row disagrees with its address",
            ));
        }
    } else {
        // Stored as the preimage spells it (Norn1-normalized), so a read
        // recomputes the same address from the columns.
        let normal: Context = serde_json::from_str(&preimage)?;
        engine.execute(
                r#"INSERT INTO "_norn_export_context" ("hash","store_format","ddl_hash","model_hash","ddl_sql","bindings") VALUES (?1,?2,?3,?4,?5,?6)"#,
                params![
                    hash,
                    normal.store_format,
                    normal.ddl_hash,
                    normal.model_hash,
                    normal.ddl_sql,
                    canonical_json(&normal.bindings)?
                ],
            )?;
    }
    let residue = Residue {
        commit_seq,
        context: hash,
        previous: record.previous.clone(),
        batch: record.batch.clone(),
        storage_version: record.storage_version,
    };
    let text = format!("{PREFIX}{}", canonical_json(&residue)?);
    #[cfg(debug_assertions)]
    {
        let rebuilt = decode(
            engine,
            text.get(PREFIX.len()..).unwrap_or_default(),
            commit_seq,
        )?;
        if canonical_json(&rebuilt)? != canonical {
            return Err(corrupt(
                commit_seq,
                "the rebuilt record differs from the written one",
            ));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = canonical;
    Ok(text)
}

/// The context row at `hash`, after recomputing its address from its columns.
fn load_context(
    engine: &impl SqlEngine,
    hash: &str,
    commit_seq: u64,
) -> Result<Option<Context>, CellError> {
    let row: Option<(String, String, String, String, String)> = engine.query_optional(
        r#"SELECT "store_format","ddl_hash","model_hash","ddl_sql","bindings" FROM "_norn_export_context" WHERE "hash"=?1"#,
        params![hash],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    let Some((store_format, ddl_hash, model_hash, ddl_sql, bindings)) = row else {
        return Ok(None);
    };
    let context = Context {
        format: CONTEXT_FORMAT.to_owned(),
        store_format,
        ddl_hash,
        model_hash,
        ddl_sql,
        bindings: serde_json::from_str(&bindings)?,
    };
    if crate::codec::hash_json(&context)? != hash {
        return Err(corrupt(
            commit_seq,
            "a context row does not hash to its address",
        ));
    }
    Ok(Some(context))
}

/// Rebuild the record of residue text `encoded` (after [`PREFIX`]) at
/// `commit_seq`. The caller checks the rebuilt record's hash against the
/// journal row's, as it does for every encoding.
pub(crate) fn decode(
    engine: &impl SqlEngine,
    encoded: &str,
    commit_seq: u64,
) -> Result<ExportRecord, CellError> {
    let mut cursor = ResidueCursor::open(encoded, commit_seq)?;
    cursor
        .step(engine, &mut RowBudget::new(usize::MAX))?
        .ok_or_else(|| corrupt(commit_seq, "an unbounded read yielded"))
}

/// A residue record's resumable reader under a shared [`RowBudget`]: the
/// bound scope, the context row, the receipt row, then the ledger span in
/// pages, each row charged as it is read.
#[derive(Clone)]
pub(crate) struct ResidueCursor {
    residue: Residue,
    scope_checked: bool,
    context: Option<Context>,
    receipt: Option<Receipt>,
    entries: Vec<LedgerEntry>,
    ledger_done: bool,
}

impl ResidueCursor {
    /// Parse residue text `encoded` (after [`PREFIX`]) bound to `commit_seq`.
    pub(crate) fn open(encoded: &str, commit_seq: u64) -> Result<Self, CellError> {
        let value: Value = serde_json::from_str(encoded)?;
        let residue: Residue = norn_ir::runtime::decode(&value).map_err(CellError::Encoding)?;
        if residue.commit_seq != commit_seq {
            return Err(corrupt(commit_seq, "the residue names another commit"));
        }
        Ok(Self {
            residue,
            scope_checked: false,
            context: None,
            receipt: None,
            entries: Vec::new(),
            ledger_done: false,
        })
    }

    /// Advance within `budget`: `Ok(Some(record))` once rebuilt, `Ok(None)`
    /// when the budget is spent first.
    pub(crate) fn step(
        &mut self,
        engine: &impl SqlEngine,
        budget: &mut RowBudget,
    ) -> Result<Option<ExportRecord>, CellError> {
        let commit_seq = self.residue.commit_seq;
        if !self.scope_checked {
            if budget.exhausted() {
                return Ok(None);
            }
            crate::payload_storage::check_batch_scope(engine, &self.residue.batch.scope)?;
            budget.charge(1);
            self.scope_checked = true;
        }
        if self.context.is_none() {
            if budget.exhausted() {
                return Ok(None);
            }
            let context = load_context(engine, &self.residue.context, commit_seq)?
                .ok_or_else(|| corrupt(commit_seq, "the context row is missing"))?;
            budget.charge(1);
            self.context = Some(context);
        }
        if self.receipt.is_none() {
            if budget.exhausted() {
                return Ok(None);
            }
            self.receipt = Some(load_receipt(engine, commit_seq)?);
            budget.charge(1);
        }
        while !self.ledger_done {
            if budget.remaining() < ENTRY_ROWS {
                return Ok(None);
            }
            self.ledger_page(engine, budget)?;
        }
        self.assemble().map(Some)
    }

    /// Read the next ledger page of this commit, at most as many rows as
    /// `budget` can pay for at their highest cost.
    fn ledger_page(
        &mut self,
        engine: &impl SqlEngine,
        budget: &mut RowBudget,
    ) -> Result<(), CellError> {
        let page = budget.remaining() / ENTRY_ROWS;
        let limit = i64::try_from(page)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let after = self.entries.last().map_or(0, |entry| entry.seq);
        let rows = engine.query(
            r#"SELECT "seq","commit_seq","world","event","prev_hash","hash" FROM "_norn_ledger" WHERE "commit_seq"=?1 AND "seq">?2 ORDER BY "seq" LIMIT ?3"#,
            params![
                to_i64(self.residue.commit_seq, "commit_seq")?,
                to_i64(after, "seq")?,
                limit
            ],
        )?;
        if rows.len() < page {
            self.ledger_done = true;
        }
        for values in rows {
            budget.charge(1);
            let row = SqlRow(values);
            let seq = to_u64(row.get(0)?, "seq")?;
            let world: String = row.get(2)?;
            let event = crate::payload_storage::decode_event_budgeted(
                engine,
                seq,
                &world,
                &row.get::<String>(3)?,
                budget,
            )?;
            self.entries.push(LedgerEntry {
                seq,
                commit_seq: to_u64(row.get(1)?, "commit_seq")?,
                world,
                event,
                prev_hash: row.get(4)?,
                hash: row.get(5)?,
            });
        }
        Ok(())
    }

    /// The complete record: the residue's own fields, its context, receipt
    /// and ledger span, and every field eligibility requires absent.
    fn assemble(&self) -> Result<ExportRecord, CellError> {
        let commit_seq = self.residue.commit_seq;
        let (Some(context), Some(receipt)) = (&self.context, &self.receipt) else {
            return Err(corrupt(
                commit_seq,
                "the record was assembled before it was read",
            ));
        };
        Ok(ExportRecord {
            format: EXPORT_FORMAT.to_owned(),
            store_format: context.store_format.clone(),
            ddl_hash: context.ddl_hash.clone(),
            model_hash: context.model_hash.clone(),
            ddl_sql: context.ddl_sql.clone(),
            bindings: context.bindings.clone(),
            previous: self.residue.previous.clone(),
            batch: self.residue.batch.clone(),
            quantity_input: None,
            receipt: receipt.clone(),
            entries: self.entries.clone(),
            consumed_input: None,
            definition_install: None,
            initial_model: None,
            storage_version: self.residue.storage_version,
            automation_continuation: None,
            automation_inline: None,
            source_evidence: Vec::new(),
        })
    }
}

/// Whether `commit_seq`'s journal row is a residue. A diagnosis of the
/// provable journal prefix (`restore`) ends before a residue its source rows
/// no longer rebuild, as at a lost journal row.
#[cfg(feature = "native")]
pub(crate) fn is_residue(engine: &impl SqlEngine, commit_seq: u64) -> Result<bool, CellError> {
    let residue: Option<i64> = engine.query_optional(
        r#"SELECT substr("record",1,?2)=?3 FROM "_norn_export_journal" WHERE "commit_seq"=?1"#,
        params![
            to_i64(commit_seq, "commit_seq")?,
            i64::try_from(PREFIX.len()).map_err(|_overflow| CellError::OutOfRange("prefix"))?,
            PREFIX
        ],
        |row| row.get(0),
    )?;
    Ok(residue == Some(1))
}

/// The hash-verified receipt row of `commit_seq`.
fn load_receipt(engine: &impl SqlEngine, commit_seq: u64) -> Result<Receipt, CellError> {
    let rows = engine.query(
        r#"SELECT "commit_seq","world","idempotency_key","request_hash","receipt","receipt_hash" FROM "_norn_receipt" WHERE "commit_seq"=?1"#,
        params![to_i64(commit_seq, "commit_seq")?],
    )?;
    crate::read::decode_receipts(rows)?
        .into_iter()
        .find(|receipt| receipt.body.commit_seq == commit_seq)
        .ok_or_else(|| corrupt(commit_seq, "the receipt row is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_byte_bound_names_the_residue_prefix_and_its_length() {
        // `payload_storage::EXPORT_BYTES` is a SQL literal: it must test the
        // exact prefix at its exact length and read the context address
        // right after it.
        let bytes = crate::payload_storage::EXPORT_BYTES;
        let test = format!(
            r#"substr("_norn_export_journal"."record",1,{})='{PREFIX}'"#,
            PREFIX.len()
        );
        let address = format!(
            r#"json_extract(substr("_norn_export_journal"."record",{}),'$.context')"#,
            PREFIX.len().saturating_add(1)
        );
        assert!(bytes.contains(&test), "{bytes}");
        assert!(bytes.contains(&address), "{bytes}");
        assert_eq!(bytes.matches("export-residue").count(), 1);
    }
}
