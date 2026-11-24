//! Lossless continuation storage and read inspection for
//! `AUTOMATION-OUTBOX-1` (`Core 04`).
//!
//! When a commit's trigger matching exceeds the inline candidate bound
//! ([`super::MAX_CANDIDATES_PER_COMMIT`]) or per-commit rule-scan budget
//! ([`super::MAX_RULE_SCANS_PER_COMMIT`]), the commit still succeeds without
//! refusing the business write. Instead, it persists one idempotent
//! [`AutomationOutboxContinuation`] row in
//! `_norn_automation_outbox_continuation` inside the same `SQLite`
//! transaction.
//!
//! ## U2 drain obligation
//! Resume under the recorded matcher version and cursor. Reconstruct the match
//! from receipt writes, retained action calls, and historical versions at
//! `trigger_commit_seq - 1` and `trigger_commit_seq`, including tombstones.
//! An absent action target can have a nonzero version. During a #1551 scrub,
//! gated as-of reads must retry without advancing the cursor. Retain the
//! required history until expansion completes.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use crate::engine::{MAX_SAFE_INTEGER, SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::read::{StoreReader, to_i64, to_u64};

use super::{AUTOMATION_OUTBOX, AutomationTriggerCandidate, AutomationTriggerKey};

/// The transactional continuation table for commits that exceed the inline
/// automation-matching bound.
const AUTOMATION_OUTBOX_CONTINUATION: &str = "_norn_automation_outbox_continuation";

/// Version of the automation matching semantics used to create a continuation.
/// First persisted in storage version 7. Bump only when matching semantics change.
pub const AUTOMATION_MATCHER_VERSION: u32 = 1;

/// Deterministic resume point in `(written objects × rules)` persisted on an
/// `_norn_automation_outbox_continuation` row when a commit crosses the inline
/// matching bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AutomationContinuationCursor {
    /// Resume Pass 1 (row-write triggers: `event:created`, `event:updated`,
    /// `lifecycle_entered`) starting at `(root_type, object_id)` in
    /// `_norn_fact_values` order (`ORDER BY root_type, id`), at
    /// `ancestor_type` within `model.chain(concrete_type)`, strictly after
    /// `after_automation_id` (`None` resumes from the first rule on
    /// `ancestor_type`), then continue through remaining ancestors, remaining
    /// written objects in Pass 1, and all of Pass 2 (`event:action`).
    Row {
        /// Root type of the written object where matching paused.
        root_type: String,
        /// Object id where matching paused.
        object_id: String,
        /// Watched type in `model.chain(concrete_type)` where matching paused.
        ancestor_type: String,
        /// Exclusive keyset cursor on `automation_id` within `ancestor_type`
        /// (`None` means resume from the first rule on `ancestor_type`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_automation_id: Option<String>,
    },
    /// Resume Pass 2 (`event:action` triggers) starting at the committed
    /// `norn.ActionCall` record `action_call_key` in `_norn_fact_values` order
    /// (`root_type = 'norn.ActionCall' ORDER BY id`), strictly after
    /// `after_automation_id` (`None` resumes from the first rule on
    /// `action_id`), then continue through remaining action calls of the
    /// commit.
    Action {
        /// `id` of the committed `norn.ActionCall` record where matching paused.
        action_call_key: String,
        /// `actionId` of that call record.
        action_id: String,
        /// Exclusive keyset cursor on `automation_id` for `action_id`
        /// (`None` means resume from the first rule on `action_id`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_automation_id: Option<String>,
    },
}

/// Durable continuation record persisted in the same transaction as a
/// triggering commit whose automation matching crossed the inline bound.
///
/// Idempotent on `trigger_commit_seq`. U2 expands the remaining candidates
/// from committed history at `trigger_commit_seq` (before = state at
/// `trigger_commit_seq - 1`, after = state at `trigger_commit_seq`) under the
/// pinned storage, engine, definitions, and matcher versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationOutboxContinuation {
    /// Commit sequence of the triggering write (primary key).
    pub trigger_commit_seq: u64,
    /// Execution world of the triggering commit.
    pub world: String,
    /// Resume position in `(written objects × rules)`.
    pub cursor: AutomationContinuationCursor,
    /// Cell storage version ([`crate::engine_release::STORAGE_VERSION`]) at
    /// the triggering commit.
    pub storage_version: u32,
    /// Engine version string (`<crate-version>/<release>`) at the triggering
    /// commit.
    pub engine_version: String,
    /// Active cell definitions version (`receipt.body.definitions_version`) at
    /// the triggering commit.
    pub definitions_version: u64,
    /// Matching semantics that U2 must use for expansion.
    pub matcher_version: u32,
    /// Logical time (ms) of the triggering commit.
    pub recorded_at_ms: u64,
    /// Commit sequence that finished expanding this continuation, or `None`
    /// while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_commit_seq: Option<u64>,
}

