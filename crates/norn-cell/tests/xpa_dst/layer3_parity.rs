//! Layer 3 — Differential Parity (Exact Cell Evaluator vs `DuckDB` / `DataFusion` OLAP).
//!
//! Feature-gated (`#[cfg(feature = "duckdb")]` and `#[cfg(feature = "datafusion")]`)
//! differential parity suites evaluating all 7 declared retail questions (with
//! nonempty seeded `StoreCommercialRollup` rows so WOS and GMROI never pass
//! vacuously on empty results), the full pack-authored `retail.coverage.json`
//! combined-view spec (`evaluate_combined_management_view_from_spec`), and
//! declared retail transforms across identical pinned cuts.
//!
//! Asserts complete envelope equality normalizing ONLY `engine` (verifying
//! `query_pin`, `revision`, `definition_pin`, `authority_pin`, `read_token`,
//! `context`, `lineage_ref`, `row.members`, bucket totals/values, and transform
//! `rows` / `lineage_pin` / `node_rows`).

use norn_cell::combined_view::CombinedViewReport;
use norn_cell::compute::transform::TransformResult;
use norn_cell::compute::{Engine as ComputeEngine, QueryResult};

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_cell::CellError;
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_cell::compute::QueryContext;
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_cell::compute::transform::RowAccess;
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_checker::metric::compute::QuestionPlan;

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::actions::StoreKpiInput;
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::cuts::{cell_id_for_entity, evaluate_multi_cell_report, retail_coverage_spec};
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::env::{DEFAULT_CLOCK_MS, EntityCell, RETAIL_QUESTIONS, RETAIL_TRANSFORMS};
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::prng::SplitMix64;

/// Assert complete byte-for-byte `QueryResult` envelope equality between Exact
/// Cell and OLAP, normalizing ONLY `engine` (preserving `context`, `rows`,
/// `row.members`, `lineage_ref`, `query_pin`, `definition_pin`, `revision`,
/// `freshness`, `exclusions`, and `source_disagreements`).
///
/// # Errors
/// Returns an error if the engine label is wrong or any envelope field differs.
pub fn assert_query_result_parity(
    label: &str,
    exact: &QueryResult,
    olap: &QueryResult,
    expected_olap_engine: ComputeEngine,
) -> Result<(), String> {
    if exact.engine != ComputeEngine::Exact {
        return Err(format!(
            "{label}: expected Exact engine on baseline, got {:?}",
            exact.engine
        ));
    }
    if olap.engine != expected_olap_engine {
        return Err(format!(
            "{label}: expected {expected_olap_engine:?} on OLAP answer, got {:?}",
            olap.engine
        ));
    }
    let mut normalized = olap.clone();
    normalized.engine = exact.engine;
    if normalized != *exact {
        return Err(format!(
            "{label}: QueryResult envelope mismatch (normalized only `engine`):\n  exact={exact:?}\n  olap ={olap:?}"
        ));
    }
    Ok(())
}

/// Assert complete `CombinedViewReport` equality across Exact Cell and OLAP
/// (normalizing ONLY `answer.engine` on every disclosed answer) and require
/// every cell snapshot to be `complete` with nonempty disclosed answers.
///
/// # Errors
/// Returns an error if any cell snapshot is incomplete or any field differs.
pub fn assert_combined_view_parity(
    label: &str,
    exact: &CombinedViewReport,
    olap: &CombinedViewReport,
    expected_olap_engine: ComputeEngine,
) -> Result<(), String> {
    if exact.cells.is_empty() {
        return Err(format!("{label}: combined view report had zero cells"));
    }
    for cell in &exact.cells {
        if !cell.complete || cell.answers.is_none() {
            return Err(format!(
                "{label}/{}: exact combined-view snapshot must be complete (missing={:?}, gates={:?}, findings={}, pending={})",
                cell.scope,
                cell.missing_feeds,
                cell.failed_coverage_gates,
                cell.open_findings,
                cell.pending_observations
            ));
        }
    }
    let mut normalized = olap.clone();
    for cell in &mut normalized.cells {
        if let Some(answers) = &mut cell.answers {
            for (q_name, ans) in answers {
                if ans.engine != expected_olap_engine {
                    return Err(format!(
                        "{label}/{}/{q_name}: expected {expected_olap_engine:?}, got {:?}",
                        cell.scope, ans.engine
                    ));
                }
                ans.engine = ComputeEngine::Exact;
            }
        }
    }
    if normalized != *exact {
        return Err(format!(
            "{label}: CombinedViewReport mismatch (normalized only answer `engine`)"
        ));
    }
    Ok(())
}

