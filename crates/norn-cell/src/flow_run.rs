//! RUN-LEDGER-1: the cell's ledger of admitted flow runs.
//!
//! A run is the object of its flow's generated Run type, whose lifecycle is
//! the flow's state machine. A commit starts a run by carrying one
//! [`RUN_STARTED`] ledger event together with the write that creates the run
//! object; the store then records the run in `_norn_flow_run` inside the same
//! SQLite transaction. Every later commit that writes a run object refreshes
//! the row's state, version and update time in that commit's transaction.
//!
//! The ledger is derived only from committed batches (their events and
//! writes), so export replay rebuilds it exactly, on every engine that runs
//! the store's SQL (the native engine and `WorkersEngine` alike). The row is
//! an index for reads, never authority: run state is the run object's
//! lifecycle property, and every read goes through the caller's read gates.

use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{StoreReader, to_i64};
use crate::store::{CommitBatch, WrittenRow};

/// ACTION-STEP-1: a flow step that calls a governed action, executed by the
/// cell under the run starter's authority. Hosts reach the built-in through
/// the cell.
pub use norn_builtins::action::{
    ActionStep, FUNCTION_ID as ACTION_FUNCTION, StepRefusal as ActionStepRefusal,
    parse_step as parse_action_step,
};

/// The `type` of the ledger event that starts a run.
pub const RUN_STARTED: &str = "norn.run.started";
/// The first generation of every run.
pub const FIRST_GENERATION: u64 = 1;
/// The most rows one [`StoreReader::flow_runs`] page returns.
pub const MAX_RUN_PAGE: u32 = 200;
/// The most ledger events [`StoreReader::run_start`] reads from a starting
/// commit; a commit's events are bounded by the turn envelope.
const MAX_START_EVENTS: usize = 4096;

pub(crate) const FLOW_RUN: &str = "_norn_flow_run";

/// The run ledger's DDL, applied with the other system tables.
pub(crate) const DDL: [&str; 3] = [
    r#"CREATE TABLE IF NOT EXISTS "_norn_flow_run" (
  "world" TEXT NOT NULL,
  "run_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "flow_id" TEXT NOT NULL,
  "flow_version" TEXT NOT NULL,
  "object_type" TEXT NOT NULL,
  "state_property" TEXT NOT NULL,
  "state" TEXT,
  "version" INTEGER NOT NULL,
  "created_at_ms" INTEGER NOT NULL,
  "created_commit_seq" INTEGER NOT NULL,
  "updated_at_ms" INTEGER NOT NULL,
  "updated_commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("world", "run_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_flow_run/object" ON "_norn_flow_run" ("world", "object_type", "run_id")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_flow_run/flow_state" ON "_norn_flow_run" ("world", "flow_id", "state", "run_id")"#,
];

/// The installed flow a run executes, pinned by artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowRef {
    /// The flow's rule artifact id.
    pub id: String,
    /// The flow's rule artifact version.
    pub version: String,
}

/// The verified principal who started a run, as the host recorded it from
/// the request that started it (ACTION-STEP-1). A run's action steps act
/// under exactly this authority for the whole run: the roles are those the
/// host verified at start, so a role revoked later still holds for this run
/// until it ends (a waiting run is bounded by its decisions' seven-day
/// expiry). Host bookkeeping, never a client claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunStarter {
    /// The verified subject id.
    pub subject: String,
    /// `user` or `service`.
    pub actor_kind: String,
    /// The roles the host verified, from `role_table`.
    pub roles: Vec<String>,
    /// The declared role table the roles came from.
    pub role_table: String,
}

/// What a [`RUN_STARTED`] event records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunStart {
    /// The run id: the run object's key.
    pub run_id: String,
    /// The flow it runs.
    pub flow: FlowRef,
    /// The flow's Run object type.
    pub object_type: String,
    /// The Run type's lifecycle property (the flow's state machine).
    pub state_property: String,
    /// Who started it, when the host recorded a verified principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<RunStarter>,
}

