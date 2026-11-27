//! Pending outbox commands, due timers, pending observations, and run records.

use norn_ir::RuntimeValue as Value;
use norn_kernel::world::outbox::Queue;
use norn_kernel::world::outbox_ledger::{DispatchProof, OperationLedger, OutboxRecord};
use serde::{Deserialize, Serialize};

use super::{StoreReader, to_i64, to_u64};
use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::store::{Consumption, OutboxCommand, TimerEntry};
use crate::system::{INBOX, OUTBOX, OUTBOX_RECORD, PERFORM_CLAIM, RUN, TIMER};

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
    /// D03 (#1699): the queue it waits in, stored when it was first enqueued.
    /// Read from the row only: it is not part of the wire form, so a journal
    /// preimage decoded as a `PendingCommand` (which never carries it) reads
    /// as the FIFO queue and must not be used to select a queue.
    #[serde(skip, default = "fifo")]
    pub queue: Queue,
}

const fn fifo() -> Queue {
    Queue::Fifo
}

/// One page of parked commands ([`StoreReader::parked_commands_page`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedPage {
    /// The page's parked commands, in enqueue order.
    pub commands: Vec<PendingCommand>,
    /// Outbox rows the page read: what a bounded scan charges for it.
    pub read: usize,
    /// The position the next page starts after; `None` once none is left.
    pub next_after: Option<u64>,
}

/// PARKED-1, D03 (#1699): the queue a kernel command (as enqueued) waits
/// in. Enqueue stores it with the row; every later read uses the stored
/// queue, never this classification of the mutable payload.
#[must_use]
pub fn classify(command: &Value) -> Queue {
    if command.get("payload").is_some_and(norn_builtins::parks) {
        Queue::Parked
    } else {
        Queue::Fifo
    }
}

