//! Domain-agnostic read-only multi-cell combined management view (`READ-LOCAL-1`).
//!
//! Projects multiple independent [`CellStore`] instances side-by-side at pinned
//! local cuts without commingling ledgers, currencies, or write authority.
//! Each cell snapshot verifies a single stable ledger head, definition pin,
//! source cursor/watermark frontier, and pending-observation identity set across
//! all evaluated questions, and fails closed (`core.compute.stale_input`) if
//! any boundary shifts during the read.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_ir::cell_id::CellId;
use norn_ir::data_quality::DataQualityFindingStatus;
use serde::{Deserialize, Serialize};

use crate::compute::{Freshness, Frontier, QueryContext, QueryResult, Watermark};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::store::CellStore;

/// Classification of a multi-cell read-only projection (`READ-LOCAL-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombinedViewKind {
    /// Side-by-side read-only management view across independent legal-entity
    /// cells. Preserves each cell's native book and currency and is explicitly
    /// not a consolidated financial statement.
    CombinedManagementView,
}

/// Read-only specification for evaluating one cell's slice of a combined view.
pub struct CellViewInput<'a, E: SqlEngine> {
    /// Cell store to inspect at a single verified read cut.
    pub store: &'a CellStore<E>,
    /// Query context pinning world, scope, mapping pin, clock, cursors, and watermarks.
    pub context: QueryContext,
    /// Required feed names whose cursors and watermarks must be complete and fresh.
    pub required_feeds: &'a [&'a str],
    /// Minimum acceptable watermark heartbeat timestamp (milliseconds since epoch).
    pub min_fresh_ms: u64,
    /// Evidence object types scanned for open data-quality findings.
    pub evidence_types: &'a [&'a str],
    /// Named admitted questions to evaluate at this cell's pinned cut.
    pub questions: &'a [(&'a str, &'a QuestionPlan)],
}

/// Pinned local frontier, coverage, and exact question answers for one cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellCutSnapshot {
    /// Durable cell identity bound to this store.
    pub cell_id: CellId,
    /// Scoped authority boundary of this cell.
    pub scope: String,
    /// World name evaluated (for example `"main"`).
    pub world: String,
    /// Verified commit count at the read cut.
    pub revision: u64,
    /// Last ledger event sequence number at the read cut.
    pub ledger_seq: u64,
    /// Verified ledger head hash at the read cut.
    pub ledger_hash: String,
    /// Active installed definitions version at the read cut.
    pub definition_version: u64,
    /// Active installed definitions admission receipt digest (or model hash).
    pub definition_pin: String,
    /// Source cursor frontier pinned to every answer in this cut.
    pub source_cursors: Frontier,
    /// Partition watermarks pinned to every answer in this cut.
    pub watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
    /// Required feeds that are absent, incomplete, or older than `min_fresh_ms`.
    pub missing_feeds: BTreeSet<String>,
    /// Open data-quality findings across the declared evidence object types.
    pub open_findings: usize,
    /// Unconsumed inbox observations quarantined at mapping time.
    pub pending_observations: usize,
    /// True only when all answers are complete and fresh with zero open feed issues.
    pub complete: bool,
    /// Exact question answers keyed by declared question name.
    pub answers: BTreeMap<String, QueryResult>,
}

/// Read-only combined projection across multiple independent cells (`READ-LOCAL-1`).
///
/// Carries no cell handles or write authority; every cell retains its own local
/// frontier, definition pin, and coverage diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombinedViewReport {
    /// Explicit view classification.
    pub view_kind: CombinedViewKind,
    /// Always `false` for [`CombinedViewKind::CombinedManagementView`].
    pub is_consolidated_financial_statement: bool,
    /// Always `true`: combined views never grant or execute writes.
    pub read_only: bool,
    /// Independent per-cell snapshots in caller-specified order.
    pub cells: Vec<CellCutSnapshot>,
}

fn pending_identities<E: SqlEngine>(store: &CellStore<E>) -> Result<Vec<(u64, String)>, CellError> {
    Ok(store
        .read()
        .pending_observations()?
        .into_iter()
        .map(|row| (row.position, row.observation_id))
        .collect())
}

fn check_missing_feeds(
    required_feeds: &[&str],
    cursors: &Frontier,
    watermarks: &BTreeMap<String, BTreeMap<String, Watermark>>,
    min_fresh_ms: u64,
) -> BTreeSet<String> {
    required_feeds
        .iter()
        .filter(|feed| {
            let Some(parts) = cursors.get(**feed) else {
                return true;
            };
            if parts.is_empty() {
                return true;
            }
            let Some(wm_parts) = watermarks.get(**feed) else {
                return true;
            };
            parts.iter().any(|(part, consumed)| {
                wm_parts.get(part).is_none_or(|wm| {
                    !wm.complete
                        || wm.observed != *consumed
                        || wm.heartbeat_ms.is_none_or(|hb| hb < min_fresh_ms)
                })
            })
        })
        .map(|feed| (*feed).to_owned())
        .collect()
}

