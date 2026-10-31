//! Transactional journal of the cell commits that still need object-store export.

use std::collections::BTreeMap;
#[cfg(feature = "native")]
use std::sync::Arc;

use norn_ir::RuntimeValue as Value;
use serde::Serialize;
#[cfg(feature = "native")]
use tokio::sync::Notify;

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::export::{EXPORT_FORMAT, ExportRecord};
use crate::model::Model;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::store::{CellStore, CommitBatch, Consumption, DefinitionInstallRecord, Receipt};
use crate::system::{GENESIS_HASH, META};

pub(crate) const JOURNAL_DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_export_journal" (
  "commit_seq" INTEGER NOT NULL,
  "hash" TEXT NOT NULL,
  "record" TEXT NOT NULL,
  PRIMARY KEY ("commit_seq")
)"#;

/// Create the local export journal table in the store's initialization transaction.
pub(crate) fn init(engine: &impl SqlEngine) -> Result<(), CellError> {
    engine.execute_batch(JOURNAL_DDL)
}

/// Read the hash-verified journal preimage without recursively verifying its
/// events. Historical event verification uses this to recover its pinned model.
pub(crate) fn read_record(
    engine: &impl SqlEngine,
    commit_seq: u64,
) -> Result<Option<ExportRecord>, CellError> {
    let row: Option<(String, String)> = engine.query_optional(
        r#"SELECT "hash", "record" FROM "_norn_export_journal" WHERE "commit_seq" = ?1"#,
        params![to_i64(commit_seq, "commit_seq")?],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some((stored_hash, serialized)) = row else {
        return Ok(None);
    };
    let record = crate::payload_storage::decode_export(engine, &serialized, commit_seq)?;
    if record.format != EXPORT_FORMAT
        || record.receipt.body.commit_seq != commit_seq
        || record.hash()? != stored_hash
    {
        return Err(CellError::LedgerCorrupt {
            seq: commit_seq,
            reason: "export journal record does not verify".to_owned(),
        });
    }
    Ok(Some(record))
}

/// Capture the exact input being consumed before the commit marks it consumed.
pub(crate) fn consumed_input<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    consumed: Option<&Consumption>,
) -> Result<Option<Value>, CellError> {
    let Some(consumed) = consumed else {
        return Ok(None);
    };

    let not_pending = || CellError::NotPending(consumed.clone());
    match consumed {
        Consumption::Observation { observation_id } => {
            let pending = reader
                .pending_observations()?
                .into_iter()
                .find(|row| row.observation_id == *observation_id)
                .ok_or_else(not_pending)?;
            preimage(&ConsumedInput::Observation {
                position: pending.position,
                observation_id: pending.observation_id,
                observed: pending.observed,
            })
        }
        Consumption::Result {
            command_id,
            generation,
        } => {
            let pending = reader
                .head_command()?
                .filter(|row| {
                    row.command.command_id == *command_id
                        && row.command.generation == *generation
                        && row.result.is_some()
                })
                .ok_or_else(not_pending)?;
            let runs = reader.runs(command_id)?;
            preimage(&ConsumedInput::Result {
                position: pending.position,
                commit_seq: pending.commit_seq,
                command: pending.command,
                result: pending.result,
                runs,
            })
        }
        Consumption::Timer {
            timer_id,
            incarnation,
        } => {
            let pending = reader
                .pending_timer(timer_id)?
                .filter(|row| row.commit_seq == *incarnation)
                .ok_or_else(not_pending)?;
            preimage(&ConsumedInput::Timer {
                commit_seq: pending.commit_seq,
                timer: pending.timer,
            })
        }
    }
}

/// The preimage of a consumed delivery, as recorded in the export.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum ConsumedInput {
    Observation {
        position: u64,
        observation_id: String,
        observed: Value,
    },
    Result {
        position: u64,
        commit_seq: u64,
        command: crate::store::OutboxCommand,
        result: Option<Value>,
        runs: Vec<crate::read::RunRecord>,
    },
    Timer {
        commit_seq: u64,
        timer: crate::store::TimerEntry,
    },
}

/// `input` as an exact runtime value.
fn preimage(input: &ConsumedInput) -> Result<Option<Value>, CellError> {
    norn_ir::runtime::encode(input)
        .map(Some)
        .map_err(|reason| CellError::LedgerCorrupt {
            seq: 0,
            reason: format!("the consumed input does not encode: {reason}"),
        })
}