/// The SQL filter selecting rows stored in `queue`.
fn in_queue(queue: Queue) -> String {
    format!("\"queue\" = '{}'", queue.as_str())
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
    /// Bounded: one indexed read of the stored FIFO queue, so a backlog of
    /// parked commands (or an unreadable result of one) never costs or
    /// blocks ordinary queue progress.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn head_command(&self) -> Result<Option<PendingCommand>, CellError> {
        Ok(self
            .outbox_rows(
                &format!("AND {}", in_queue(Queue::Fifo)),
                params![],
                " LIMIT 1",
            )?
            .into_iter()
            .next())
    }

    /// PARKED-1: pending commands that wait for the cell to resolve them, in
    /// enqueue order. They are never performed or claimed.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn parked_commands(&self) -> Result<Vec<PendingCommand>, CellError> {
        self.outbox_rows(&format!("AND {}", in_queue(Queue::Parked)), params![], "")
    }

    /// PARKED-1, DECISION-SCAN-1 (#1702): at most `limit` (at least 1) parked
    /// commands enqueued after position `after`, read with a SQL `LIMIT`, so a
    /// bounded reader pages through a backlog without loading it whole.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn parked_commands_page(&self, after: u64, limit: usize) -> Result<ParkedPage, CellError> {
        let limit = limit.max(1);
        let rows = self.outbox_rows(
            &format!("AND \"position\" > ?1 AND {}", in_queue(Queue::Parked)),
            params![to_i64(after, "position")?],
            &format!(" LIMIT {limit}"),
        )?;
        let read = rows.len();
        let next_after = rows
            .last()
            .map(|row| row.position)
            .filter(|_| read >= limit);
        Ok(ParkedPage {
            commands: rows,
            read,
            next_after,
        })
    }

    /// DECISION-SCAN-1 (#1702): whether a parked command is enqueued after
    /// position `after`: one probe of `_norn_outbox/queue`, reading no
    /// command or result.
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn parked_command_after(&self, after: u64) -> Result<bool, CellError> {
        self.check_live()?;
        let found: Option<i64> = self.conn.query_optional(
            &format!(
                "SELECT 1 FROM \"{OUTBOX}\" WHERE \"consumed_commit_seq\" IS NULL AND \"position\" > ?1 AND {} LIMIT 1",
                in_queue(Queue::Parked)
            ),
            params![to_i64(after, "position")?],
            |row| row.get(0),
        )?;
        Ok(found.is_some())
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
        // D1: an outcome masked by an unreconciled restore fence (or a row
        // waiting on compensation) is never consumable through SQL alone.
        if self
            .outbox_ledger(command_id)?
            .is_some_and(|ledger| ledger.head_projection().outcome.is_none())
        {
            return Ok(None);
        }
        if row.queue == Queue::Parked {
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

    /// Reconstruct the active [`OperationLedger`] of `command_id` by replaying
    /// its immutable `_norn_outbox_record` log (`D1`).
    ///
    /// # Errors
    ///
    /// Store read, decode, or replay integrity errors.
    pub fn outbox_ledger(&self, command_id: &str) -> Result<Option<OperationLedger>, CellError> {
        self.check_live()?;
        crate::outbox_ledger::load_active_ledger(self.conn, command_id)
    }

    /// Every immutable [`OutboxRecord`] persisted for `command_id` across all
    /// epochs, in append order (`D1`).
    ///
    /// # Errors
    ///
    /// Store read or decode errors.
    pub fn outbox_records(&self, command_id: &str) -> Result<Vec<OutboxRecord>, CellError> {
        self.check_live()?;
        crate::outbox_ledger::load_all_records(self.conn, command_id)
    }

    /// The immutable [`DispatchProof`] for `(command_id, generation)`, if that
    /// generation was durably claimed (`D1`).
    ///
    /// # Errors
    ///
    /// Store read, decode, or replay integrity errors.
    pub fn dispatch_proof(
        &self,
        command_id: &str,
        generation: u64,
    ) -> Result<Option<DispatchProof>, CellError> {
        self.check_live()?;
        crate::outbox_ledger::load_dispatch_proof(self.conn, command_id, generation)
    }

    /// Prior-generation immutable outcome records attached in the active epoch
    /// before `active_generation`, for export preimage capture (`D1`).
    pub(crate) fn outbox_late_outcomes(
        &self,
        command_id: &str,
        active_generation: u64,
    ) -> Result<Vec<OutboxRecord>, CellError> {
        self.check_live()?;
        crate::outbox_ledger::prior_outcome_records(self.conn, command_id, active_generation)
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
                "SELECT \"position\", \"commit_seq\", \"command_id\", \"generation\", \"idempotency_key\", \"deadline\", \"command\", \"result\", \"queue\" FROM \"{OUTBOX}\" WHERE {base} {filter} ORDER BY \"position\"{limit}"
            ),
            bindings,
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let deadline: Option<i64> = row.get(5)?;
            let command: String = row.get(6)?;
            let result: Option<String> = row.get(7)?;
            let queue: String = row.get(8)?;
            let queue = Queue::parse(&queue).ok_or_else(|| CellError::CorruptValue {
                table: OUTBOX.to_owned(),
                column: "queue".to_owned(),
            })?;
            out.push(PendingCommand {
                queue,
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

    /// D1: sorted `(command_id, prior_generation_outcomes)` across every
    /// operation whose latest epoch retains at least one attached outcome on a
    /// superseded generation.
    ///
    /// # Errors
    ///
    /// SQL engine or ledger replay errors.
    pub fn outbox_prior_outcomes(&self) -> Result<Vec<(String, Vec<u64>)>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT DISTINCT \"command_id\" FROM \"{OUTBOX_RECORD}\" ORDER BY \"command_id\""
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let command_id: String = SqlRow(values).get(0)?;
            if let Some(ledger) = self.outbox_ledger(&command_id)? {
                let prior: Vec<u64> = ledger
                    .prior_generations()
                    .iter()
                    .filter_map(|(gen_id, rec)| rec.outcome.is_some().then_some(*gen_id))
                    .collect();
                if !prior.is_empty() {
                    out.push((command_id, prior));
                }
            }
        }
        Ok(out)
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