fn count_open_findings<E: SqlEngine>(
    store: &CellStore<E>,
    world: &str,
    evidence_types: &[&str],
) -> Result<usize, CellError> {
    let reader = store.read();
    let mut open = 0_usize;
    for ty in evidence_types {
        let mut after: Option<String> = None;
        loop {
            let page = reader.ids_after(world, ty, after.as_deref(), 1_000)?;
            if page.is_empty() {
                break;
            }
            for id in &page {
                let found = reader.observation_findings(world, ty, id)?;
                open = open.saturating_add(
                    found
                        .into_iter()
                        .filter(|f| matches!(f.status, DataQualityFindingStatus::Open))
                        .count(),
                );
            }
            if page.len() < 1_000 {
                break;
            }
            after = page.last().cloned();
        }
    }
    Ok(open)
}

/// Evaluate one cell's pinned read cut using the supplied question evaluator
/// (either the cell's exact evaluator or a synced OLAP engine).
///
/// # Errors
/// Returns `CellError::ComputeRefused("core.compute.stale_input")` if the
/// ledger head, definition pin, query context, or pending-observation identity
/// set shifts during evaluation, or propagates underlying store/evaluator errors.
pub fn evaluate_cell_cut<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    evaluate: &mut impl FnMut(&str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CellCutSnapshot, CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    let cell_id = input.store.cell_id()?.ok_or_else(stale)?;
    let head_before = input.store.verified_head()?;
    let (definition_version, definition_pin) =
        match input.store.read().active_definitions_identity()? {
            Some(pair) => pair,
            None => (0, input.store.model().model_hash().to_owned()),
        };
    let pending_before = pending_identities(input.store)?;

    let mut answers = BTreeMap::new();
    for (name, plan) in input.questions {
        let (expected_query_pin, expected_def_pin) = input.store.question_pins(plan, None)?;
        if expected_def_pin != definition_pin {
            return Err(stale());
        }
        let result = evaluate(name, plan, input.context.clone())?;
        if result.query_pin != expected_query_pin
            || result.revision != head_before
            || result.known_as_of != head_before.commits
            || result.definition_pin != definition_pin
            || result.context != input.context
        {
            return Err(stale());
        }
        answers.insert((*name).to_owned(), result);
    }

    let open_findings =
        count_open_findings(input.store, &input.context.world, input.evidence_types)?;
    let head_after = input.store.verified_head()?;
    let pending_after = pending_identities(input.store)?;
    if head_after != head_before || pending_after != pending_before {
        return Err(stale());
    }

    let missing_feeds = check_missing_feeds(
        input.required_feeds,
        &input.context.source_cursors,
        &input.context.watermarks,
        input.min_fresh_ms,
    );
    let all_fresh = answers
        .values()
        .all(|q| q.complete && matches!(q.freshness, Freshness::Exact | Freshness::Fresh));
    let pending_observations = pending_after.len();
    let complete =
        all_fresh && missing_feeds.is_empty() && open_findings == 0 && pending_observations == 0;

    Ok(CellCutSnapshot {
        cell_id,
        scope: input.context.scope.clone(),
        world: input.context.world.clone(),
        revision: head_before.commits,
        ledger_seq: head_before.seq,
        ledger_hash: head_before.hash,
        definition_version,
        definition_pin,
        source_cursors: input.context.source_cursors.clone(),
        watermarks: input.context.watermarks.clone(),
        missing_feeds,
        open_findings,
        pending_observations,
        complete,
        answers,
    })
}

/// Evaluate a read-only combined management view across independent cells
/// (`READ-LOCAL-1`), refusing duplicate cell identities and unstable cuts.
///
/// # Errors
/// Returns `CellError::ComputeRefused("core.compute.duplicate_cell")` if two
/// inputs resolve to the same [`CellId`], or propagates any error from
/// [`evaluate_cell_cut`].
pub fn evaluate_combined_management_view<E: SqlEngine>(
    inputs: &[CellViewInput<'_, E>],
    mut evaluate: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, CellError> {
    let mut seen_cells = BTreeSet::new();
    let mut cells = Vec::with_capacity(inputs.len());
    for (idx, input) in inputs.iter().enumerate() {
        let snap = evaluate_cell_cut(input, &mut |name, plan, ctx| evaluate(idx, name, plan, ctx))?;
        if !seen_cells.insert(snap.cell_id.clone()) {
            return Err(CellError::ComputeRefused("core.compute.duplicate_cell"));
        }
        cells.push(snap);
    }
    Ok(CombinedViewReport {
        view_kind: CombinedViewKind::CombinedManagementView,
        is_consolidated_financial_statement: false,
        read_only: true,
        cells,
    })
}
