//! TIMER-RETRY-1: when the host may next retry a due item it could not
//! handle.
//!
//! A host runs the cell's due work from its clock (a quantity reservation's
//! expiry, a flow timer, a decision's deadline). An item that is due but
//! cannot be handled (a refused turn, a retired flow) stays due. Its retry
//! eligibility is kept here, durably, per item and incarnation: the next
//! wake, a request that re-arms the host's alarm, a cold open and the dev
//! host's loop all read the same record, so a stuck item is retried on its
//! backoff and never in a tight loop, and never hides the items behind it.
//!
//! The record is host bookkeeping, not cell history: it is written outside
//! the ledger, and a new incarnation of the item (a timer scheduled again, a
//! decision's next generation) starts afresh, because the record names the
//! incarnation it was made for.

use std::collections::BTreeMap;

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{PendingTimer, StoreReader, to_i64, to_u64};
use crate::store::{CellStore, TimerEntry};

pub(crate) const DUE_RETRY: &str = "_norn_due_retry";

/// The retry table's DDL, applied with the other system tables.
pub(crate) const DDL: [&str; 1] = [r#"CREATE TABLE IF NOT EXISTS "_norn_due_retry" (
  "item" TEXT NOT NULL,
  "incarnation" INTEGER NOT NULL,
  "attempts" INTEGER NOT NULL,
  "eligible_at" INTEGER NOT NULL,
  PRIMARY KEY ("item")
)"#];

/// The retry key of a timer row (quantity or ordinary).
#[must_use]
pub fn timer_item(timer_id: &str) -> String {
    format!("timer/{timer_id}")
}

/// The retry key of a decision.
#[must_use]
pub fn decision_item(decision_id: &str) -> String {
    format!("decision/{decision_id}")
}

/// The retry key of an approved action call (APPROVAL-RECOVERY-1).
#[must_use]
pub fn approval_item(decision_id: &str) -> String {
    format!("approval/{decision_id}")
}

/// One item's retry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueRetry {
    /// The item's incarnation the record was made for (a timer's scheduling
    /// commit, a decision's generation).
    pub incarnation: u64,
    /// Failed attempts at this incarnation.
    pub attempts: u32,
    /// The earliest logical time the item may be tried again.
    pub eligible_at: u64,
}

/// The backoff after `attempts` failures: `base_ms` doubled per earlier
/// failure, at most `cap_ms`.
#[must_use]
pub fn backoff(attempts: u32, base_ms: u64, cap_ms: u64) -> u64 {
    let doublings = attempts.saturating_sub(1).min(32);
    base_ms
        .saturating_mul(1_u64.checked_shl(doublings).unwrap_or(u64::MAX))
        .min(cap_ms)
}

const TIMER_JOIN: &str = r#"LEFT JOIN "_norn_due_retry" AS r ON r."item"=('timer/'||t."timer_id") AND r."incarnation"=t."commit_seq""#;
const QUANTITY: &str =
    r#"t."timer_id">='norn.quantity.expiry/' AND t."timer_id"<'norn.quantity.expiry0'"#;
const ORDINARY: &str =
    r#"(t."timer_id"<'norn.quantity.expiry/' OR t."timer_id">='norn.quantity.expiry0')"#;