/// Assert `VIEW-ENGINE-1` complete `TransformResult` envelope equality between
/// Exact Cell and OLAP, normalizing ONLY `engine`.
///
/// # Errors
/// Returns an error if the engine label is wrong or any transform field differs.
pub fn assert_transform_parity(
    label: &str,
    exact: &TransformResult,
    olap: &TransformResult,
    expected_olap_engine: ComputeEngine,
) -> Result<(), String> {
    if exact.engine != ComputeEngine::Exact {
        return Err(format!(
            "{label}: expected Exact transform engine, got {:?}",
            exact.engine
        ));
    }
    if olap.engine != expected_olap_engine {
        return Err(format!(
            "{label}: expected {expected_olap_engine:?} transform engine, got {:?}",
            olap.engine
        ));
    }
    let mut normalized = olap.clone();
    normalized.engine = exact.engine;
    if normalized != *exact {
        return Err(format!(
            "{label}: TransformResult parity mismatch (exact_rows={} olap_rows={})",
            exact.rows.len(),
            olap.rows.len()
        ));
    }
    Ok(())
}

/// Seed a 2-entity parity world containing nonempty costed sales, costed lots,
/// verified `on_hand` & `available` counts, opening & closing valuation cuts,
/// verified `StoreCommercialRollup` rows for `2026-08-31` (making WOS and GMROI
/// nonempty), and fresh baseline rows across all 4 feeds so the full declared
/// `retail.coverage.json` spec evaluates `complete == true`.
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn seed_parity_cells(seed: u64) -> Result<Vec<EntityCell>, String> {
    let mut rng = SplitMix64::new(seed).fork(0x1A3E_3001);
    let specs = [
        (0_usize, "LE-PARITY-EGP", "EGP", "sole_proprietorship"),
        (1_usize, "LE-PARITY-USD", "USD", "partnership"),
    ];
    let mut cells = Vec::with_capacity(specs.len());
    for (entity_idx, entity_id, ccy, kind) in specs {
        let mut cell = EntityCell::open(
            &cell_id_for_entity(seed, entity_idx.saturating_add(200)),
            entity_id,
            ccy,
            &format!("org_parity_{entity_idx}"),
            "prod",
            DEFAULT_CLOCK_MS,
        )?;
        cell.register_book(&format!("Parity Book {entity_idx}"), kind)?;
        for sku_idx in 0_usize..2 {
            let sku_id = format!("SKU-P{entity_idx}-{sku_idx}");
            let cost_ref = format!("COST-P{entity_idx}-{sku_idx}");
            let unit_cost = i64::try_from(rng.range_u64(60, 180)).unwrap_or(100);
            cell.register_sku(&sku_id, "dry_goods", "each")?;
            cell.record_unit_cost(
                &sku_id,
                &cost_ref,
                "SUP-PARITY",
                &unit_cost.to_string(),
                "2026-07-01T00:00:00Z",
            )?;
            for store_idx in 0_usize..2 {
                let store_id = format!("ST-P{entity_idx}-{store_idx}");
                if sku_idx == 0 {
                    cell.register_store(
                        &store_id,
                        &format!("Parity Store {entity_idx}-{store_idx}"),
                        "CUST-MALL-CFC-OPS",
                        "mall_store",
                        Some("MALL-CFC"),
                    )?;
                }
                seed_parity_position(
                    &mut cell, &mut rng, entity_idx, store_idx, sku_idx, &store_id, &sku_id,
                    &cost_ref, unit_cost,
                )?;
            }
        }
        cell.ingest_baseline_feeds(
            &format!("ST-P{entity_idx}-0"),
            &format!("SKU-P{entity_idx}-0"),
            "100",
            "2026-09-02T12:00:00Z",
            &format!("parity-{entity_idx}"),
        )?;
        cell.refresh_watermarks();
        cells.push(cell);
    }
    Ok(cells)
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
struct ParityPositionKey<'a> {
    entity_idx: usize,
    store_idx: usize,
    sku_idx: usize,
    store_id: &'a str,
    sku_id: &'a str,
    cost_ref: &'a str,
    lot_id: &'a str,
    open_cut_id: &'a str,
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
struct ParityPositionAmounts {
    init_qty: i64,
    w8_sold: i64,
    w9_sold: i64,
    unit_cost: i64,
    margin_per_unit: i64,
    open_val: i64,
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
#[allow(
    clippy::too_many_arguments,
    reason = "explicit parity fixture seeding parameters"
)]
fn seed_parity_position(
    cell: &mut EntityCell,
    rng: &mut SplitMix64,
    entity_idx: usize,
    store_idx: usize,
    sku_idx: usize,
    store_id: &str,
    sku_id: &str,
    cost_ref: &str,
    unit_cost: i64,
) -> Result<(), String> {
    let init_qty = i64::try_from(rng.range_u64(80, 140)).unwrap_or(100);
    let w8_sold = i64::try_from(rng.range_u64(4, 12)).unwrap_or(6);
    let w9_sold = i64::try_from(rng.range_u64(6, 16)).unwrap_or(10);
    let margin_per_unit = i64::try_from(rng.range_u64(30, 90)).unwrap_or(50);

    let lot_id = format!("LOT-P{entity_idx}-{store_idx}-{sku_idx}");
    cell.record_costed_lot(
        &lot_id,
        store_id,
        sku_id,
        "CUST-MALL-CFC-OPS",
        &init_qty.to_string(),
        "each",
        &unit_cost.to_string(),
        "2026-08-03T09:00:00Z",
    )?;
    let w8_rev = w8_sold.saturating_mul(unit_cost.saturating_add(margin_per_unit));
    cell.record_costed_sale(
        &format!("SALE-P{entity_idx}-{store_idx}-{sku_idx}-W8"),
        store_id,
        sku_id,
        &w8_sold.to_string(),
        "each",
        &w8_rev.to_string(),
        &unit_cost.to_string(),
        cost_ref,
        "2026-08-26T12:00:00Z",
    )?;
    let open_rem = init_qty.saturating_sub(w8_sold);
    let open_val = open_rem.saturating_mul(unit_cost);
    cell.update_lot_balance(
        &lot_id,
        store_id,
        sku_id,
        &open_rem.to_string(),
        &w8_sold.to_string(),
        &unit_cost.to_string(),
        "2026-08-29T18:00:00Z",
    )?;
    let open_cut_id = format!("CUT-OPEN-P{entity_idx}-{store_idx}-{sku_idx}");
    cell.record_stock_valuation_cut(
        &open_cut_id,
        store_id,
        sku_id,
        "each",
        "2026-08-31T00:00:00Z",
        &open_rem.to_string(),
        &open_val.to_string(),
    )?;

    let key = ParityPositionKey {
        entity_idx,
        store_idx,
        sku_idx,
        store_id,
        sku_id,
        cost_ref,
        lot_id: &lot_id,
        open_cut_id: &open_cut_id,
    };
    let amounts = ParityPositionAmounts {
        init_qty,
        w8_sold,
        w9_sold,
        unit_cost,
        margin_per_unit,
        open_val,
    };
    seed_parity_w9_close_and_rollup(cell, &key, &amounts)
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn seed_parity_w9_close_and_rollup(
    cell: &mut EntityCell,
    key: &ParityPositionKey<'_>,
    amt: &ParityPositionAmounts,
) -> Result<(), String> {
    let tag = format!("P{}-{}-{}", key.entity_idx, key.store_idx, key.sku_idx);
    let w9_rev = amt
        .w9_sold
        .saturating_mul(amt.unit_cost.saturating_add(amt.margin_per_unit));
    let w9_gm = amt.w9_sold.saturating_mul(amt.margin_per_unit);
    cell.record_costed_sale(
        &format!("SALE-{tag}-W9"),
        key.store_id,
        key.sku_id,
        &amt.w9_sold.to_string(),
        "each",
        &w9_rev.to_string(),
        &amt.unit_cost.to_string(),
        key.cost_ref,
        "2026-09-02T14:00:00Z",
    )?;
    let total_depleted = amt.w8_sold.saturating_add(amt.w9_sold);
    let close_rem = amt.init_qty.saturating_sub(total_depleted);
    let close_val = close_rem.saturating_mul(amt.unit_cost);
    cell.update_lot_balance(
        key.lot_id,
        key.store_id,
        key.sku_id,
        &close_rem.to_string(),
        &total_depleted.to_string(),
        &amt.unit_cost.to_string(),
        "2026-09-05T16:00:00Z",
    )?;
    let on_hand_id = format!("{}/{}/on_hand", key.store_id, key.sku_id);
    let count_ref = format!("CC-{tag}");
    cell.record_store_count(
        false,
        &on_hand_id,
        key.store_id,
        key.sku_id,
        &close_rem.to_string(),
        "each",
        "on_hand",
        "2026-09-06T10:00:00Z",
        &count_ref,
    )?;
    cell.record_store_count(
        false,
        &format!("{}/{}/available", key.store_id, key.sku_id),
        key.store_id,
        key.sku_id,
        &close_rem.saturating_sub(2).to_string(),
        "each",
        "available",
        "2026-09-06T10:00:00Z",
        &format!("AV-{tag}"),
    )?;
    cell.record_stock_valuation_cut(
        &format!("CUT-CLOSE-{tag}"),
        key.store_id,
        key.sku_id,
        "each",
        "2026-09-06T23:59:59.999999999Z",
        &close_rem.to_string(),
        &close_val.to_string(),
    )?;
    cell.record_store_kpi(&StoreKpiInput {
        rollup_id: &format!("KPI-{tag}-W9"),
        store_id: key.store_id,
        sku_id: key.sku_id,
        period_week: "2026-08-31",
        unit: "each",
        on_hand_count_id: &on_hand_id,
        opening_cut_id: key.open_cut_id,
        count_ref: &count_ref,
        count_as_of: "2026-09-06T10:00:00Z",
        costed_ref: key.cost_ref,
        period_start: "2026-08-31T00:00:00Z",
        period_end: "2026-09-06T23:59:59.999999999Z",
        sales_weeks: 1,
        on_hand_qty: &close_rem.to_string(),
        avg_weekly_sales_qty: &amt.w9_sold.to_string(),
        period_gross_margin: &w9_gm.to_string(),
        opening_stock_value: &amt.open_val.to_string(),
        closing_stock_value: &close_val.to_string(),
    })
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn assert_combined_view_engine_parity(
    label: &str,
    cells: &[EntityCell],
    expected_olap_engine: ComputeEngine,
    mut olap_eval: impl FnMut(usize, &QuestionPlan, QueryContext) -> Result<QueryResult, String>,
) -> Result<(), String> {
    let spec = retail_coverage_spec()?;
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
    // Honor the exact spec-projected `plan` and `ctx` passed by the combined-view evaluator.
    let olap_report = evaluate_multi_cell_report(&refs, &spec, |idx, _, plan, ctx| {
        olap_eval(idx, plan, ctx).map_err(CellError::Engine)
    })?;
    assert_combined_view_parity(label, &exact_report, &olap_report, expected_olap_engine)
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn assert_nonempty_question(label: &str, result: &QueryResult) -> Result<(), String> {
    if result.rows.is_empty() || !result.rows.values().any(|r| r.value.is_some()) {
        return Err(format!(
            "{label}: question produced vacuous empty/all-null rows ({} rows)",
            result.rows.len()
        ));
    }
    Ok(())
}

/// Layer 3A (`#[cfg(feature = "duckdb")]`): Full Exact-Cell vs `DuckDB` parity
/// across all 7 declared retail questions (nonempty WOS & GMROI), full
/// `retail.coverage.json` combined-view snapshots, and transforms.
///
/// # Errors
/// Returns an error if `DuckDB` syncing, evaluation, or any parity assertion fails.
#[cfg(feature = "duckdb")]
pub fn run_duckdb_parity_suite(seed: u64) -> Result<(), String> {
    use norn_cell::analytics::TenantId;
    use norn_cell::analytics::engine::{Engine as DuckEngine, Settings as DuckSettings};

    let cells = seed_parity_cells(seed)?;
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let duck = DuckEngine::new(DuckSettings {
        root: dir.path().join("duckdb"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("duckdb init: {e}"))?;

    let mut tenants = Vec::with_capacity(cells.len());
    for cell in &cells {
        tenants.push(sync_duckdb_cell(&duck, cell)?);
    }
    let eval_one =
        |book: &EntityCell, tenant: &TenantId, plan: &QuestionPlan, ctx: QueryContext| {
            eval_duckdb_question(&duck, book, tenant, plan, ctx)
        };
    for (cell, tenant) in cells.iter().zip(&tenants) {
        for q_name in RETAIL_QUESTIONS {
            let plan = cell.pack.retail_question(q_name)?;
            let ctx = cell.query_context();
            let exact = cell
                .cell()?
                .store()
                .evaluate_question(&plan, ctx.clone(), |_| Ok(true))
                .map_err(|e| format!("exact {q_name}: {e}"))?;
            let label = format!("duckdb/{}/{q_name}", cell.entity_id);
            assert_nonempty_question(&label, &exact)?;
            let olap = eval_one(cell, tenant, &plan, ctx)?;
            assert_query_result_parity(&label, &exact, &olap, ComputeEngine::Duckdb)?;
        }
        for tf_id in RETAIL_TRANSFORMS {
            let plan = cell.pack.transform_plan(tf_id)?;
            let exact = cell.evaluate_transform(tf_id)?;
            if exact.rows.is_empty() {
                return Err(format!(
                    "duckdb/{}/{tf_id}: empty transform rows",
                    cell.entity_id
                ));
            }
            let olap = cell
                .cell()?
                .store()
                .evaluate_transform_duckdb(&plan, "main", &cell.scope, |_| Ok(RowAccess::Read))
                .map_err(|e| format!("duckdb transform {tf_id}: {e}"))?;
            assert_transform_parity(
                &format!("duckdb/{}/{tf_id}", cell.entity_id),
                &exact,
                &olap,
                ComputeEngine::Duckdb,
            )?;
        }
    }
    assert_combined_view_engine_parity(
        "duckdb/combined_view",
        &cells,
        ComputeEngine::Duckdb,
        |idx, plan, ctx| {
            let book = cells.get(idx).ok_or("missing cell")?;
            let tenant = tenants.get(idx).ok_or("missing tenant")?;
            eval_one(book, tenant, plan, ctx)
        },
    )
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
    plan: &QuestionPlan,
    ctx: QueryContext,
) -> Result<QueryResult, String> {
    let prepared = book
        .cell()?
        .store()
        .analytics_prepare(plan, ctx, tenant)
        .map_err(|e| format!("prepare {}: {e}", plan.question().name))?;
    let response = duck.evaluate(prepared.request()).map_err(|e| {
        format!(
            "duck eval {}: {} {}",
            plan.question().name,
            e.code,
            e.detail
        )
    })?;
    book.cell()?
        .store()
        .analytics_answer(prepared, response)
        .map_err(|e| format!("answer {}: {e}", plan.question().name))
}

/// Layer 3B (`#[cfg(feature = "datafusion")]`): Full Exact-Cell vs `DataFusion`
/// parity across all 7 declared retail questions (nonempty WOS & GMROI), full
/// `retail.coverage.json` combined-view snapshots, and transforms.
///
/// # Errors
/// Returns an error if `DataFusion` syncing, evaluation, or any parity assertion fails.
#[cfg(feature = "datafusion")]
pub fn run_datafusion_parity_suite(seed: u64) -> Result<(), String> {
    use norn_cell::analytics::TenantId;
    use norn_cell::analytics::datafusion::{Engine as DfEngine, Settings as DfSettings};

    let cells = seed_parity_cells(seed)?;
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let df = DfEngine::new(DfSettings {
        root: dir.path().join("datafusion"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("datafusion init: {e}"))?;

    let mut tenants = Vec::with_capacity(cells.len());
    for cell in &cells {
        tenants.push(sync_datafusion_cell(&df, cell)?);
    }
    let eval_one =
        |book: &EntityCell, tenant: &TenantId, plan: &QuestionPlan, ctx: QueryContext| {
            eval_datafusion_question(&df, book, tenant, plan, ctx)
        };
    for (cell, tenant) in cells.iter().zip(&tenants) {
        for q_name in RETAIL_QUESTIONS {
            let plan = cell.pack.retail_question(q_name)?;
            let ctx = cell.query_context();
            let exact = cell
                .cell()?
                .store()
                .evaluate_question(&plan, ctx.clone(), |_| Ok(true))
                .map_err(|e| format!("exact {q_name}: {e}"))?;
            let label = format!("datafusion/{}/{q_name}", cell.entity_id);
            assert_nonempty_question(&label, &exact)?;
            let olap = eval_one(cell, tenant, &plan, ctx)?;
            assert_query_result_parity(&label, &exact, &olap, ComputeEngine::DataFusion)?;
        }
        for tf_id in RETAIL_TRANSFORMS {
            let plan = cell.pack.transform_plan(tf_id)?;
            let exact = cell.evaluate_transform(tf_id)?;
            if exact.rows.is_empty() {
                return Err(format!(
                    "datafusion/{}/{tf_id}: empty transform rows",
                    cell.entity_id
                ));
            }
            let olap = cell
                .cell()?
                .store()
                .evaluate_transform_datafusion(&plan, "main", &cell.scope, |_| Ok(RowAccess::Read))
                .map_err(|e| format!("datafusion transform {tf_id}: {e}"))?;
            assert_transform_parity(
                &format!("datafusion/{}/{tf_id}", cell.entity_id),
                &exact,
                &olap,
                ComputeEngine::DataFusion,
            )?;
        }
    }
    assert_combined_view_engine_parity(
        "datafusion/combined_view",
        &cells,
        ComputeEngine::DataFusion,
        |idx, plan, ctx| {
            let book = cells.get(idx).ok_or("missing cell")?;
            let tenant = tenants.get(idx).ok_or("missing tenant")?;
            eval_one(book, tenant, plan, ctx)
        },
    )
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
    plan: &QuestionPlan,
    ctx: QueryContext,
) -> Result<QueryResult, String> {
    let prepared = book
        .cell()?
        .store()
        .analytics_prepare(plan, ctx, tenant)
        .map_err(|e| format!("prepare {}: {e}", plan.question().name))?;
    let response = df
        .evaluate(prepared.request())
        .map_err(|e| format!("df eval {}: {} {}", plan.question().name, e.code, e.detail))?;
    book.cell()?
        .store()
        .analytics_answer(prepared, response)
        .map_err(|e| format!("answer {}: {e}", plan.question().name))
}