/// Persist a commit's replayable export record in the same engine transaction.
pub(crate) fn record<E: SqlEngine>(
    engine: &E,
    model: &Model,
    batch: &CommitBatch,
    quantity_input: Option<Value>,
    receipt: &Receipt,
    consumed_input: Option<Value>,
    definition_install: Option<DefinitionInstallRecord>,
) -> Result<(), CellError> {
    let commit_seq = receipt.body.commit_seq;
    let prior: Option<(i64, String)> = engine
        .query_optional(
            r#"SELECT "commit_seq", "hash" FROM "_norn_export_journal" ORDER BY "commit_seq" DESC LIMIT 1"#,
            &[],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    let previous = match prior {
        Some((seq, hash)) if to_u64(seq, "export commit_seq")? < commit_seq => hash,
        Some((seq, _)) => {
            return Err(CellError::LedgerCorrupt {
                seq: to_u64(seq, "export commit_seq")?,
                reason: "export journal is ahead of the committed receipt".to_owned(),
            });
        }
        None if commit_seq == 1 => GENESIS_HASH.to_owned(),
        // A pre-export cell file has receipts but no original CommitBatch payloads.
        // Keep new writes working while leaving a visible chain gap that the exporter
        // rejects instead of presenting a partial export as complete.
        None => String::new(),
    };

    let reader = StoreReader::new(engine, model);
    let entries: Vec<_> = reader
        .ledger(receipt.body.first_event_seq)?
        .into_iter()
        .filter(|entry| entry.commit_seq == commit_seq)
        .collect();
    if u64::try_from(entries.len()).map_err(|_overflow| CellError::OutOfRange("event count"))?
        != receipt.body.event_count
    {
        return Err(CellError::LedgerCorrupt {
            seq: commit_seq,
            reason: "export journal event span disagrees with the receipt".to_owned(),
        });
    }

    let mut bindings = BTreeMap::new();
    let rows = engine.query(&format!(
        "SELECT \"key\", \"value\" FROM \"{META}\" WHERE \"key\" LIKE 'binding:%' ORDER BY \"key\""
    ), &[])?;
    for values in rows {
        let row = SqlRow(values);
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        if let Some(key) = key.strip_prefix("binding:") {
            bindings.insert(key.to_owned(), value);
        }
    }

    let export = ExportRecord {
        format: EXPORT_FORMAT.to_owned(),
        store_format: engine.query_row(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\"='format'"),
            &[],
            |row| row.get(0),
        )?,
        ddl_hash: model.ddl_hash().to_owned(),
        model_hash: model.model_hash().to_owned(),
        ddl_sql: model.ddl_sql().to_owned(),
        bindings,
        previous,
        batch: batch.clone(),
        quantity_input,
        receipt: receipt.clone(),
        entries,
        consumed_input,
        initial_model: (commit_seq == 1 && definition_install.is_none())
            .then(|| model.definitions_snapshot()),
        definition_install,
    };
    for entry in &export.entries {
        reader.verify_durable_event(&entry.world, entry.commit_seq, &entry.event)?;
    }
    let hash = export.hash()?;
    let serialized = crate::payload_storage::encode_export(engine, &export)?;
    engine.execute(
        r#"INSERT INTO "_norn_export_journal" ("commit_seq", "hash", "record") VALUES (?1, ?2, ?3)"#,
        params![to_i64(commit_seq, "commit_seq")?, hash, serialized],
    )?;
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Return the current commit head without rechecking the complete ledger.
    ///
    /// # Errors
    ///
    /// Engine or sequence-range errors while reading the receipt head.
    pub fn export_commit_count(&self) -> Result<u64, CellError> {
        Ok(self.read().next_commit_seq()?.saturating_sub(1))
    }

    /// Return the immutable export record for one commit, verifying its local Norn1 hash.
    ///
    /// # Errors
    ///
    /// Engine errors, invalid JSON, or an export record whose content hash does not verify.
    pub fn export_record(&self, commit_seq: u64) -> Result<Option<ExportRecord>, CellError> {
        let Some(record) = read_record(&self.conn, commit_seq)? else {
            return Ok(None);
        };
        for entry in &record.entries {
            self.read()
                .verify_durable_event(&entry.world, entry.commit_seq, &entry.event)?;
        }
        Ok(Some(record))
    }

    /// A wake-up signal for an exporter waiting for new committed records.
    #[cfg(feature = "native")]
    #[must_use]
    pub fn export_notification(&self) -> Arc<Notify> {
        Arc::clone(&self.export_notify)
    }

    /// Whether a restored command is fenced from automatic external execution.
    ///
    /// A fence is read-only here; hosts must not clear it or resend that
    /// command generation without an explicit reconciliation policy.
    ///
    /// # Errors
    ///
    /// A SQLite read failure.
    pub fn perform_is_fenced(&self, command_id: &str) -> Result<bool, CellError> {
        Ok(self.perform_fence_generation(command_id)?.is_some())
    }

    /// The generation fenced by the most recent restore, when present.
    /// Hosts may authorize a later generation only after ledgered reconciliation.
    ///
    /// # Errors
    /// A store read or corrupt fence value.
    pub fn perform_fence_generation(&self, command_id: &str) -> Result<Option<u64>, CellError> {
        let fenced: Option<String> = self.conn.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![format!("restore:perform_fence:{command_id}")],
            |row| row.get(0),
        )?;
        fenced
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|error| CellError::LedgerCorrupt {
                        seq: 0,
                        reason: format!("invalid restore fence generation: {error}"),
                    })
            })
            .transpose()
    }
}