/// Producer release label recorded on continuation rows.
#[must_use]
pub fn current_engine_version() -> String {
    format!(
        "{}/{}",
        env!("CARGO_PKG_VERSION"),
        crate::engine_release::WRITER
    )
}

pub(super) fn insert_continuation<E: SqlEngine>(
    conn: &E,
    record: &AutomationOutboxContinuation,
) -> Result<(), CellError> {
    let cursor_json = serde_json::to_string(&record.cursor).map_err(CellError::Json)?;
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO \"{AUTOMATION_OUTBOX_CONTINUATION}\" (\
              \"trigger_commit_seq\", \"world\", \"cursor\", \"storage_version\", \
              \"engine_version\", \"definitions_version\", \"matcher_version\", \
              \"recorded_at_ms\", \"completed_commit_seq\"\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        ),
        params![
            to_i64(record.trigger_commit_seq, "continuation trigger commit seq")?,
            record.world.as_str(),
            cursor_json,
            i64::from(record.storage_version),
            record.engine_version.as_str(),
            to_i64(
                record.definitions_version,
                "continuation definitions version"
            )?,
            i64::from(record.matcher_version),
            to_i64(record.recorded_at_ms, "continuation recorded_at_ms")?,
            record
                .completed_commit_seq
                .map(|seq| to_i64(seq, "continuation completed commit seq"))
                .transpose()?,
        ],
    )
}

pub(super) fn limit_to_sql(limit: usize) -> i64 {
    i64::try_from(limit)
        .unwrap_or(MAX_SAFE_INTEGER)
        .min(MAX_SAFE_INTEGER)
}

const SELECT_COLUMNS: &str = "\"world\", \"automation_id\", \"revision\", \"trigger_commit_seq\", \"object_type\", \"object_id\", \"object_version\", \"trigger_kind\", \"action_id\", \"before_values\", \"after_values\", \"recorded_at_ms\", \"consumed_commit_seq\", \"voided_commit_seq\"";

const CONTINUATION_SELECT_COLUMNS: &str = "\"trigger_commit_seq\", \"world\", \"cursor\", \"storage_version\", \"engine_version\", \"definitions_version\", \"matcher_version\", \"recorded_at_ms\", \"completed_commit_seq\"";

fn decode_snapshot(text: Option<&str>) -> Result<Option<BTreeMap<String, Value>>, CellError> {
    text.map(|raw| {
        match Value::from_json_text(raw)
            .map_err(|e| CellError::Encoding(format!("automation outbox snapshot: {e}")))?
        {
            Value::Object(map) => Ok(map),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => Err(CellError::Encoding(
                "automation outbox snapshot must be a JSON object".to_owned(),
            )),
        }
    })
    .transpose()
}

fn decode_candidate(values: Vec<SqlValue>) -> Result<AutomationTriggerCandidate, CellError> {
    let row = SqlRow(values);
    let opt_seq = |idx: usize, label: &'static str| -> Result<Option<u64>, CellError> {
        row.get::<Option<i64>>(idx)?
            .map(|seq| to_u64(seq, label))
            .transpose()
    };
    Ok(AutomationTriggerCandidate {
        world: row.get(0)?,
        key: AutomationTriggerKey {
            automation_id: row.get(1)?,
            revision: to_u64(row.get(2)?, "automation revision")?,
            trigger_commit_seq: to_u64(row.get(3)?, "trigger commit seq")?,
            object_type: row.get(4)?,
            object_id: row.get(5)?,
            object_version: to_u64(row.get(6)?, "object version")?,
        },
        trigger_kind: row.get(7)?,
        action_id: row.get(8)?,
        before_values: decode_snapshot(row.get::<Option<String>>(9)?.as_deref())?,
        after_values: decode_snapshot(row.get::<Option<String>>(10)?.as_deref())?,
        recorded_at_ms: to_u64(row.get(11)?, "trigger recorded_at_ms")?,
        consumed_commit_seq: opt_seq(12, "consumed commit seq")?,
        voided_commit_seq: opt_seq(13, "voided commit seq")?,
    })
}

