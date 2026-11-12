//! DECISION-LEDGER-1: the cell's ledger of resolved decisions.
//!
//! A decision is a parked `norn.decision@1` perform of a flow run (PARKED-1):
//! while the parked command waits, the decision is open. The commit that
//! consumes the command's recorded result carries one [`DECISION_RESOLVED`]
//! ledger event naming the decision, the command and generation it resolves,
//! how it was resolved, the choice and the deciding principal; the store then
//! records it in `_norn_decision` inside the same SQLite transaction. A
//! decision resolves at most once: a second resolution of the same decision,
//! or one whose commit does not consume exactly that command's result,
//! refuses and nothing commits.
//!
//! The ledger is derived only from committed batches, so export replay
//! rebuilds it exactly, on every engine that runs the store's SQL. It answers
//! "who decided, when, and what" after the parked command is consumed; it is
//! an index for reads, never authority, and every read goes through the
//! caller's gates.

/// The `norn.decision@1` request a parked decision carries, and its bounds.
/// Hosts reach built-ins through the cell (`norn_cell::builtin_registered`).
pub use norn_builtins::decision::{
    DecisionOption, DecisionRequest, MAX_EXPIRES_IN_MS, MAX_TEXT_CHARS, OptionEffect, Risk,
    parse_request,
};
use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{StoreReader, to_i64};
use crate::store::{CommitBatch, Consumption};

/// The `type` of the ledger event that resolves a decision.
pub const DECISION_RESOLVED: &str = "norn.decision.resolved";

pub(crate) const DECISION: &str = "_norn_decision";

/// The decision ledger's DDL, applied with the other system tables.
pub(crate) const DDL: [&str; 2] = [
    r#"CREATE TABLE IF NOT EXISTS "_norn_decision" (
  "world" TEXT NOT NULL,
  "decision_id" TEXT NOT NULL,
  "command_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "run_id" TEXT NOT NULL,
  "step" TEXT NOT NULL,
  "resolution" TEXT NOT NULL,
  "choice" TEXT,
  "note" TEXT,
  "decided_by" TEXT,
  "idempotency_key" TEXT,
  "request_hash" TEXT,
  "resolved_at_ms" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("world", "decision_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_decision/run" ON "_norn_decision" ("world", "run_id", "decision_id")"#,
];

/// How a decision was resolved (DECISION-TERMINAL-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// A decider chose an option.
    Decided,
    /// Its deadline passed first.
    Expired,
    /// It was withdrawn before anyone decided.
    Withdrawn,
}

impl Resolution {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decided => "decided",
            Self::Expired => "expired",
            Self::Withdrawn => "withdrawn",
        }
    }

    fn parse(text: &str) -> Result<Self, CellError> {
        match text {
            "decided" => Ok(Self::Decided),
            "expired" => Ok(Self::Expired),
            "withdrawn" => Ok(Self::Withdrawn),
            _unknown => Err(refused("decision.resolution_invalid")),
        }
    }
}

/// What a [`DECISION_RESOLVED`] event records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionResolved {
    /// The decision id.
    pub decision_id: String,
    /// The parked command it resolves.
    pub command_id: String,
    /// The command's generation.
    pub generation: u64,
    /// The run that waited on it.
    pub run_id: String,
    /// The flow step that asked.
    pub step: String,
    /// How it was resolved.
    pub resolution: Resolution,
    /// The chosen option id, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// The decider's note, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The deciding principal's subject id, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// The decider's idempotency key, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The digest of the decider's request (choice and note), when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
}

impl DecisionResolved {
    /// The ledger event a resolving commit carries.
    ///
    /// # Errors
    /// A value outside the runtime value domain.
    pub fn event(&self) -> Result<Value, CellError> {
        Ok(Value::Object(
            [
                (
                    "type".to_owned(),
                    Value::String(DECISION_RESOLVED.to_owned()),
                ),
                (
                    "decision".to_owned(),
                    norn_ir::runtime::encode(self).map_err(CellError::Encoding)?,
                ),
            ]
            .into(),
        ))
    }
}

/// One resolved decision, as the ledger records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    /// The event's fields.
    pub resolved: DecisionResolved,
    /// The resolving commit's logical time, in milliseconds.
    pub resolved_at_ms: u64,
    /// The resolving commit.
    pub commit_seq: u64,
}

fn refused(code: &'static str) -> CellError {
    CellError::DecisionRefused(code)
}

