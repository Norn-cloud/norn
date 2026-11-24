//! MANIFEST-LAWS-1 (S1, S5, S6, S7) on the adapter: residue pages and the
//! settle contract. A page holds at most [`RESIDUE_PAGE_ROWS`] rows and
//! [`RESIDUE_PAGE_BYTES`] of serialized JSON, brackets and separators
//! included; its bound is enforced from each row's value bytes (a lower bound
//! on its serialized size) before any value is materialized, and a landing
//! whose residue needs more than [`MAX_RESIDUE_PAGES`] pages is refused
//! before its first answer.

use std::collections::{BTreeMap, BTreeSet};

use super::super::{
    EngineRefusal, MAX_RESIDUE_PAGES, RESIDUE_EXCEEDED, RESIDUE_PAGE_BYTES, RESIDUE_PAGE_ROWS,
    ResidueRow, RuleCheck, SettleRequest, TableSchema,
};
use super::laws::{self, TypedRow};
use super::{Outcome, engine_error, refusal};

/// A landing's staged state between its steps.
#[derive(Debug, Clone)]
pub(super) struct Staged {
    /// The landed table.
    pub table: TableSchema,
    /// Every typed row, in row order.
    pub rows: Vec<TypedRow>,
    /// Rows that could not be typed, and every finding so far.
    pub held: Vec<laws::HeldRow>,
    /// Each residue row's undecided rules.
    pub residue: BTreeMap<u64, BTreeSet<String>>,
    /// The load's counts.
    pub load: super::super::LoadResponse,
}

impl Staged {
    fn row(&self, number: u64) -> Option<&TypedRow> {
        self.rows
            .binary_search_by_key(&number, |row| row.row)
            .ok()
            .and_then(|at| self.rows.get(at))
    }
}

/// [`RESIDUE_EXCEEDED`] for residue row `row`, larger than a page alone.
fn too_wide(row: u64) -> EngineRefusal {
    refusal(
        RESIDUE_EXCEEDED,
        format!("residue row {row} is larger than a residue page ({RESIDUE_PAGE_BYTES} bytes)"),
    )
}

/// A typed row's value bytes: its wide (text-like) values' bytes.
fn value_bytes(table: &TableSchema, row: &TypedRow) -> usize {
    table
        .columns
        .iter()
        .zip(&row.cells)
        .filter(|(column, _)| laws::is_wide(column.kind))
        .map(|(_, cell)| cell.as_str().map_or(0, str::len))
        .fold(0_usize, usize::saturating_add)
}

/// S6: the residue page after row `after`, at most `limit` rows (clamped to
/// `1..=RESIDUE_PAGE_ROWS`), ending before the row that would take the
/// page's serialized rows past [`RESIDUE_PAGE_BYTES`].
pub(super) fn page(staged: &Staged, after: u64, limit: u32) -> Outcome<Vec<ResidueRow>> {
    let take = usize::try_from(limit.clamp(1, RESIDUE_PAGE_ROWS)).unwrap_or(1);
    let mut candidates = Vec::new();
    for (row, rules) in staged
        .residue
        .range(after.saturating_add(1)..)
        .take(take)
    {
        let typed = staged
            .row(*row)
            .ok_or_else(|| engine_error("a residue row without its typed row"))?;
        candidates.push((typed, rules, value_bytes(&staged.table, typed)));
    }
    // S7: one bracket, then each row and the separator after it.
    let mut lower = 1_usize;
    let mut fits = 0_usize;
    for (_, _, bytes) in &candidates {
        lower = lower.saturating_add(bytes.saturating_add(1));
        if lower > RESIDUE_PAGE_BYTES {
            break;
        }
        fits = fits.saturating_add(1);
    }
    let mut out = Vec::new();
    let mut bytes = 1_usize;
    for (typed, rules, _) in candidates.iter().take(fits) {
        let values = staged
            .table
            .columns
            .iter()
            .zip(&typed.cells)
            .filter_map(|(column, cell)| {
                cell.present(column.kind).map(|value| (column.id.clone(), value))
            })
            .collect();
        let residue = ResidueRow {
            row: typed.row,
            values,
            rules: rules.iter().cloned().collect(),
        };
        let size = serde_json::to_string(&residue)
            .map_err(|e| engine_error(e.to_string()))?
            .len()
            .saturating_add(1);
        if size.saturating_add(1) > RESIDUE_PAGE_BYTES {
            return Err(too_wide(residue.row));
        }
        bytes = bytes.saturating_add(size);
        if bytes > RESIDUE_PAGE_BYTES {
            return Ok(out);
        }
        out.push(residue);
    }
    match candidates.get(fits) {
        Some((typed, _, value_bytes)) if value_bytes.saturating_add(2) > RESIDUE_PAGE_BYTES => {
            Err(too_wide(typed.row))
        }
        _ => Ok(out),
    }
}

/// S1: refuse a landing whose residue takes more than [`MAX_RESIDUE_PAGES`]
/// pages of `limit` rows, counted as the pages are answered.
pub(super) fn fits(staged: &Staged, limit: u32) -> Outcome<()> {
    let mut after = 0;
    for _ in 0..MAX_RESIDUE_PAGES {
        match page(staged, after, limit)?.last() {
            Some(last) => after = last.row,
            None => return Ok(()),
        }
    }
    if page(staged, after, 1)?.is_empty() {
        return Ok(());
    }
    Err(refusal(
        RESIDUE_EXCEEDED,
        format!(
            "the landing leaves more residue than {MAX_RESIDUE_PAGES} pages of {} rows",
            limit.clamp(1, RESIDUE_PAGE_ROWS)
        ),
    ))
}

/// MANIFEST-LAWS-1 / LAW-04: record the per-row findings of the residue rows
/// up to `through` and remove those rows from the residue. A finding must
/// name a rule the engine left to that row, once, with the label and hold
/// the landing pinned for that rule.
pub(super) fn settle(staged: &mut Staged, checks: &[RuleCheck], request: &SettleRequest) -> Outcome<()> {
    let by_rule: BTreeMap<&str, &RuleCheck> =
        checks.iter().map(|check| (check.rule.as_str(), check)).collect();
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for finding in &request.findings {
        let Some(check) = by_rule
            .get(finding.rule.as_str())
            .filter(|check| check.label == finding.label && check.holds == finding.holds)
        else {
            return Err(refusal(
                "bulk.invalid_finding",
                "the finding does not match the landed rule's label and hold",
            ));
        };
        let left = finding.row <= request.through
            && staged
                .residue
                .get(&finding.row)
                .is_some_and(|rules| rules.contains(&finding.rule));
        if !left || !seen.insert((finding.row, finding.rule.clone())) {
            return Err(refusal(
                "bulk.invalid_finding",
                format!(
                    "row {} is not residue of that rule up to {}",
                    finding.row, request.through
                ),
            ));
        }
        let row = staged
            .row(finding.row)
            .ok_or_else(|| engine_error("a residue row without its typed row"))?;
        findings.push(laws::finding(check, row));
    }
    staged.held.extend(findings);
    staged.residue = staged.residue.split_off(&request.through.saturating_add(1));
    Ok(())
}