fn decode_continuation(values: Vec<SqlValue>) -> Result<AutomationOutboxContinuation, CellError> {
    let row = SqlRow(values);
    let cursor_raw: String = row.get(2)?;
    let cursor: AutomationContinuationCursor =
        serde_json::from_str(&cursor_raw).map_err(CellError::Json)?;
    let storage_version = u32::try_from(row.get::<i64>(3)?)
        .map_err(|_overflow| CellError::OutOfRange("continuation storage version"))?;
    let completed_commit_seq = row
        .get::<Option<i64>>(8)?
        .map(|seq| to_u64(seq, "continuation completed commit seq"))
        .transpose()?;
    Ok(AutomationOutboxContinuation {
        trigger_commit_seq: to_u64(row.get(0)?, "continuation trigger commit seq")?,
        world: row.get(1)?,
        cursor,
        storage_version,
        engine_version: row.get(4)?,
        definitions_version: to_u64(row.get(5)?, "continuation definitions version")?,
        matcher_version: u32::try_from(row.get::<i64>(6)?)
            .map_err(|_overflow| CellError::OutOfRange("automation matcher version"))?,
        recorded_at_ms: to_u64(row.get(7)?, "continuation recorded_at_ms")?,
        completed_commit_seq,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Pending (unconsumed, unvoided) automation trigger candidates in `world`,
    /// ordered deterministically by `(trigger_commit_seq, automation_id,
    /// revision, object_type, object_id, object_version)`, up to `limit`.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn pending_automation_triggers(
        &self,
        world: &str,
        limit: usize,
    ) -> Result<Vec<AutomationTriggerCandidate>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL \
                 ORDER BY \"trigger_commit_seq\", \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\" \
                 LIMIT ?2"
            ),
            params![world, limit_to_sql(limit)],
        )?;
        rows.into_iter().map(decode_candidate).collect()
    }

    /// All automation trigger candidates recorded by commit `commit_seq` in
    /// `world` (including consumed and voided rows for audit inspection).
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn automation_triggers_for_commit(
        &self,
        world: &str,
        commit_seq: u64,
    ) -> Result<Vec<AutomationTriggerCandidate>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"trigger_commit_seq\" = ?2 \
                 ORDER BY \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\""
            ),
            params![world, to_i64(commit_seq, "trigger commit seq")?],
        )?;
        rows.into_iter().map(decode_candidate).collect()
    }

    /// Total number of pending (unconsumed, unvoided) automation trigger
    /// candidates in `world`.
    ///
    /// # Errors
    /// SQL engine or count range errors.
    pub fn pending_automation_trigger_count(&self, world: &str) -> Result<u64, CellError> {
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL"
            ),
            params![world],
            |row| row.get(0),
        )?;
        to_u64(count, "automation trigger count")
    }

    /// The outbox continuation row for `trigger_commit_seq`, if matching
    /// crossed the inline bound for that commit.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn automation_outbox_continuation(
        &self,
        trigger_commit_seq: u64,
    ) -> Result<Option<AutomationOutboxContinuation>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {CONTINUATION_SELECT_COLUMNS} FROM \"{AUTOMATION_OUTBOX_CONTINUATION}\" \
                 WHERE \"trigger_commit_seq\" = ?1"
            ),
            params![to_i64(
                trigger_commit_seq,
                "continuation trigger commit seq"
            )?],
        )?;
        rows.into_iter().next().map(decode_continuation).transpose()
    }

    /// Pending (uncompleted) automation outbox continuation rows in `world`,
    /// ordered deterministically by `trigger_commit_seq`, up to `limit`.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn pending_automation_continuations(
        &self,
        world: &str,
        limit: usize,
    ) -> Result<Vec<AutomationOutboxContinuation>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {CONTINUATION_SELECT_COLUMNS} FROM \"{AUTOMATION_OUTBOX_CONTINUATION}\" \
                 WHERE \"world\" = ?1 AND \"completed_commit_seq\" IS NULL \
                 ORDER BY \"trigger_commit_seq\" LIMIT ?2"
            ),
            params![world, limit_to_sql(limit)],
        )?;
        rows.into_iter().map(decode_continuation).collect()
    }

    /// Total number of pending (uncompleted) automation outbox continuation
    /// rows in `world`.
    ///
    /// # Errors
    /// SQL engine or count range errors.
    pub fn pending_automation_continuation_count(&self, world: &str) -> Result<u64, CellError> {
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM \"{AUTOMATION_OUTBOX_CONTINUATION}\" \
                 WHERE \"world\" = ?1 AND \"completed_commit_seq\" IS NULL"
            ),
            params![world],
            |row| row.get(0),
        )?;
        to_u64(count, "automation continuation count")
    }
}
