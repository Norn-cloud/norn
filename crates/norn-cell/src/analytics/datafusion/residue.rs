//! MANIFEST-LAWS-1 (S1, S5, S6, S7) on the adapter: residue pages and the
//! settle contract. A page holds at most [`RESIDUE_PAGE_ROWS`] rows and
//! [`RESIDUE_PAGE_BYTES`] of serialized JSON, brackets and separators
//! included; its bound is enforced from each row's value bytes (a lower bound
//! on its serialized size, recorded when the row was staged) before any value
//! is read back from disk, and a landing whose residue needs more than
//! [`MAX_RESIDUE_PAGES`] pages is refused before its first answer.

use std::collections::{BTreeMap, BTreeSet};

use super::super::{
    EngineRefusal, MAX_RESIDUE_PAGES, RESIDUE_EXCEEDED, RESIDUE_PAGE_BYTES, RESIDUE_PAGE_ROWS,
    ResidueRow, RuleCheck, SettleRequest,
};
use super::columnar::Sink;
use super::laws;
use super::staging::{self, Staged};
use super::{Engine, Outcome, Tenant, engine_error, refusal};

/// [`RESIDUE_EXCEEDED`] for residue row `row`, larger than a page alone.
fn too_wide(row: u64) -> EngineRefusal {
    refusal(
        RESIDUE_EXCEEDED,
        format!("residue row {row} is larger than a residue page ({RESIDUE_PAGE_BYTES} bytes)"),
    )
}

/// S6: the residue page after row `after`, at most `limit` rows (clamped to
/// `1..=RESIDUE_PAGE_ROWS`), ending before the row that would take the
/// page's serialized rows past [`RESIDUE_PAGE_BYTES`].
pub(super) fn page(
    engine: &Engine,
    tenant: &Tenant,
    staged: &Staged,
    after: u64,
    limit: u32,
) -> Outcome<Vec<ResidueRow>> {
    let take = usize::try_from(limit.clamp(1, RESIDUE_PAGE_ROWS)).unwrap_or(1);
    let candidates: Vec<_> = staged
        .residue
        .range(after.saturating_add(1)..)
        .take(take)
        .collect();
    // S7: one bracket, then each row and the separator after it.
    let mut lower = 1_usize;
    let mut fits = 0_usize;
    for (_, residue) in &candidates {
        lower = lower.saturating_add(residue.bytes.saturating_add(1));
        if lower > RESIDUE_PAGE_BYTES {
            break;
        }
        fits = fits.saturating_add(1);
    }
    let wanted: Vec<u64> = candidates.iter().take(fits).map(|(row, _)| **row).collect();
    let typed = staging::typed_rows(engine, tenant, staged, &wanted)?;
    let mut out = Vec::new();
    let mut bytes = 1_usize;
    for (row, residue) in candidates.iter().take(fits) {
        let typed = typed
            .get(row)
            .ok_or_else(|| engine_error("a residue row without its typed row"))?;
        let values = staged
            .table
            .columns
            .iter()
            .zip(&typed.cells)
            .filter_map(|(column, cell)| {
                cell.present(column.kind)
                    .map(|value| (column.id.clone(), value))
            })
            .collect();
        let page_row = ResidueRow {
            row: **row,
            values,
            rules: residue.rules.iter().cloned().collect(),
        };
        let size = serde_json::to_string(&page_row)
            .map_err(|e| engine_error(e.to_string()))?
            .len()
            .saturating_add(1);
        if size.saturating_add(1) > RESIDUE_PAGE_BYTES {
            return Err(too_wide(page_row.row));
        }
        bytes = bytes.saturating_add(size);
        if bytes > RESIDUE_PAGE_BYTES {
            return Ok(out);
        }
        out.push(page_row);
    }
    match candidates.get(fits) {
        Some((row, residue)) if residue.bytes.saturating_add(2) > RESIDUE_PAGE_BYTES => {
            Err(too_wide(**row))
        }
        _ => Ok(out),
    }
}

/// S1: refuse a landing whose residue takes more than [`MAX_RESIDUE_PAGES`]
/// pages of `limit` rows, counted as the pages are answered.
pub(super) fn fits(engine: &Engine, tenant: &Tenant, staged: &Staged, limit: u32) -> Outcome<()> {
    let mut after = 0;
    for _ in 0..MAX_RESIDUE_PAGES {
        match page(engine, tenant, staged, after, limit)?.last() {
            Some(last) => after = last.row,
            None => return Ok(()),
        }
    }
    if page(engine, tenant, staged, after, 1)?.is_empty() {
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
/// up to `through` (into a findings file of this step) and remove those rows
/// from the residue. A finding must name a rule the engine left to that row,
/// once, with the label and hold the landing pinned for that rule.
pub(super) fn settle(
    engine: &Engine,
    tenant: &Tenant,
    staged: &mut Staged,
    checks: &[RuleCheck],
    request: &SettleRequest,
) -> Outcome<()> {
    let by_rule: BTreeMap<&str, &RuleCheck> = checks
        .iter()
        .map(|check| (check.rule.as_str(), check))
        .collect();
    let mut seen = BTreeSet::new();
    let mut decided = Vec::new();
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
                .is_some_and(|residue| residue.rules.contains(&finding.rule));
        if !left || !seen.insert((finding.row, finding.rule.clone())) {
            return Err(refusal(
                "bulk.invalid_finding",
                format!(
                    "row {} is not residue of that rule up to {}",
                    finding.row, request.through
                ),
            ));
        }
        decided.push((finding.row, *check));
    }
    let rows: Vec<u64> = decided.iter().map(|(row, _)| *row).collect();
    let typed = staging::typed_rows(engine, tenant, staged, &rows)?;
    let mut findings = decided
        .iter()
        .map(|(row, check)| {
            typed
                .get(row)
                .map(|typed| laws::finding(check, typed))
                .ok_or_else(|| engine_error("a residue row without its typed row"))
        })
        .collect::<Outcome<Vec<_>>>()?;
    let dir = staged
        .typed
        .parent()
        .ok_or_else(|| engine_error("a staged landing's directory"))?;
    super::merge::sort_held(&mut findings);
    let part = dir.join(format!("findings-{}.parquet", staged.findings.len()));
    let mut sink = Sink::staged(&part, &staging::held_schema(&staged.table))?;
    staging::write_held(&mut sink, &staged.table, &findings)?;
    sink.finish()?;
    staged.findings.push(part);
    staged.record(&findings);
    staged.residue = staged.residue.split_off(&request.through.saturating_add(1));
    Ok(())
}