impl<E: SqlEngine> StoreReader<'_, E> {
    /// TIMER-RETRY-1: up to `limit` timers due at `now` and eligible to be
    /// tried (no retry record for their incarnation, or one whose backoff has
    /// passed), earliest first, ties by id: host-owned quantity expiries when
    /// `quantity`, else ordinary (flow) timers.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn eligible_due_timers(
        &self,
        now: u64,
        limit: usize,
        quantity: bool,
    ) -> Result<Vec<PendingTimer>, CellError> {
        let (index, kind) = if quantity {
            ("_norn_timer/quantity_due", QUANTITY)
        } else {
            ("_norn_timer/ordinary_due", ORDINARY)
        };
        let rows = self.conn.query(
            &format!(
                r#"SELECT t."commit_seq",t."timer_id",t."due",t."timer" FROM "_norn_timer" AS t INDEXED BY "{index}" {TIMER_JOIN} WHERE {kind} AND t."due"<=?1 AND (r."item" IS NULL OR r."eligible_at"<=?1) ORDER BY t."due",t."timer_id" LIMIT ?2"#
            ),
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

    /// TIMER-RETRY-1: when the earliest timer may next be tried: its due
    /// time, or, for one that failed at its current incarnation, the later of
    /// its due time and its retry eligibility. `None` when no timer is
    /// pending.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn next_eligible_timer(&self) -> Result<Option<u64>, CellError> {
        // The earliest timer with no retry record, through the due index: it
        // passes over only the timers that failed.
        let fresh = self.conn.query_optional(
            &format!(
                r#"SELECT t."due" FROM "_norn_timer" AS t INDEXED BY "_norn_timer/due" {TIMER_JOIN} WHERE r."item" IS NULL ORDER BY t."due",t."timer_id" LIMIT 1"#
            ),
            params![],
            |row| to_u64(row.get(0)?, "timer due"),
        )?;
        // Every timer that failed at its current incarnation: as many as the
        // retry records.
        let retried = self.conn.query_optional(
            &format!(
                r#"SELECT MIN(MAX(t."due",r."eligible_at")) FROM "{DUE_RETRY}" AS r JOIN "_norn_timer" AS t ON r."item"=('timer/'||t."timer_id") AND r."incarnation"=t."commit_seq""#
            ),
            params![],
            |row| row.get::<Option<i64>>(0),
        )?;
        let retried = match retried.flatten() {
            Some(value) => Some(to_u64(value, "timer retry")?),
            None => None,
        };
        Ok(match (fresh, retried) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, None) => left,
            (None, right) => right,
        })
    }

    /// TIMER-RETRY-1: the retry records whose item key starts with `prefix`
    /// (for example every decision's), by item.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn due_retries(&self, prefix: &str) -> Result<BTreeMap<String, DueRetry>, CellError> {
        let rows = self.conn.query(
            &format!(
                r#"SELECT "item","incarnation","attempts","eligible_at" FROM "{DUE_RETRY}" WHERE substr("item",1,length(?1))=?1 ORDER BY "item""#
            ),
            params![prefix],
        )?;
        let mut out = BTreeMap::new();
        for values in rows {
            let row = SqlRow(values);
            let item: String = row.get(0)?;
            out.insert(
                item,
                DueRetry {
                    incarnation: to_u64(row.get(1)?, "retry incarnation")?,
                    attempts: u32::try_from(row.get::<i64>(2)?).unwrap_or(u32::MAX),
                    eligible_at: to_u64(row.get(3)?, "retry eligibility")?,
                },
            );
        }
        Ok(out)
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// TIMER-RETRY-1: record that `item` at `incarnation` was due at `now`
    /// and could not be handled. It may be tried again after the backoff of
    /// its failures at this incarnation ([`backoff`] over `base_ms` and
    /// `cap_ms`); a record of an earlier incarnation starts afresh.
    ///
    /// # Errors
    /// SQL engine or decode errors: the failure is then not recorded.
    pub fn record_due_failure(
        &self,
        item: &str,
        incarnation: u64,
        now: u64,
        (base_ms, cap_ms): (u64, u64),
    ) -> Result<DueRetry, CellError> {
        self.conn.transaction(|| -> Result<DueRetry, CellError> {
            let previous = self.conn.query_optional(
                &format!(
                    r#"SELECT "incarnation","attempts" FROM "{DUE_RETRY}" WHERE "item"=?1"#
                ),
                params![item],
                |row| Ok((row.get::<i64>(0)?, row.get::<i64>(1)?)),
            )?;
            let incarnation_sql = to_i64(incarnation, "retry incarnation")?;
            let attempts = match previous {
                Some((seen, attempts)) if seen == incarnation_sql => {
                    u32::try_from(attempts).unwrap_or(u32::MAX).saturating_add(1)
                }
                _ => 1,
            };
            let retry = DueRetry {
                incarnation,
                attempts,
                eligible_at: now.saturating_add(backoff(attempts, base_ms, cap_ms)),
            };
            self.conn.execute(
                &format!(
                    r#"INSERT INTO "{DUE_RETRY}" ("item","incarnation","attempts","eligible_at") VALUES (?1,?2,?3,?4) ON CONFLICT("item") DO UPDATE SET "incarnation"=excluded."incarnation","attempts"=excluded."attempts","eligible_at"=excluded."eligible_at""#
                ),
                params![
                    item,
                    incarnation_sql,
                    i64::from(retry.attempts),
                    to_i64(retry.eligible_at, "retry eligibility")?
                ],
            )?;
            Ok(retry)
        })
    }

    /// TIMER-RETRY-1: forget `item`'s retry record (it was handled).
    ///
    /// # Errors
    /// SQL engine errors.
    pub fn clear_due_retry(&self, item: &str) -> Result<(), CellError> {
        self.conn.transaction(|| -> Result<(), CellError> {
            self.conn.execute(
                &format!(r#"DELETE FROM "{DUE_RETRY}" WHERE "item"=?1"#),
                params![item],
            )?;
            Ok(())
        })
    }

    /// TIMER-RETRY-1: forget the timer retry records whose timer
    /// incarnation is gone (consumed by any path), so the records stay as
    /// few as the items that are still stuck.
    ///
    /// # Errors
    /// SQL engine errors.
    pub fn prune_timer_retries(&self) -> Result<(), CellError> {
        self.conn.transaction(|| -> Result<(), CellError> {
            self.conn.execute(
                &format!(
                    r#"DELETE FROM "{DUE_RETRY}" WHERE substr("item",1,6)='timer/' AND NOT EXISTS (SELECT 1 FROM "_norn_timer" AS t WHERE ('timer/'||t."timer_id")="{DUE_RETRY}"."item" AND t."commit_seq"="{DUE_RETRY}"."incarnation")"#
                ),
                params![],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::backoff;

    #[test]
    fn backoff_doubles_from_the_base_and_stops_at_the_cap() {
        assert_eq!(backoff(1, 60_000, 3_600_000), 60_000);
        assert_eq!(backoff(2, 60_000, 3_600_000), 120_000);
        assert_eq!(backoff(6, 60_000, 3_600_000), 1_920_000);
        assert_eq!(backoff(7, 60_000, 3_600_000), 3_600_000);
        assert_eq!(backoff(u32::MAX, 60_000, 3_600_000), 3_600_000);
    }
}
