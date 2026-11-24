//! Findings / quarantine ledger pagination.

use std::collections::{BTreeMap, BTreeSet};

use super::{LedgerEntry, StoreReader, to_i64, to_u64};
use crate::engine::{MAX_SAFE_INTEGER, SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::payload_storage::EVENT_BYTES;
use crate::reactive::DecodeBudget;
use crate::system::LEDGER;

/// A bounded ledger scan with only finding producer payloads decoded.
/// Every ledger row consumes the scan budget, including unrelated payloads.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingLedgerPage {
    /// Finding events and their same-commit observation metadata.
    pub entries: Vec<LedgerEntry>,
    /// All examined rows, including same-commit lookup work and repeated rows.
    pub scanned_rows: usize,
    /// Last outer-page position, or the supplied boundary at end of history.
    pub scanned_through: u64,
    /// Commits whose event positions were examined in this scan.
    pub scanned_commits: BTreeSet<u64>,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Scan ledger positions while decoding only finding producer candidates
    /// and their same-commit observation metadata. Unrelated payloads consume
    /// the row budget without consuming the candidate payload byte budget.
    /// Lengths and all scanned rows are admitted before any body is decoded.
    /// Candidates still require verified history access before disclosure.
    ///
    /// # Errors
    /// SQL, sequence-range, durable evidence or event decode errors.
    #[allow(
        clippy::too_many_lines,
        reason = "Keep the row/byte admission and same-commit evidence policy together"
    )]
    pub fn finding_ledger_page_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Option<FindingLedgerPage>, CellError> {
        // S8 (#1551 round 4): a scan that selects findings by position
        // relies on every covered entry being present.
        self.check_ledger(after)?;
        let mut page = FindingLedgerPage {
            entries: Vec::new(),
            scanned_rows: 0,
            scanned_through: after,
            scanned_commits: BTreeSet::new(),
        };
        if limit == 0 || after >= u64::try_from(MAX_SAFE_INTEGER).unwrap_or(0) {
            return Ok(Some(page));
        }
        // Check end-of-history even with no rows left: exactly exhausting the
        // admitted scan is valid, but another real ledger row must refuse.
        let lengths = self.conn.query(
            &format!("SELECT \"seq\", \"commit_seq\", json_extract(\"event\", '$.op') IN ('data_quality_finding', 'observed_out_of_band'), json_extract(\"event\", '$.type') = 'observation.recorded', {EVENT_BYTES} + octet_length(\"world\") + octet_length(\"prev_hash\") + octet_length(\"hash\") + 64 FROM \"{LEDGER}\" WHERE \"seq\" > ?1 ORDER BY \"seq\" LIMIT ?2"),
            params![to_i64(after, "seq")?, i64::try_from(limit.min(budget.remaining_rows().saturating_add(1))).unwrap_or(MAX_SAFE_INTEGER)],
        )?;
        if lengths.len() > budget.remaining_rows() {
            return Ok(None);
        }
        // Reserve the outer page, then charge every same-commit lookup row to
        // the same request budget. The extra row is a refusal sentinel, never
        // an invitation to parse another commit or all future ledger history.
        let mut work_rows = lengths.len();
        let mut bytes = 0_u64;
        let mut candidates = Vec::new();
        let mut finding_commits = BTreeMap::new();
        for values in lengths {
            let row = SqlRow(values);
            let sequence = to_u64(row.get::<i64>(0)?, "seq")?;
            let commit = to_u64(row.get::<i64>(1)?, "commit_seq")?;
            page.scanned_through = sequence;
            page.scanned_commits.insert(commit);
            let finding = row.get::<Option<i64>>(2)? == Some(1);
            let metadata = row.get::<Option<i64>>(3)? == Some(1);
            let associated = if metadata {
                if let Some(found) = finding_commits.get(&commit) {
                    *found
                } else {
                    let remaining = budget.remaining_rows().saturating_sub(work_rows);
                    let lookup = self.conn.query(
                        &format!("SELECT CASE WHEN json_extract(\"event\", '$.op') IN ('data_quality_finding', 'observed_out_of_band') THEN {EVENT_BYTES} + octet_length(\"world\") + octet_length(\"prev_hash\") + octet_length(\"hash\") + 64 ELSE NULL END FROM \"{LEDGER}\" WHERE \"commit_seq\" = ?1 ORDER BY \"seq\" LIMIT ?2"),
                        params![to_i64(commit, "commit_seq")?, i64::try_from(remaining.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER)],
                    )?;
                    if lookup.len() > remaining {
                        return Ok(None);
                    }
                    work_rows = work_rows.saturating_add(lookup.len());
                    let mut found = false;
                    for values in lookup {
                        if let Some(length) = SqlRow(values).get::<Option<i64>>(0)? {
                            found = true;
                            // Actual finding evidence is subject to the byte
                            // policy even when it lies beyond this outer page.
                            // Unrelated raw observation bodies stay excluded.
                            bytes = bytes.saturating_add(to_u64(length, "finding lookup length")?);
                        }
                    }
                    finding_commits.insert(commit, found);
                    found
                }
            } else {
                false
            };
            if finding || associated {
                bytes =
                    bytes.saturating_add(to_u64(row.get::<i64>(4)?, "ledger candidate length")?);
                candidates.push(sequence);
            }
        }
        page.scanned_rows = work_rows;
        if !budget.charge(bytes) {
            return Ok(None);
        }
        budget.rows = budget.rows.saturating_sub(page.scanned_rows);
        for sequence in candidates {
            page.entries
                .extend(self.ledger_page(sequence.saturating_sub(1), 1)?);
        }
        Ok(Some(page))
    }
}