impl RunStart {
    /// The ledger event a starting commit carries.
    ///
    /// # Errors
    /// A value outside the runtime value domain.
    pub fn event(&self) -> Result<Value, CellError> {
        Ok(Value::Object(
            [
                ("type".to_owned(), Value::String(RUN_STARTED.to_owned())),
                (
                    "run".to_owned(),
                    norn_ir::runtime::encode(self).map_err(CellError::Encoding)?,
                ),
            ]
            .into(),
        ))
    }
}

/// One run, as the ledger records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRow {
    /// The run id.
    pub run_id: String,
    /// The request generation; a re-issue or cancellation bumps it.
    pub generation: u64,
    /// The flow it runs.
    pub flow: FlowRef,
    /// The flow's Run object type.
    pub object_type: String,
    /// The current lifecycle state; `None` once the run object is deleted.
    pub state: Option<String>,
    /// The run object's current version.
    pub version: u64,
    /// The starting commit's logical time, in milliseconds.
    pub created_at_ms: u64,
    /// The starting commit.
    pub created_commit_seq: u64,
    /// The logical time of the last commit that wrote the run object.
    pub updated_at_ms: u64,
    /// The last commit that wrote the run object.
    pub updated_commit_seq: u64,
}

/// A page request over the run ledger, in run-id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    /// Only runs of this flow id.
    pub flow: Option<String>,
    /// Only runs in this state.
    pub state: Option<String>,
    /// Only runs whose id sorts after this cursor.
    pub after: Option<String>,
    /// At most this many rows (clamped to `1..=MAX_RUN_PAGE`).
    pub limit: u32,
}

fn refused(code: &'static str) -> CellError {
    CellError::RunRefused(code)
}

fn starts(batch: &CommitBatch) -> Result<Vec<RunStart>, CellError> {
    let mut starts = Vec::new();
    for event in &batch.events {
        if event.get("type").and_then(Value::as_str) != Some(RUN_STARTED) {
            continue;
        }
        let object = event
            .as_object()
            .ok_or_else(|| refused("run.start_invalid"))?;
        if object.len() != 2 {
            return Err(refused("run.start_invalid"));
        }
        let start: RunStart = object
            .get("run")
            .and_then(|run| norn_ir::runtime::decode(run).ok())
            .ok_or_else(|| refused("run.start_invalid"))?;
        if start.run_id.is_empty()
            || start.flow.id.is_empty()
            || start.flow.version.is_empty()
            || starts
                .iter()
                .any(|seen: &RunStart| seen.run_id == start.run_id)
        {
            return Err(refused("run.start_invalid"));
        }
        starts.push(start);
    }
    Ok(starts)
}

fn state_of<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    object_type: &str,
    run_id: &str,
    property: &str,
) -> Result<Option<String>, CellError> {
    let Some(object) = reader.object(world, object_type, run_id)? else {
        return Ok(None);
    };
    match object.values.get(property) {
        Some(Value::String(state)) => Ok(Some(state.clone())),
        _ => Err(refused("run.state_invalid")),
    }
}

/// Before `batch`'s writes apply: every run it starts must be new. A run id
/// already started in this world refuses `run.exists`; a run object that
/// already exists (untracked, or written before the start) refuses
/// `run.start_invalid`, so only a creating write starts a run.
///
/// # Errors
/// `run.exists`, `run.start_invalid`, or SQL engine errors.
pub(crate) fn check_starts<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Result<(), CellError> {
    for start in starts(batch)? {
        if reader.flow_run(&batch.world, &start.run_id)?.is_some() {
            return Err(refused("run.exists"));
        }
        // The state property must be a declared lifecycle of the Run type, or
        // the run could never reach a terminal state and would count as live.
        let lifecycle = reader
            .model()
            .object_types()
            .find(|declared| declared.name.as_str() == start.object_type)
            .is_some_and(|declared| {
                declared
                    .lifecycles
                    .iter()
                    .any(|lifecycle| lifecycle.property.as_str() == start.state_property)
            });
        if !lifecycle
            || reader
                .object(&batch.world, &start.object_type, &start.run_id)?
                .is_some()
        {
            return Err(refused("run.start_invalid"));
        }
    }
    Ok(())
}

/// Whether a ledger event is a run start (host bookkeeping, not a domain
/// event a typed commit reader decodes).
#[must_use]
pub fn is_run_start(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some(RUN_STARTED)
}

