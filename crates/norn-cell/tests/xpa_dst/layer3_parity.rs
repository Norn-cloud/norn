//! Layer 3 — Differential Parity (Exact Cell Evaluator vs `DuckDB` / `DataFusion` OLAP).
//!
//! Feature-gated (`#[cfg(feature = "duckdb")]` and `#[cfg(feature = "datafusion")]`)
//! differential parity suites evaluating all declared retail questions, pinned
//! multi-cell combined-view reports (`evaluate_combined_management_view_from_spec`),
//! and declared retail transforms across identical pinned cuts.
//!
//! Asserts byte-for-byte parity on `query_pin`, `revision`, `definition_pin`,
//! `authority_pin`, `read_token`, bucket values, `lineage_ref`, `row.members`,
//! and transform `rows` / `lineage_pin` / `node_rows`.

use norn_cell::combined_view::CombinedViewReport;
use norn_cell::compute::QueryResult;
use norn_cell::compute::transform::TransformResult;

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_cell::CellError;
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_cell::compute::transform::RowAccess;

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::cuts::{dst_coverage_spec, evaluate_multi_cell_report};
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::env::{EntityCell, RETAIL_QUESTIONS, RETAIL_TRANSFORMS};
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::layer2_simulator::{SimConfig, SimulatedWorld, run_seeded_multi_cell_sim};

/// Assert strict byte-for-byte parity between Exact Cell and OLAP `QueryResult`
/// envelopes (normalizing only `engine`).
///
/// # Errors
/// Returns an error if any pin, revision, lineage handle, exclusion, or row field differs.
pub fn assert_query_result_parity(
    label: &str,
    exact: &QueryResult,
    olap: &QueryResult,
) -> Result<(), String> {
    if olap.query_pin != exact.query_pin
        || olap.definition_pin != exact.definition_pin
        || olap.revision != exact.revision
        || olap.known_as_of != exact.known_as_of
        || olap.grain != exact.grain
        || olap.lineage_ref != exact.lineage_ref
        || olap.complete != exact.complete
        || olap.freshness != exact.freshness
        || olap.exclusions != exact.exclusions
        || olap.unlinked != exact.unlinked
        || olap.rows.len() != exact.rows.len()
    {
        return Err(format!("{label}: query envelope metadata mismatch"));
    }
    for (key, exact_row) in &exact.rows {
        let olap_row = olap
            .rows
            .get(key)
            .ok_or_else(|| format!("{label}: missing OLAP bucket {key}"))?;
        if olap_row.bucket != exact_row.bucket
            || olap_row.totals != exact_row.totals
            || olap_row.value != exact_row.value
            || olap_row.members != exact_row.members
            || olap_row.suppressed != exact_row.suppressed
            || olap_row.null_counts != exact_row.null_counts
        {
            return Err(format!(
                "{label}/{key}: row mismatch:\n  exact={exact_row:?}\n  olap ={olap_row:?}"
            ));
        }
    }
    Ok(())
}

