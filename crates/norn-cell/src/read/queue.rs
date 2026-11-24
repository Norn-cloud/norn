//! Pending outbox commands, due timers, pending observations, and run records.

use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use super::{StoreReader, to_i64, to_u64};
use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::store::{Consumption, OutboxCommand, TimerEntry};
use crate::system::{INBOX, OUTBOX, PERFORM_CLAIM, RUN, TIMER};

/// A pending outbox row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCommand {
    /// Position in enqueue order.
    pub position: u64,
    /// The commit that last enqueued it.
    pub commit_seq: u64,
    /// The command as enqueued.
    pub command: OutboxCommand,
    /// The recorded `perform` result, once performed.
    pub result: Option<Value>,
}

/// PARKED-1: the SQL form of [`is_parked`] over the stored `command` JSON,
/// so queue selection skips parked rows without loading them. A string
/// comparison is false for a non-string or missing member, exactly as
/// [`norn_builtins::parks`] reads it; [`is_parked`] still re-checks each row.
const PARKED_SQL: &str = "(json_extract(\"command\", '$.payload.kind') IS 'perform' AND json_extract(\"command\", '$.payload.function') IN ('norn.decision', 'norn.action') AND json_extract(\"command\", '$.payload.functionVersion') IS '1')";

/// PARKED-1: whether `command` waits outside the FIFO perform queue.
#[must_use]
pub fn is_parked(command: &OutboxCommand) -> bool {
    command
        .command
        .get("payload")
        .is_some_and(norn_builtins::parks)
}

/// A pending timer row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTimer {
    /// The commit that scheduled it: its incarnation.
    pub commit_seq: u64,
    /// The timer.
    pub timer: TimerEntry,
}

/// One `perform` execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// The command id.
    pub command_id: String,
    /// The generation.
    pub generation: u64,
    /// 1-based attempt number within the generation.
    pub attempt: u64,
    /// The outcome.
    pub outcome: Value,
    /// The commit that consumed the result, if any.
    pub consumed_commit_seq: Option<u64>,
}