/// Whether a ledger event is a decision resolution (host bookkeeping, not a
/// domain event a typed commit reader decodes).
#[must_use]
pub fn is_decision_resolved(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some(DECISION_RESOLVED)
}

fn resolutions(batch: &CommitBatch) -> Result<Vec<DecisionResolved>, CellError> {
    let mut found = Vec::new();
    for event in batch
        .events
        .iter()
        .filter(|event| is_decision_resolved(event))
    {
        let object = event
            .as_object()
            .ok_or_else(|| refused("decision.resolution_invalid"))?;
        if object.len() != 2 {
            return Err(refused("decision.resolution_invalid"));
        }
        let resolved: DecisionResolved = object
            .get("decision")
            .and_then(|decision| norn_ir::runtime::decode(decision).ok())
            .ok_or_else(|| refused("decision.resolution_invalid"))?;
        found.push(resolved);
    }
    // One decision per commit: it consumes exactly its own command's result.
    if found.len() > 1 {
        return Err(refused("decision.resolution_invalid"));
    }
    if let Some(resolved) = found.first() {
        let consumes_it = matches!(
            &batch.consumes,
            Some(Consumption::Result { command_id, generation })
                if *command_id == resolved.command_id && *generation == resolved.generation
        );
        let decided = resolved.resolution == Resolution::Decided;
        if !consumes_it
            || resolved.decision_id.is_empty()
            || resolved.run_id.is_empty()
            || decided != resolved.choice.is_some()
            || decided != resolved.decided_by.is_some()
        {
            return Err(refused("decision.resolution_invalid"));
        }
    }
    Ok(found)
}

/// Before `batch`'s writes apply: a decision it resolves must be open.
///
/// # Errors
/// `decision.resolution_invalid`, `decision.resolved`, or SQL engine errors.
pub(crate) fn check<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Result<(), CellError> {
    for resolved in resolutions(batch)? {
        if reader
            .decision_resolution(&batch.world, &resolved.decision_id)?
            .is_some()
        {
            return Err(refused("decision.resolved"));
        }
    }
    Ok(())
}

/// Record the decision `batch` resolves, inside the commit's transaction.
///
/// # Errors
/// As [`check`]; SQL engine errors.
pub(crate) fn apply<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    commit_seq: u64,
) -> Result<(), CellError> {
    for resolved in resolutions(batch)? {
        reader.conn.execute(
            &format!(
                r#"INSERT INTO "{DECISION}" ("world","decision_id","command_id","generation","run_id","step","resolution","choice","note","decided_by","idempotency_key","request_hash","resolved_at_ms","commit_seq") VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#
            ),
            params![
                batch.world,
                resolved.decision_id,
                resolved.command_id,
                to_i64(resolved.generation, "decision generation")?,
                resolved.run_id,
                resolved.step,
                resolved.resolution.as_str(),
                resolved.choice,
                resolved.note,
                resolved.decided_by,
                resolved.idempotency_key,
                resolved.request_hash,
                to_i64(batch.logical_time, "logical time")?,
                to_i64(commit_seq, "commit sequence")?
            ],
        )?;
    }
    Ok(())
}

fn decode(row: &SqlRow) -> Result<DecisionRow, CellError> {
    let unsigned = |index: usize, what: &'static str| {
        u64::try_from(row.get::<i64>(index)?).map_err(|_negative| CellError::OutOfRange(what))
    };
    Ok(DecisionRow {
        resolved: DecisionResolved {
            decision_id: row.get(0)?,
            command_id: row.get(1)?,
            generation: unsigned(2, "decision generation")?,
            run_id: row.get(3)?,
            step: row.get(4)?,
            resolution: Resolution::parse(&row.get::<String>(5)?)?,
            choice: row.get(6)?,
            note: row.get(7)?,
            decided_by: row.get(8)?,
            idempotency_key: row.get(9)?,
            request_hash: row.get(10)?,
        },
        resolved_at_ms: unsigned(11, "decision resolved time")?,
        commit_seq: unsigned(12, "decision commit")?,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// How `decision_id` was resolved in `world`, once it was.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn decision_resolution(
        &self,
        world: &str,
        decision_id: &str,
    ) -> Result<Option<DecisionRow>, CellError> {
        self.conn.query_optional(
            &format!(
                r#"SELECT "decision_id","command_id","generation","run_id","step","resolution","choice","note","decided_by","idempotency_key","request_hash","resolved_at_ms","commit_seq" FROM "{DECISION}" WHERE "world"=?1 AND "decision_id"=?2"#
            ),
            params![world, decision_id],
            decode,
        )
    }
}