/// Assert strict parity between Exact Cell and OLAP multi-cell `CombinedViewReport`s,
/// including `authority_pin`, `read_token`, `definition_pin`, `revision`, and every
/// question answer's `lineage_ref` and `members`.
///
/// # Errors
/// Returns an error if any cell snapshot or answer differs.
pub fn assert_combined_view_parity(
    label: &str,
    exact: &CombinedViewReport,
    olap: &CombinedViewReport,
) -> Result<(), String> {
    if olap.view_kind != exact.view_kind
        || olap.is_consolidated_financial_statement != exact.is_consolidated_financial_statement
        || olap.read_only != exact.read_only
        || olap.cells.len() != exact.cells.len()
    {
        return Err(format!("{label}: combined-view header mismatch"));
    }
    for (exact_c, olap_c) in exact.cells.iter().zip(&olap.cells) {
        if olap_c.cell_id != exact_c.cell_id
            || olap_c.scope != exact_c.scope
            || olap_c.world != exact_c.world
            || olap_c.revision != exact_c.revision
            || olap_c.ledger_seq != exact_c.ledger_seq
            || olap_c.definition_version != exact_c.definition_version
            || olap_c.definition_pin != exact_c.definition_pin
            || olap_c.authority_pin != exact_c.authority_pin
            || olap_c.read_token != exact_c.read_token
            || olap_c.source_cursors != exact_c.source_cursors
            || olap_c.watermarks != exact_c.watermarks
            || olap_c.missing_feeds != exact_c.missing_feeds
            || olap_c.open_findings != exact_c.open_findings
            || olap_c.pending_observations != exact_c.pending_observations
            || olap_c.failed_coverage_gates != exact_c.failed_coverage_gates
            || olap_c.complete != exact_c.complete
        {
            return Err(format!(
                "{label}/{}: CellCutSnapshot metadata mismatch",
                exact_c.scope
            ));
        }
        if olap_c.answers.is_some() != exact_c.answers.is_some() {
            return Err(format!(
                "{label}/{}: answers presence mismatch",
                exact_c.scope
            ));
        }
        if let (Some(exact_ans), Some(olap_ans)) = (&exact_c.answers, &olap_c.answers) {
            if exact_ans.len() != olap_ans.len() {
                return Err(format!("{label}/{}: answer count mismatch", exact_c.scope));
            }
            for (q_name, eq) in exact_ans {
                let oq = olap_ans.get(q_name).ok_or_else(|| {
                    format!("{label}/{}/{q_name}: missing OLAP answer", exact_c.scope)
                })?;
                assert_query_result_parity(&format!("{label}/{}/{q_name}", exact_c.scope), eq, oq)?;
            }
        }
    }
    Ok(())
}