/// A pending inbox row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingObservation {
    /// Position in ingestion order.
    pub position: u64,
    /// The observation id.
    pub observation_id: String,
    /// The observation as ingested.
    pub observed: Value,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Pending outbox commands, in enqueue order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_commands(&self) -> Result<Vec<PendingCommand>, CellError> {
        self.outbox_rows("", params![], "")
    }

    /// The oldest pending command that does not park (the next to
    /// `perform`), if any. Parked commands never block it (PARKED-1).
    ///
    /// Bounded: SQL skips parked rows and reads one row, so a backlog of
    /// parked commands (or an unreadable result of one) never costs or
    /// blocks ordinary queue progress.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn head_command(&self) -> Result<Option<PendingCommand>, CellError> {
        let mut after = 0_i64;
        loop {
            let filter = format!("AND \"position\" > ?1 AND NOT {PARKED_SQL}");
            let Some(row) = self
                .outbox_rows(&filter, params![after], " LIMIT 1")?
                .into_iter()
                .next()
            else {
                return Ok(None);
            };
            // [`is_parked`] stays the authority; `PARKED_SQL` only narrows.
            if !is_parked(&row.command) {
                return Ok(Some(row));
            }
            after = to_i64(row.position, "position")?;
        }
    }

    /// PARKED-1: pending commands that wait for the cell to resolve them, in
    /// enqueue order. They are never performed or claimed.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn parked_commands(&self) -> Result<Vec<PendingCommand>, CellError> {
        Ok(self
            .outbox_rows(&format!("AND {PARKED_SQL}"), params![], "")?
            .into_iter()
            .filter(|row| is_parked(&row.command))
            .collect())
    }

    /// The pending result delivery of `command_id` at `generation`: the FIFO
    /// head, or a parked command, once its result is recorded and until a
    /// commit consumes it. Reads and decodes only that command's row (and the
    /// head's, for a non-parked command).
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_result(
        &self,
        command_id: &str,
        generation: u64,
    ) -> Result<Option<PendingCommand>, CellError> {
        let Some(row) = self
            .pending_command(command_id)?
            .filter(|row| row.command.generation == generation && row.result.is_some())
        else {
            return Ok(None);
        };
        if is_parked(&row.command) {
            return Ok(Some(row));
        }
        let head = self.head_command()?.map(|head| head.position);
        Ok((head == Some(row.position)).then_some(row))
    }

    /// The unconsumed outbox row of `command_id`, if any, read and decoded
    /// alone (command ids are unique in the outbox).
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_command(&self, command_id: &str) -> Result<Option<PendingCommand>, CellError> {
        Ok(self
            .outbox_rows("AND \"command_id\" = ?1", params![command_id], " LIMIT 1")?
            .into_iter()
            .next())
    }

    /// The outbox command `command_id`, consumed or not: what a parked
    /// command carried after the commit that consumed its result
    /// (APPROVAL-RECOVERY-1 reads an approved call from it).
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn outbox_command(&self, command_id: &str) -> Result<Option<OutboxCommand>, CellError> {
        self.check_live()?;
        self.conn.query_optional(
            &format!(
                "SELECT \"command_id\", \"generation\", \"idempotency_key\", \"deadline\", \"command\" FROM \"{OUTBOX}\" WHERE \"command_id\" = ?1"
            ),
            params![command_id],
            |row| {
                let deadline: Option<i64> = row.get(3)?;
                let command: String = row.get(4)?;
                Ok(OutboxCommand {
                    command_id: row.get(0)?,
                    generation: to_u64(row.get(1)?, "generation")?,
                    idempotency_key: row.get(2)?,
                    deadline: deadline
                        .map(|value| to_u64(value, "deadline"))
                        .transpose()?,
                    command: serde_json::from_str(&command)?,
                })
            },
        )
    }

    /// The unconsumed outbox row at `position`, if any, read and decoded alone.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub(crate) fn pending_at(&self, position: u64) -> Result<Option<PendingCommand>, CellError> {
        Ok(self
            .outbox_rows(
                "AND \"position\" = ?1",
                params![to_i64(position, "position")?],
                " LIMIT 1",
            )?
            .into_iter()
            .next())
    }

    /// Whether `command_id` and `generation` have a durable perform claim.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn perform_claimed(&self, command_id: &str, generation: u64) -> Result<bool, CellError> {
        self.check_live()?;
        let found: Option<i64> = self
            .conn
            .query_optional(
                &format!(
                    "SELECT 1 FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
                ),
                params![command_id, to_i64(generation, "generation")?],
                |row| row.get::<i64>(0),
            )?;
        Ok(found.is_some())
    }

    /// The outbox entry `command_id`, consumed or not, with the commit that
    /// enqueued it: a parked call's decision read back after it resolved
    /// (APPROVAL-RECOVERY-1).
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn outbox_entry(&self, command_id: &str) -> Result<Option<PendingCommand>, CellError> {
        Ok(self
            .outbox_select("\"command_id\" = ?1", "", params![command_id], " LIMIT 1")?
            .into_iter()
            .next())
    }

    /// APPROVAL-REQUESTED-1: the parked calls whose requester is exactly
    /// `requester` (its subject, actor kind and role table), as their
    /// `(command_id, generation)`, in enqueue order. Only those two columns
    /// are read back, so a caller names its own calls before any parked
    /// payload is loaded or decoded, and another requester's call costs it
    /// nothing more than an id that exists nowhere. With `waiting`, only the
    /// calls that still wait: undecided (no result recorded), or approved
    /// and neither run nor refused, in `world`.
    ///
    /// SQL narrows on the stored command, as [`Self::parked_commands`] does;
    /// the caller re-checks every row it loads.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn parked_calls_of(
        &self,
        world: &str,
        requester: (&str, &str, &str),
        waiting: bool,
    ) -> Result<Vec<(String, u64)>, CellError> {
        self.check_live()?;
        let (subject, actor_kind, role_table) = requester;
        let approved = crate::decision_ledger::APPROVED_CALL;
        let select = format!(
            r#"SELECT o."command_id", o."generation" FROM "{OUTBOX}" o WHERE substr(o."command_id",1,14)='norn.approval/' AND json_extract(o."command",'$.payload.approval.requester.subject') IS ?1 AND json_extract(o."command",'$.payload.approval.requester.actorKind') IS ?2 AND json_extract(o."command",'$.payload.approval.requester.roleTable') IS ?3"#
        );
        let rows = if waiting {
            self.conn.query(
                &format!(
                    r#"{select} AND ((o."result" IS NULL AND o."consumed_commit_seq" IS NULL) OR EXISTS (SELECT 1 FROM "{approved}" a WHERE a."world"=?4 AND a."command_id"=o."command_id" AND a."consumed_seq" IS NULL AND a."refused_code" IS NULL)) ORDER BY o."position""#
                ),
                params![subject, actor_kind, role_table, world],
            )?
        } else {
            self.conn.query(
                &format!(r#"{select} ORDER BY o."position""#),
                params![subject, actor_kind, role_table],
            )?
        };
        rows.into_iter()
            .map(|values| {
                let row = SqlRow(values);
                Ok((row.get(0)?, to_u64(row.get(1)?, "generation")?))
            })
            .collect()
    }

    fn outbox_rows(
        &self,
        filter: &str,
        bindings: &[SqlValue],
        limit: &str,
    ) -> Result<Vec<PendingCommand>, CellError> {
        self.outbox_select("\"consumed_commit_seq\" IS NULL", filter, bindings, limit)
    }

    fn outbox_select(
        &self,
        base: &str,
        filter: &str,
        bindings: &[SqlValue],
        limit: &str,
    ) -> Result<Vec<PendingCommand>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"position\", \"commit_seq\", \"command_id\", \"generation\", \"idempotency_key\", \"deadline\", \"command\", \"result\" FROM \"{OUTBOX}\" WHERE {base} {filter} ORDER BY \"position\"{limit}"
            ),
            bindings,
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let deadline: Option<i64> = row.get(5)?;
            let command: String = row.get(6)?;
            let result: Option<String> = row.get(7)?;
            out.push(PendingCommand {
                position: to_u64(row.get(0)?, "position")?,
                commit_seq: to_u64(row.get(1)?, "commit_seq")?,
                command: OutboxCommand {
                    command_id: row.get(2)?,
                    generation: to_u64(row.get(3)?, "generation")?,
                    idempotency_key: row.get(4)?,
                    deadline: deadline
                        .map(|value| to_u64(value, "deadline"))
                        .transpose()?,
                    command: serde_json::from_str(&command)?,
                },
                result: result
                    .map(|result| {
                        crate::payload_storage::decode_result(
                            self.conn,
                            &result,
                            Some(&crate::payload_storage::receipt_world(
                                self.conn,
                                to_u64(row.get(1)?, "commit_seq")?,
                            )?),
                        )
                    })
                    .transpose()?,
            });
        }
        Ok(out)
    }

    /// Enumerate all pending timers, earliest due first (ties by id).
    /// Runtime delivery and alarm rearming use bounded selectors instead.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_timers(&self) -> Result<Vec<PendingTimer>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"commit_seq\", \"timer_id\", \"due\", \"timer\" FROM \"{TIMER}\" ORDER BY \"due\", \"timer_id\""
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let timer: String = row.get(3)?;
            out.push(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "commit_seq")?,
                timer: TimerEntry {
                    timer_id: row.get(1)?,
                    due: to_u64(row.get(2)?, "due")?,
                    timer: serde_json::from_str(&timer)?,
                },
            });
        }
        Ok(out)
    }

    /// One pending timer by its unique primary key, without enumerating others.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn pending_timer(&self, timer_id: &str) -> Result<Option<PendingTimer>, CellError> {
        self.check_live()?;
        self.conn.query_optional(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" WHERE "timer_id"=?1 LIMIT 1"#,
            params![timer_id],
            |row| Ok(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry { timer_id: row.get(1)?, due: to_u64(row.get(2)?, "timer due")?, timer: serde_json::from_str(&row.get::<String>(3)?)? },
            }),
        )
    }

    /// Whether a known timer is pending, using its unique primary key.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn has_pending_timer(&self, timer_id: &str) -> Result<bool, CellError> {
        self.check_live()?;
        Ok(self
            .conn
            .query_optional(
                r#"SELECT 1 FROM "_norn_timer" WHERE "timer_id"=?1 LIMIT 1"#,
                params![timer_id],
                |row| row.get::<i64>(0),
            )?
            .is_some())
    }

    /// Earliest deadline across ordinary and quantity timers. This covering
    /// index lookup reads at most one row and never decodes a timer payload.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn next_timer_due(&self) -> Result<Option<u64>, CellError> {
        self.check_live()?;
        self.conn.query_optional(
            r#"SELECT "due" FROM "_norn_timer" INDEXED BY "_norn_timer/due" ORDER BY "due","timer_id" LIMIT 1"#,
            params![],
            |row| to_u64(row.get(0)?, "timer due"),
        )
    }

    /// TIMER-ALARM-1: the earliest deadline strictly after `after`, across
    /// ordinary and quantity timers, through the same covering index. A host
    /// re-arms its alarm with it once the work due at `after` was run, so a
    /// timer it could not deliver never re-arms the alarm in a tight loop.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn next_timer_due_after(&self, after: u64) -> Result<Option<u64>, CellError> {
        self.check_live()?;
        self.conn.query_optional(
            r#"SELECT "due" FROM "_norn_timer" INDEXED BY "_norn_timer/due" WHERE "due">?1 ORDER BY "due","timer_id" LIMIT 1"#,
            params![to_i64(after, "timer time")?],
            |row| to_u64(row.get(0)?, "timer due"),
        )
    }

    /// The earliest ordinary timer due at `now` (ties by id), if any.
    /// The partial index excludes host-owned quantity timers before seeking,
    /// so even an overdue quantity backlog cannot amplify this read.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn due_timer(&self, now: u64) -> Result<Option<PendingTimer>, CellError> {
        self.check_live()?;
        self.conn.query_optional(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" INDEXED BY "_norn_timer/ordinary_due" WHERE ("timer_id"<'norn.quantity.expiry/' OR "timer_id">='norn.quantity.expiry0') AND "due"<=?1 ORDER BY "due","timer_id" LIMIT 1"#,
            params![to_i64(now, "timer time")?],
            |row| Ok(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry { timer_id: row.get(1)?, due: to_u64(row.get(2)?, "timer due")?, timer: serde_json::from_str(&row.get::<String>(3)?)? },
            }),
        )
    }

    /// TIMER-ALARM-1: up to `limit` ordinary timers due at `now`, earliest
    /// first (ties by id), through the same partial index as
    /// [`Self::due_timer`]: a host delivers each on its own, so one it cannot
    /// deliver never hides the ones behind it.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn due_timers(&self, now: u64, limit: usize) -> Result<Vec<PendingTimer>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" INDEXED BY "_norn_timer/ordinary_due" WHERE ("timer_id"<'norn.quantity.expiry/' OR "timer_id">='norn.quantity.expiry0') AND "due"<=?1 ORDER BY "due","timer_id" LIMIT ?2"#,
            params![
                to_i64(now, "timer time")?,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for values in rows {
            let row = SqlRow(values);
            let timer: String = row.get(3)?;
            out.push(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry {
                    timer_id: row.get(1)?,
                    due: to_u64(row.get(2)?, "timer due")?,
                    timer: serde_json::from_str(&timer)?,
                },
            });
        }
        Ok(out)
    }

    /// Earliest due host-owned quantity deadline, using the timer due index.
    ///
    /// # Errors
    /// Storage or decoding errors.
    pub fn due_quantity_timer(&self, now: u64) -> Result<Option<PendingTimer>, CellError> {
        self.check_live()?;
        self.conn.query_optional(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" INDEXED BY "_norn_timer/quantity_due" WHERE "timer_id" >= 'norn.quantity.expiry/' AND "timer_id" < 'norn.quantity.expiry0' AND "due"<=?1 ORDER BY "due","timer_id" LIMIT 1"#,
            params![to_i64(now, "quantity expiry time")?], |row| Ok(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry { timer_id: row.get(1)?, due: to_u64(row.get(2)?, "timer due")?, timer: serde_json::from_str(&row.get::<String>(3)?)? },
            }),
        )
    }

    /// Pending (unconsumed) observations, in ingestion order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_observations(&self) -> Result<Vec<PendingObservation>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"position\", \"observation_id\", \"observed\" FROM \"{INBOX}\" WHERE \"consumed_commit_seq\" IS NULL ORDER BY \"position\""
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let observed: String = row.get(2)?;
            out.push(PendingObservation {
                position: to_u64(row.get(0)?, "position")?,
                observation_id: row.get(1)?,
                observed: serde_json::from_str(&observed)?,
            });
        }
        Ok(out)
    }

    /// One pending (unconsumed) observation by its id, through the inbox's
    /// unique id index; `None` when it is absent or already consumed.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_observation(
        &self,
        observation_id: &str,
    ) -> Result<Option<PendingObservation>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"position\", \"observation_id\", \"observed\" FROM \"{INBOX}\" WHERE \"observation_id\" = ?1 AND \"consumed_commit_seq\" IS NULL"
            ),
            params![observation_id],
        )?;
        let Some(values) = rows.into_iter().next() else {
            return Ok(None);
        };
        let row = SqlRow(values);
        let observed: String = row.get(2)?;
        Ok(Some(PendingObservation {
            position: to_u64(row.get(0)?, "position")?,
            observation_id: row.get(1)?,
            observed: serde_json::from_str(&observed)?,
        }))
    }

    /// The runs of `command_id`, in order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn runs(&self, command_id: &str) -> Result<Vec<RunRecord>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"generation\", \"attempt\", \"outcome\", \"consumed_commit_seq\" FROM \"{RUN}\" WHERE \"command_id\" = ?1 ORDER BY \"generation\", \"attempt\""
            ),
            params![command_id],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let outcome: String = row.get(2)?;
            let consumed: Option<i64> = row.get(3)?;
            out.push(RunRecord {
                command_id: command_id.to_owned(),
                generation: to_u64(row.get(0)?, "generation")?,
                attempt: to_u64(row.get(1)?, "attempt")?,
                outcome: crate::payload_storage::decode_result(
                    self.conn,
                    &outcome,
                    crate::payload_storage::command_world(
                        self.conn,
                        command_id,
                        to_u64(row.get(0)?, "generation")?,
                    )?
                    .as_deref(),
                )?,
                consumed_commit_seq: consumed
                    .map(|value| to_u64(value, "commit_seq"))
                    .transpose()?,
            });
        }
        Ok(out)
    }

    /// How many times any command was executed (`perform` runs).
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn executions(&self) -> Result<u64, CellError> {
        self.check_live()?;
        let count: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM \"{RUN}\""),
            params![],
            |row| row.get(0),
        )?;
        to_u64(count, "executions")
    }

    /// Whether `consumption` names a pending delivery: a timer with that id
    /// and incarnation; the outbox head with that id and generation and a
    /// recorded result; an unconsumed observation with that id.
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn is_pending(&self, consumption: &Consumption) -> Result<bool, CellError> {
        self.check_live()?;
        match consumption {
            Consumption::Timer {
                timer_id,
                incarnation,
            } => {
                let found: Option<i64> = self.conn.query_optional(
                    &format!(
                        "SELECT 1 FROM \"{TIMER}\" WHERE \"timer_id\" = ?1 AND \"commit_seq\" = ?2 LIMIT 1"
                    ),
                    params![timer_id, to_i64(*incarnation, "incarnation")?],
                    |row| row.get(0),
                )?;
                Ok(found.is_some())
            }
            Consumption::Result {
                command_id,
                generation,
            } => Ok(self.pending_result(command_id, *generation)?.is_some()),
            Consumption::Observation { observation_id } => {
                let found: Option<i64> = self.conn.query_optional(
                    &format!(
                        "SELECT 1 FROM \"{INBOX}\" WHERE \"observation_id\" = ?1 AND \"consumed_commit_seq\" IS NULL"
                    ),
                    params![observation_id],
                    |row| row.get(0),
                )?;
                Ok(found.is_some())
            }
        }
    }
}