/// Record the runs `batch` starts and refresh every run it writes, inside the
/// commit's transaction (after its writes are applied).
///
/// # Errors
/// `run.start_invalid` for a malformed start or one without its creating
/// write, `run.exists` for a run id already started in this world,
/// `run.state_invalid` when a run object's lifecycle property is not a state;
/// SQL engine errors.
pub(crate) fn apply<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    written: &[WrittenRow],
    commit_seq: u64,
) -> Result<(), CellError> {
    let conn = reader.conn;
    let world = batch.world.as_str();
    let now = to_i64(batch.logical_time, "logical time")?;
    let seq = to_i64(commit_seq, "commit sequence")?;
    let starts = starts(batch)?;
    for start in &starts {
        // The starting commit creates the run object, of the Run type it names.
        // Several writes to the run object in one batch: the last one holds
        // the final version (the state read below is the final state too).
        let created = written
            .iter()
            .rfind(|row| row.object_type == start.object_type && row.key == start.run_id)
            .ok_or_else(|| refused("run.start_invalid"))?;
        let state = state_of(
            reader,
            world,
            &start.object_type,
            &start.run_id,
            &start.state_property,
        )?
        .ok_or_else(|| refused("run.start_invalid"))?;
        let exists = conn.query_optional(
            &format!(r#"SELECT 1 FROM "{FLOW_RUN}" WHERE "world"=?1 AND "run_id"=?2"#),
            params![world, start.run_id],
            |_row| Ok(()),
        )?;
        if exists.is_some() {
            return Err(refused("run.exists"));
        }
        conn.execute(
            &format!(
                r#"INSERT INTO "{FLOW_RUN}" ("world","run_id","generation","flow_id","flow_version","object_type","state_property","state","version","created_at_ms","created_commit_seq","updated_at_ms","updated_commit_seq") VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?10,?11)"#
            ),
            params![
                world,
                start.run_id,
                to_i64(FIRST_GENERATION, "run generation")?,
                start.flow.id,
                start.flow.version,
                start.object_type,
                start.state_property,
                state,
                to_i64(created.version, "run version")?,
                now,
                seq
            ],
        )?;
    }
    for row in written {
        if starts
            .iter()
            .any(|start| start.run_id == row.key && start.object_type == row.object_type)
        {
            continue;
        }
        let tracked = conn.query_optional(
            &format!(
                r#"SELECT "state_property" FROM "{FLOW_RUN}" INDEXED BY "_norn_flow_run/object" WHERE "world"=?1 AND "object_type"=?2 AND "run_id"=?3"#
            ),
            params![world, row.object_type, row.key],
            |found| found.get::<String>(0),
        )?;
        let Some(property) = tracked else {
            continue;
        };
        let state = state_of(reader, world, &row.object_type, &row.key, &property)?;
        conn.execute(
            &format!(
                r#"UPDATE "{FLOW_RUN}" SET "state"=?1,"version"=?2,"updated_at_ms"=?3,"updated_commit_seq"=?4 WHERE "world"=?5 AND "run_id"=?6"#
            ),
            params![
                state,
                to_i64(row.version, "run version")?,
                now,
                seq,
                world,
                row.key
            ],
        )?;
    }
    Ok(())
}

const COLUMNS: &str = r#""run_id","generation","flow_id","flow_version","object_type","state","version","created_at_ms","created_commit_seq","updated_at_ms","updated_commit_seq""#;

fn decode(row: &SqlRow) -> Result<RunRow, CellError> {
    let unsigned = |index: usize, what: &'static str| {
        u64::try_from(row.get::<i64>(index)?).map_err(|_negative| CellError::OutOfRange(what))
    };
    Ok(RunRow {
        run_id: row.get(0)?,
        generation: unsigned(1, "run generation")?,
        flow: FlowRef {
            id: row.get(2)?,
            version: row.get(3)?,
        },
        object_type: row.get(4)?,
        state: row.get(5)?,
        version: unsigned(6, "run version")?,
        created_at_ms: unsigned(7, "run created time")?,
        created_commit_seq: unsigned(8, "run created commit")?,
        updated_at_ms: unsigned(9, "run updated time")?,
        updated_commit_seq: unsigned(10, "run updated commit")?,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The [`RunStart`] the commit that started `run_id` in `world` carried,
    /// read from that commit's ledger events.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn run_start(&self, world: &str, run_id: &str) -> Result<Option<RunStart>, CellError> {
        let Some(row) = self.flow_run(world, run_id)? else {
            return Ok(None);
        };
        let first = self.conn.query_optional(
            &format!(
                r#"SELECT MIN("seq") FROM "{LEDGER}" WHERE "commit_seq"=?1"#,
                LEDGER = crate::system::LEDGER
            ),
            params![to_i64(row.created_commit_seq, "commit sequence")?],
            |found| found.get::<Option<i64>>(0),
        )?;
        let Some(Some(first)) = first else {
            return Ok(None);
        };
        let after = u64::try_from(first.saturating_sub(1))
            .map_err(|_negative| CellError::OutOfRange("ledger sequence"))?;
        for entry in self.ledger_page(after, MAX_START_EVENTS)? {
            if entry.commit_seq != row.created_commit_seq {
                break;
            }
            if entry.world != world || !is_run_start(&entry.event) {
                continue;
            }
            let start: Option<RunStart> = entry
                .event
                .get("run")
                .and_then(|run| norn_ir::runtime::decode(run).ok());
            if let Some(start) = start.filter(|start| start.run_id == run_id) {
                return Ok(Some(start));
            }
        }
        Ok(None)
    }

    /// The run `run_id` in `world`, if it was started.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn flow_run(&self, world: &str, run_id: &str) -> Result<Option<RunRow>, CellError> {
        self.conn.query_optional(
            &format!(r#"SELECT {COLUMNS} FROM "{FLOW_RUN}" WHERE "world"=?1 AND "run_id"=?2"#),
            params![world, run_id],
            decode,
        )
    }

    /// RUN-LIVE-1: how many runs of `flow` in `world` are live: their run
    /// object exists and its lifecycle state is not a terminal state of its
    /// Run type's lifecycle. A Run type the model no longer declares counts
    /// as live, so a retire guard fails closed.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn live_runs(&self, world: &str, flow: &str) -> Result<u64, CellError> {
        let rows = self.conn.query(
            &format!(
                r#"SELECT "object_type","state_property","state" FROM "{FLOW_RUN}" INDEXED BY "_norn_flow_run/flow_state" WHERE "world"=?1 AND "flow_id"=?2 AND "state" IS NOT NULL"#
            ),
            params![world, flow],
        )?;
        let mut live = 0_u64;
        for values in rows {
            let row = SqlRow(values);
            let (object_type, property, state): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let terminal = self
                .model()
                .object_types()
                .find(|declared| declared.name.as_str() == object_type)
                .and_then(|declared| {
                    declared
                        .lifecycles
                        .iter()
                        .find(|lifecycle| lifecycle.property.as_str() == property)
                })
                .is_some_and(|lifecycle| lifecycle.terminal.contains(&state));
            if !terminal {
                live = live
                    .checked_add(1)
                    .ok_or(CellError::OutOfRange("live runs"))?;
            }
        }
        Ok(live)
    }

    /// One page of runs in `world`, in run-id order, filtered by flow and
    /// state. The last row's id is the next page's `after` cursor.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn flow_runs(&self, world: &str, filter: &RunFilter) -> Result<Vec<RunRow>, CellError> {
        let limit = i64::from(filter.limit.clamp(1, MAX_RUN_PAGE));
        let rows = self.conn.query(
            &format!(
                r#"SELECT {COLUMNS} FROM "{FLOW_RUN}" WHERE "world"=?1 AND (?2 IS NULL OR "flow_id"=?2) AND (?3 IS NULL OR "state"=?3) AND (?4 IS NULL OR "run_id">?4) ORDER BY "run_id" LIMIT ?5"#
            ),
            params![world, filter.flow, filter.state, filter.after, limit],
        )?;
        rows.into_iter()
            .map(|values| decode(&SqlRow(values)))
            .collect()
    }
}