/// Assert `VIEW-ENGINE-1` parity between Exact Cell and OLAP `TransformResult`s
/// (normalizing only `engine`).
///
/// # Errors
/// Returns an error if rows, lineage pin, node volumes, or exclusions differ.
pub fn assert_transform_parity(
    label: &str,
    exact: &TransformResult,
    olap: &TransformResult,
) -> Result<(), String> {
    if olap.transform != exact.transform
        || olap.version != exact.version
        || olap.plan_pin != exact.plan_pin
        || olap.definition_pin != exact.definition_pin
        || olap.revision != exact.revision
        || olap.columns != exact.columns
        || olap.grain != exact.grain
        || olap.complete != exact.complete
        || olap.withheld != exact.withheld
        || olap.exclusions != exact.exclusions
        || olap.lineage_pin != exact.lineage_pin
        || olap.node_rows != exact.node_rows
        || olap.rows != exact.rows
    {
        return Err(format!(
            "{label}: transform parity mismatch (exact_rows={} olap_rows={})",
            exact.rows.len(),
            olap.rows.len()
        ));
    }
    Ok(())
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn seed_parity_world(seed: u64) -> Result<SimulatedWorld, String> {
    run_seeded_multi_cell_sim(SimConfig {
        seed,
        entity_count: 2,
        weeks: 16,
        stores_per_entity: 2,
        skus_per_entity: 2,
    })
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn assert_combined_view_engine_parity(
    label: &str,
    cells: &[EntityCell],
    mut olap_eval: impl FnMut(usize, &str) -> Result<QueryResult, String>,
) -> Result<(), String> {
    let spec = dst_coverage_spec();
    let refs: Vec<&EntityCell> = cells.iter().collect();
    let exact_report = evaluate_multi_cell_report(&refs, &spec, |idx, _, plan, ctx| {
        let book = refs
            .get(idx)
            .ok_or_else(|| CellError::Engine("missing cell".to_owned()))?;
        book.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })?;
    let olap_report = evaluate_multi_cell_report(&refs, &spec, |idx, q_name, _, _| {
        olap_eval(idx, q_name).map_err(CellError::Engine)
    })?;
    assert_combined_view_parity(label, &exact_report, &olap_report)
}

/// Layer 3A (`#[cfg(feature = "duckdb")]`): Full Exact-Cell vs `DuckDB` parity
/// across all declared retail questions, combined-view snapshots, and transforms.
///
/// # Errors
/// Returns an error if `DuckDB` syncing, evaluation, or any parity assertion fails.
#[cfg(feature = "duckdb")]
pub fn run_duckdb_parity_suite(seed: u64) -> Result<(), String> {
    use norn_cell::analytics::TenantId;
    use norn_cell::analytics::engine::{Engine as DuckEngine, Settings as DuckSettings};

    let world = seed_parity_world(seed)?;
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let duck = DuckEngine::new(DuckSettings {
        root: dir.path().join("duckdb"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("duckdb init: {e}"))?;

    let mut tenants = Vec::with_capacity(world.cells.len());
    for cell in &world.cells {
        tenants.push(sync_duckdb_cell(&duck, cell)?);
    }
    let eval_one = |book: &EntityCell, tenant: &TenantId, q_name: &str| {
        eval_duckdb_question(&duck, book, tenant, q_name)
    };
    for (cell, tenant) in world.cells.iter().zip(&tenants) {
        for q_name in RETAIL_QUESTIONS {
            let exact = cell.answer(q_name)?;
            let olap = eval_one(cell, tenant, q_name)?;
            assert_query_result_parity(
                &format!("duckdb/{}/{q_name}", cell.entity_id),
                &exact,
                &olap,
            )?;
        }
        for tf_id in RETAIL_TRANSFORMS {
            let plan = cell.pack.transform_plan(tf_id)?;
            let exact = cell.evaluate_transform(tf_id)?;
            let olap = cell
                .cell()?
                .store()
                .evaluate_transform_duckdb(&plan, "main", &cell.scope, |_| Ok(RowAccess::Read))
                .map_err(|e| format!("duckdb transform {tf_id}: {e}"))?;
            assert_transform_parity(&format!("duckdb/{}/{tf_id}", cell.entity_id), &exact, &olap)?;
        }
    }
    assert_combined_view_engine_parity("duckdb/combined_view", &world.cells, |idx, q_name| {
        let book = world.cells.get(idx).ok_or("missing cell")?;
        let tenant = tenants.get(idx).ok_or("missing tenant")?;
        eval_one(book, tenant, q_name)
    })
}

#[cfg(feature = "duckdb")]
fn sync_duckdb_cell(
    duck: &norn_cell::analytics::engine::Engine,
    cell: &EntityCell,
) -> Result<norn_cell::analytics::TenantId, String> {
    let store = cell.cell()?.store();
    let tenant = norn_cell::analytics::TenantId::of_scope(&cell.scope);
    let mut frontier = duck
        .frontier(
            &store
                .analytics_frontier(&tenant, "main")
                .map_err(|e| format!("frontier: {e}"))?,
        )
        .map_err(|e| format!("duck frontier: {} {}", e.code, e.detail))?
        .frontier;
    loop {
        let batch = store
            .analytics_delta(&tenant, "main", frontier, 64)
            .map_err(|e| format!("delta: {e}"))?;
        frontier = duck
            .sync(&batch.request)
            .map_err(|e| format!("duck sync: {} {}", e.code, e.detail))?
            .frontier;
        if batch.complete {
            break;
        }
    }
    Ok(tenant)
}

#[cfg(feature = "duckdb")]
fn eval_duckdb_question(
    duck: &norn_cell::analytics::engine::Engine,
    book: &EntityCell,
    tenant: &norn_cell::analytics::TenantId,
    q_name: &str,
) -> Result<QueryResult, String> {
    let plan = book.pack.retail_question(q_name)?;
    let prepared = book
        .cell()?
        .store()
        .analytics_prepare(&plan, book.query_context(), tenant)
        .map_err(|e| format!("prepare {q_name}: {e}"))?;
    let response = duck
        .evaluate(prepared.request())
        .map_err(|e| format!("duck eval {q_name}: {} {}", e.code, e.detail))?;
    book.cell()?
        .store()
        .analytics_answer(prepared, response)
        .map_err(|e| format!("answer {q_name}: {e}"))
}

/// Layer 3B (`#[cfg(feature = "datafusion")]`): Full Exact-Cell vs `DataFusion` parity
/// across all declared retail questions, combined-view snapshots, and transforms.
///
/// # Errors
/// Returns an error if `DataFusion` syncing, evaluation, or any parity assertion fails.
#[cfg(feature = "datafusion")]
pub fn run_datafusion_parity_suite(seed: u64) -> Result<(), String> {
    use norn_cell::analytics::TenantId;
    use norn_cell::analytics::datafusion::{Engine as DfEngine, Settings as DfSettings};

    let world = seed_parity_world(seed)?;
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let df = DfEngine::new(DfSettings {
        root: dir.path().join("datafusion"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("datafusion init: {e}"))?;

    let mut tenants = Vec::with_capacity(world.cells.len());
    for cell in &world.cells {
        tenants.push(sync_datafusion_cell(&df, cell)?);
    }
    let eval_one = |book: &EntityCell, tenant: &TenantId, q_name: &str| {
        eval_datafusion_question(&df, book, tenant, q_name)
    };
    for (cell, tenant) in world.cells.iter().zip(&tenants) {
        for q_name in RETAIL_QUESTIONS {
            let exact = cell.answer(q_name)?;
            let olap = eval_one(cell, tenant, q_name)?;
            assert_query_result_parity(
                &format!("datafusion/{}/{q_name}", cell.entity_id),
                &exact,
                &olap,
            )?;
        }
        for tf_id in RETAIL_TRANSFORMS {
            let plan = cell.pack.transform_plan(tf_id)?;
            let exact = cell.evaluate_transform(tf_id)?;
            let olap = cell
                .cell()?
                .store()
                .evaluate_transform_datafusion(&plan, "main", &cell.scope, |_| Ok(RowAccess::Read))
                .map_err(|e| format!("datafusion transform {tf_id}: {e}"))?;
            assert_transform_parity(
                &format!("datafusion/{}/{tf_id}", cell.entity_id),
                &exact,
                &olap,
            )?;
        }
    }
    assert_combined_view_engine_parity("datafusion/combined_view", &world.cells, |idx, q_name| {
        let book = world.cells.get(idx).ok_or("missing cell")?;
        let tenant = tenants.get(idx).ok_or("missing tenant")?;
        eval_one(book, tenant, q_name)
    })
}

#[cfg(feature = "datafusion")]
fn sync_datafusion_cell(
    df: &norn_cell::analytics::datafusion::Engine,
    cell: &EntityCell,
) -> Result<norn_cell::analytics::TenantId, String> {
    let store = cell.cell()?.store();
    let tenant = norn_cell::analytics::TenantId::of_scope(&cell.scope);
    let mut frontier = df
        .frontier(
            &store
                .analytics_frontier(&tenant, "main")
                .map_err(|e| format!("frontier: {e}"))?,
        )
        .map_err(|e| format!("df frontier: {} {}", e.code, e.detail))?
        .frontier;
    loop {
        let batch = store
            .analytics_delta(&tenant, "main", frontier, 64)
            .map_err(|e| format!("delta: {e}"))?;
        frontier = df
            .sync(&batch.request)
            .map_err(|e| format!("df sync: {} {}", e.code, e.detail))?
            .frontier;
        if batch.complete {
            break;
        }
    }
    Ok(tenant)
}

#[cfg(feature = "datafusion")]
fn eval_datafusion_question(
    df: &norn_cell::analytics::datafusion::Engine,
    book: &EntityCell,
    tenant: &norn_cell::analytics::TenantId,
    q_name: &str,
) -> Result<QueryResult, String> {
    let plan = book.pack.retail_question(q_name)?;
    let prepared = book
        .cell()?
        .store()
        .analytics_prepare(&plan, book.query_context(), tenant)
        .map_err(|e| format!("prepare {q_name}: {e}"))?;
    let response = df
        .evaluate(prepared.request())
        .map_err(|e| format!("df eval {q_name}: {} {}", e.code, e.detail))?;
    book.cell()?
        .store()
        .analytics_answer(prepared, response)
        .map_err(|e| format!("answer {q_name}: {e}"))
}
