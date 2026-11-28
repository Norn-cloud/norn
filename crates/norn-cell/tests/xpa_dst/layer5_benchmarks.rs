//! Layer 5 — Scale & Latency Benchmark Suite (Boat/CI runnable, lightweight smoke
//! in default unit tests).
//!
//! Pure-Rust, zero-GPL synthetic generators matching:
//! 1. **M5 Forecasting (Walmart hierarchical retail) shape:** configurable
//!    cardinality up to 3,049 SKUs × 10 stores × 5+ years (260+ weeks) of daily
//!    POS volume, sell prices, and calendar events, emitting exact weekly
//!    rollups, sell-through, WOS, GMROI, and safety stock.
//! 2. **`FPABench`-style multidimensional P&L recalculation workload:** configurable
//!    `smoke` / `medium` / `full` (`XPA_BENCH_SCALE`) scale profiles measuring
//!    ingest throughput, incremental commit latency, fork-branching latency, and
//!    exact-cell vs OLAP (`DuckDB` / `DataFusion`) query p50/p95/p99.
//!
//! Evidence class: `sampled` (performance telemetry and bounded synthetic
//! scalability). Never claims formal proof or unexecuted scale runs.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use norn_cell::CellStore;
use norn_ir::Decimal;
use serde::{Deserialize, Serialize};

use super::cuts::cell_id_for_entity;
use super::env::{DEFAULT_CLOCK_MS, EntityCell};
use super::prng::{
    CENT_SCALE, LatencySummary, SplitMix64, decimal_from_scaled, div_i128, div_u64, div_usize,
    exact_ratio_of_sums, rem_usize, scaled, week_timestamp_utc,
};

/// Scale profile selected via `XPA_BENCH_SCALE=smoke|medium|full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchScale {
    /// Fast CI smoke profile (< 200ms).
    Smoke,
    /// Medium Boat profile (~10k generated facts).
    Medium,
    /// Full Boat/CI profile (M5 3,049 SKUs × 10 stores × 260 weeks; 100k–1M+ `FPABench` facts).
    Full,
}

impl BenchScale {
    /// Parse `XPA_BENCH_SCALE` env var (defaults to `fallback` when unset).
    ///
    /// # Errors
    /// Returns an error if `XPA_BENCH_SCALE` is set to an unknown value.
    pub fn from_env(fallback: Self) -> Result<Self, String> {
        match std::env::var("XPA_BENCH_SCALE") {
            Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "smoke" => Ok(Self::Smoke),
                "medium" => Ok(Self::Medium),
                "full" => Ok(Self::Full),
                other => Err(format!(
                    "invalid XPA_BENCH_SCALE `{other}` (expected smoke|medium|full)"
                )),
            },
            Err(_) => Ok(fallback),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Medium => "medium",
            Self::Full => "full",
        }
    }
}

/// Configurable M5 Forecasting hierarchical retail workload shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct M5ShapeConfig {
    pub stores: usize,
    pub skus: usize,
    pub weeks: usize,
}

impl M5ShapeConfig {
    /// Full Walmart M5 reference cardinality (`10 stores × 3,049 SKUs × 260 weeks` = 5 years).
    #[must_use]
    pub const fn full_m5() -> Self {
        Self {
            stores: 10,
            skus: 3_049,
            weeks: 260,
        }
    }

    #[must_use]
    pub const fn for_scale(scale: BenchScale) -> Self {
        match scale {
            BenchScale::Smoke => Self {
                stores: 2,
                skus: 16,
                weeks: 8,
            },
            BenchScale::Medium => Self {
                stores: 10,
                skus: 128,
                weeks: 26,
            },
            BenchScale::Full => Self::full_m5(),
        }
    }
}

/// Exact aggregated metrics emitted by one M5-shaped hierarchical retail run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct M5WorkloadSummary {
    pub stores: usize,
    pub skus: usize,
    pub weeks: usize,
    pub daily_pos_events: u64,
    pub calendar_event_weeks: u64,
    pub total_units_sold: u64,
    pub total_revenue: String,
    pub total_gross_margin: String,
    pub portfolio_sell_through: Option<String>,
    pub portfolio_wos: Option<String>,
    pub portfolio_gmroi: Option<String>,
    pub avg_safety_stock_units: u64,
    pub generation_elapsed_us: u64,
}

#[derive(Default)]
struct M5Accum {
    daily_pos_events: u64,
    calendar_event_weeks: u64,
    total_units_sold: u64,
    total_received: u64,
    total_ending_on_hand: u64,
    total_revenue_scaled: i128,
    total_margin_scaled: i128,
    total_avg_inv_scaled: i128,
    total_safety_stock: u64,
}

/// Run the pure-Rust M5 hierarchical retail generator (daily POS + sell prices +
/// calendar events -> exact weekly rollups, sell-through, WOS, GMROI, safety stock).
///
/// # Errors
/// Returns an error if exact decimal arithmetic overflows.
pub fn run_m5_synthetic_workload(
    seed: u64,
    cfg: M5ShapeConfig,
) -> Result<M5WorkloadSummary, String> {
    let start = Instant::now();
    let mut rng = SplitMix64::new(seed).fork(0x4D35_0001);
    let mut acc = M5Accum::default();

    for store_idx in 0..cfg.stores {
        let store_lift = u64::try_from(rem_usize(store_idx, 3))
            .unwrap_or(0)
            .saturating_add(1);
        for sku_idx in 0..cfg.skus {
            accumulate_m5_series(
                store_idx, sku_idx, store_lift, cfg.weeks, &mut rng, &mut acc,
            )?;
        }
    }

    let weeks_i128 = i128::try_from(cfg.weeks.max(1)).unwrap_or(1);
    let avg_weekly_scaled = div_i128(scaled(i128::from(acc.total_units_sold)), weeks_i128);
    let portfolio_sell_through = exact_ratio_of_sums(
        scaled(i128::from(acc.total_units_sold)),
        scaled(i128::from(acc.total_received)),
    )?
    .map(Decimal::canonical);
    let portfolio_wos = exact_ratio_of_sums(
        scaled(i128::from(acc.total_ending_on_hand)),
        avg_weekly_scaled,
    )?
    .map(Decimal::canonical);
    let portfolio_gmroi = exact_ratio_of_sums(acc.total_margin_scaled, acc.total_avg_inv_scaled)?
        .map(Decimal::canonical);

    let series_count = u64::try_from(cfg.stores.saturating_mul(cfg.skus)).unwrap_or(0);
    let avg_safety_stock_units = div_u64(acc.total_safety_stock, series_count);
    let elapsed_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);

    Ok(M5WorkloadSummary {
        stores: cfg.stores,
        skus: cfg.skus,
        weeks: cfg.weeks,
        daily_pos_events: acc.daily_pos_events,
        calendar_event_weeks: acc.calendar_event_weeks,
        total_units_sold: acc.total_units_sold,
        total_revenue: decimal_from_scaled(acc.total_revenue_scaled)?.canonical(),
        total_gross_margin: decimal_from_scaled(acc.total_margin_scaled)?.canonical(),
        portfolio_sell_through,
        portfolio_wos,
        portfolio_gmroi,
        avg_safety_stock_units,
        generation_elapsed_us: elapsed_us,
    })
}

fn accumulate_m5_series(
    store_idx: usize,
    sku_idx: usize,
    store_lift: u64,
    weeks: usize,
    rng: &mut SplitMix64,
    acc: &mut M5Accum,
) -> Result<(), String> {
    let base_cost_cents = rng.range_u64(250, 4_500);
    let markup_bps = rng.range_u64(1_250, 2_100);
    let sell_price_cents = div_u64(base_cost_cents.saturating_mul(markup_bps), 1_000);
    let unit_cost_scaled = i128::from(base_cost_cents).saturating_mul(CENT_SCALE);
    let sell_price_scaled = i128::from(sell_price_cents).saturating_mul(CENT_SCALE);
    let unit_margin_scaled = sell_price_scaled
        .checked_sub(unit_cost_scaled)
        .ok_or("m5 unit margin underflow")?;

    let mut series_sold: u64 = 0;
    let mut max_weekly: u64 = 0;
    for week in 0..weeks {
        let is_event_week = rem_usize(week, 13) == 6;
        if is_event_week && store_idx == 0 && sku_idx == 0 {
            acc.calendar_event_weeks = acc.calendar_event_weeks.saturating_add(1);
        }
        let mut weekly_qty: u64 = 0;
        for day in 0_u64..7 {
            let weekend_boost = u64::from(day >= 5);
            let event_boost = u64::from(is_event_week).saturating_mul(2);
            let base = rng.range_u64(0, 3).saturating_add(store_lift);
            weekly_qty = weekly_qty
                .saturating_add(base)
                .saturating_add(weekend_boost)
                .saturating_add(event_boost);
            acc.daily_pos_events = acc.daily_pos_events.saturating_add(1);
        }
        series_sold = series_sold.saturating_add(weekly_qty);
        if weekly_qty > max_weekly {
            max_weekly = weekly_qty;
        }
    }

    let weeks_u64 = u64::try_from(weeks.max(1)).unwrap_or(1);
    let avg_weekly = div_u64(series_sold, weeks_u64);
    let safety_stock = max_weekly.saturating_sub(avg_weekly).saturating_mul(2);
    let ending_on_hand = avg_weekly.saturating_mul(4).saturating_add(safety_stock);
    let received = series_sold.saturating_add(ending_on_hand);
    let avg_inv_units = div_u64(received.saturating_add(ending_on_hand), 2);

    acc.total_units_sold = acc.total_units_sold.saturating_add(series_sold);
    acc.total_received = acc.total_received.saturating_add(received);
    acc.total_ending_on_hand = acc.total_ending_on_hand.saturating_add(ending_on_hand);
    acc.total_safety_stock = acc.total_safety_stock.saturating_add(safety_stock);
    acc.total_revenue_scaled = acc
        .total_revenue_scaled
        .checked_add(i128::from(series_sold).saturating_mul(sell_price_scaled))
        .ok_or("m5 total revenue overflow")?;
    acc.total_margin_scaled = acc
        .total_margin_scaled
        .checked_add(i128::from(series_sold).saturating_mul(unit_margin_scaled))
        .ok_or("m5 total margin overflow")?;
    acc.total_avg_inv_scaled = acc
        .total_avg_inv_scaled
        .checked_add(i128::from(avg_inv_units).saturating_mul(unit_cost_scaled))
        .ok_or("m5 avg inventory overflow")?;
    Ok(())
}

/// `FPABench`-style multidimensional P&L recalculation profile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FpaBenchConfig {
    pub stores: usize,
    pub skus: usize,
    pub weeks: usize,
    pub cell_sales_commits: usize,
    pub fork_branches: usize,
    pub query_iterations: usize,
}

impl FpaBenchConfig {
    #[must_use]
    pub const fn for_scale(scale: BenchScale) -> Self {
        match scale {
            BenchScale::Smoke => Self {
                stores: 2,
                skus: 4,
                weeks: 8,
                cell_sales_commits: 16,
                fork_branches: 4,
                query_iterations: 5,
            },
            BenchScale::Medium => Self {
                stores: 20,
                skus: 50,
                weeks: 12,
                cell_sales_commits: 200,
                fork_branches: 16,
                query_iterations: 20,
            },
            BenchScale::Full => Self {
                stores: 100,
                skus: 200,
                weeks: 52,
                cell_sales_commits: 2_000,
                fork_branches: 64,
                query_iterations: 50,
            },
        }
    }
}

/// Structured Layer 5 benchmark scorecard (JSON & Markdown serializable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkScorecard {
    pub scale: String,
    pub seed: u64,
    pub evidence_class: String,
    pub m5_summary: M5WorkloadSummary,
    pub fpabench_generated_facts: u64,
    pub fpabench_total_net_income: String,
    pub cell_commits_executed: usize,
    pub ingest_facts_per_sec: u64,
    pub commit_latency: LatencySummary,
    pub fork_branch_latency: LatencySummary,
    pub exact_query_latency: LatencySummary,
    pub duckdb_query_latency: Option<LatencySummary>,
    pub datafusion_query_latency: Option<LatencySummary>,
}

impl BenchmarkScorecard {
    /// Render a concise GitHub-flavored Markdown scorecard table.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "### xP&A Layer 5 Benchmark Scorecard (scale=`{}`, seed=`0x{:016x}`, evidence=`{}`)",
            self.scale, self.seed, self.evidence_class
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "| Metric | Value |");
        let _ = writeln!(out, "|---|---|");
        let _ = writeln!(
            out,
            "| M5 Shape (`stores × skus × weeks`) | `{} × {} × {}` ({} daily POS events) |",
            self.m5_summary.stores,
            self.m5_summary.skus,
            self.m5_summary.weeks,
            self.m5_summary.daily_pos_events
        );
        let _ = writeln!(
            out,
            "| M5 Revenue / Margin / GMROI | `{}` / `{}` / `{}` |",
            self.m5_summary.total_revenue,
            self.m5_summary.total_gross_margin,
            self.m5_summary.portfolio_gmroi.as_deref().unwrap_or("n/a")
        );
        let _ = writeln!(
            out,
            "| FPABench Facts / Net Income | `{}` facts / `{}` |",
            self.fpabench_generated_facts, self.fpabench_total_net_income
        );
        let _ = writeln!(
            out,
            "| Governed Cell Ingest Throughput | `{} facts/s` ({} commits) |",
            self.ingest_facts_per_sec, self.cell_commits_executed
        );
        let _ = writeln!(
            out,
            "| Incremental Commit Latency (p50 / p95 / p99) | `{} / {} / {} µs` |",
            self.commit_latency.p50_us, self.commit_latency.p95_us, self.commit_latency.p99_us
        );
        let _ = writeln!(
            out,
            "| Fork Branching Latency (p50 / p95 / p99) | `{} / {} / {} µs` |",
            self.fork_branch_latency.p50_us,
            self.fork_branch_latency.p95_us,
            self.fork_branch_latency.p99_us
        );
        let _ = writeln!(
            out,
            "| Exact Cell Query Latency (p50 / p95 / p99) | `{} / {} / {} µs` |",
            self.exact_query_latency.p50_us,
            self.exact_query_latency.p95_us,
            self.exact_query_latency.p99_us
        );
        if let Some(duck) = &self.duckdb_query_latency {
            let _ = writeln!(
                out,
                "| DuckDB OLAP Query Latency (p50 / p95 / p99) | `{} / {} / {} µs` |",
                duck.p50_us, duck.p95_us, duck.p99_us
            );
        }
        if let Some(df) = &self.datafusion_query_latency {
            let _ = writeln!(
                out,
                "| DataFusion OLAP Query Latency (p50 / p95 / p99) | `{} / {} / {} µs` |",
                df.p50_us, df.p95_us, df.p99_us
            );
        }
        out
    }
}

/// Run the full Layer 5 benchmark harness at `scale` and `seed`, returning a
/// structured [`BenchmarkScorecard`].
///
/// # Errors
/// Returns an error if any workload generation, cell commit, fork, or query fails.
pub fn run_benchmark_scorecard(scale: BenchScale, seed: u64) -> Result<BenchmarkScorecard, String> {
    let m5_summary = run_m5_synthetic_workload(seed, M5ShapeConfig::for_scale(scale))?;
    let fpa_cfg = FpaBenchConfig::for_scale(scale);
    let (fpabench_generated_facts, fpabench_net_scaled) = run_fpabench_pnl_stream(seed, fpa_cfg)?;
    let (cell, ingest_facts_per_sec, commit_latency) = benchmark_cell_ingest(seed, fpa_cfg)?;
    let fork_branch_latency = benchmark_fork_branching(&cell, fpa_cfg.fork_branches)?;

    let plan = cell
        .pack
        .retail_question_with_bound("supply.retail.store_gross_margin_rate_weekly", 10_000)?;
    let exact_query_latency = benchmark_exact_queries(&cell, &plan, fpa_cfg.query_iterations)?;
    let duckdb_query_latency = benchmark_duckdb_queries(&cell, &plan, fpa_cfg.query_iterations)?;
    let datafusion_query_latency =
        benchmark_datafusion_queries(&cell, &plan, fpa_cfg.query_iterations)?;

    Ok(BenchmarkScorecard {
        scale: scale.as_str().to_owned(),
        seed,
        evidence_class: "sampled".to_owned(),
        m5_summary,
        fpabench_generated_facts,
        fpabench_total_net_income: decimal_from_scaled(fpabench_net_scaled)?.canonical(),
        cell_commits_executed: fpa_cfg.cell_sales_commits,
        ingest_facts_per_sec,
        commit_latency,
        fork_branch_latency,
        exact_query_latency,
        duckdb_query_latency,
        datafusion_query_latency,
    })
}

fn benchmark_cell_ingest(
    seed: u64,
    cfg: FpaBenchConfig,
) -> Result<(EntityCell, u64, LatencySummary), String> {
    let mut cell = EntityCell::open(
        &cell_id_for_entity(seed, 500),
        "LE-BENCH",
        "EGP",
        "org_xpa_bench",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    cell.register_book("xP&A Benchmark Book", "corporation")?;
    let bench_stores = cfg.stores.clamp(1, 8);
    let bench_skus = cfg.skus.clamp(1, 8);
    for s_idx in 0..bench_skus {
        let sku = format!("SKU-B-{s_idx}");
        cell.register_sku(&sku, "dry_goods", "each")?;
        cell.record_unit_cost(
            &sku,
            &format!("COST-B-{s_idx}"),
            "SUP-BENCH",
            "100",
            "2026-07-01T00:00:00Z",
        )?;
    }
    for st_idx in 0..bench_stores {
        let store = format!("ST-B-{st_idx}");
        cell.register_store(&store, &store, "CUST-B", "mall_store", Some("MALL-CFC"))?;
    }

    let mut rng = SplitMix64::new(seed).fork(0xBE4C_0001);
    let mut latencies = Vec::with_capacity(cfg.cell_sales_commits);
    let ingest_start = Instant::now();
    for idx in 0..cfg.cell_sales_commits {
        let st_idx = rem_usize(idx, bench_stores);
        let s_idx = rem_usize(div_usize(idx, bench_stores), bench_skus);
        let week = u32::try_from(rem_usize(idx, 16)).unwrap_or(0);
        let day = u32::try_from(rem_usize(idx, 6)).unwrap_or(0);
        let qty = rng.range_u64(1, 10);
        let rev = qty.saturating_mul(rng.range_u64(140, 240));
        let sold_at = week_timestamp_utc(week, day, 12)?;
        let t0 = Instant::now();
        cell.record_costed_sale(
            &format!("SALE-B-{idx}"),
            &format!("ST-B-{st_idx}"),
            &format!("SKU-B-{s_idx}"),
            &qty.to_string(),
            "each",
            &rev.to_string(),
            "100",
            &format!("COST-B-{s_idx}"),
            &sold_at,
        )?;
        latencies.push(t0.elapsed());
    }
    let elapsed_us = ingest_start
        .elapsed()
        .max(Duration::from_micros(1))
        .as_micros()
        .max(1);
    let commits_u128 = u128::try_from(cfg.cell_sales_commits).unwrap_or(0);
    let throughput = commits_u128
        .saturating_mul(1_000_000)
        .checked_div(elapsed_us)
        .unwrap_or(0);
    let ingest_facts_per_sec = u64::try_from(throughput).unwrap_or(u64::MAX);
    Ok((
        cell,
        ingest_facts_per_sec,
        LatencySummary::from_durations(&latencies),
    ))
}

fn benchmark_fork_branching(
    cell: &EntityCell,
    fork_branches: usize,
) -> Result<LatencySummary, String> {
    let head = cell.head()?;
    let mut latencies = Vec::with_capacity(fork_branches);
    for idx in 0..fork_branches {
        let world = format!("bench-fork-{idx}");
        let t0 = Instant::now();
        let forked: CellStore = cell
            .cell()?
            .store()
            .fork(&world, &head)
            .map_err(|e| format!("bench fork {world}: {e}"))?;
        latencies.push(t0.elapsed());
        drop(forked);
    }
    Ok(LatencySummary::from_durations(&latencies))
}

fn benchmark_exact_queries(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    iterations: usize,
) -> Result<LatencySummary, String> {
    let ctx = cell.query_context();
    let mut latencies = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let _ = cell
            .cell()?
            .store()
            .evaluate_question(plan, ctx.clone(), |_| Ok(true))
            .map_err(|e| format!("bench exact query: {e}"))?;
        latencies.push(t0.elapsed());
    }
    Ok(LatencySummary::from_durations(&latencies))
}

fn run_fpabench_pnl_stream(seed: u64, cfg: FpaBenchConfig) -> Result<(u64, i128), String> {
    let mut rng = SplitMix64::new(seed).fork(0xF9AB_E4C4);
    let mut facts: u64 = 0;
    let mut net_income_scaled: i128 = 0;
    for _store in 0..cfg.stores {
        let store_rent_weekly = scaled(i128::from(rng.range_u64(800, 3_500)));
        for _week in 0..cfg.weeks {
            let mut week_gm: i128 = 0;
            for _sku in 0..cfg.skus {
                let qty = i128::from(rng.range_u64(1, 25));
                let price = scaled(i128::from(rng.range_u64(50, 350)));
                let cost = scaled(i128::from(rng.range_u64(25, 200)));
                let gm = qty
                    .saturating_mul(price)
                    .checked_sub(qty.saturating_mul(cost))
                    .ok_or("fpa gm underflow")?;
                week_gm = week_gm.checked_add(gm).ok_or("fpa week_gm overflow")?;
                facts = facts.saturating_add(1);
            }
            let week_ni = week_gm
                .checked_sub(store_rent_weekly)
                .ok_or("fpa net income underflow")?;
            net_income_scaled = net_income_scaled
                .checked_add(week_ni)
                .ok_or("fpa total net income overflow")?;
        }
    }
    Ok((facts, net_income_scaled))
}

#[cfg(feature = "duckdb")]
fn benchmark_duckdb_queries(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    iterations: usize,
) -> Result<Option<LatencySummary>, String> {
    use norn_cell::analytics::TenantId;
    use norn_cell::analytics::engine::{Engine as DuckEngine, Settings as DuckSettings};

    let dir = tempfile::tempdir().map_err(|e| format!("duck tempdir: {e}"))?;
    let duck = DuckEngine::new(DuckSettings {
        root: dir.path().join("duckdb"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("duck init: {e}"))?;
    let store = cell.cell()?.store();
    let tenant = TenantId::of_scope(&cell.scope);
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
            .analytics_delta(&tenant, "main", frontier, 128)
            .map_err(|e| format!("delta: {e}"))?;
        frontier = duck
            .sync(&batch.request)
            .map_err(|e| format!("duck sync: {} {}", e.code, e.detail))?
            .frontier;
        if batch.complete {
            break;
        }
    }
    let ctx = cell.query_context();
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let prepared = store
            .analytics_prepare(plan, ctx.clone(), &tenant)
            .map_err(|e| format!("duck prepare: {e}"))?;
        let response = duck
            .evaluate(prepared.request())
            .map_err(|e| format!("duck eval: {} {}", e.code, e.detail))?;
        let _ = store
            .analytics_answer(prepared, response)
            .map_err(|e| format!("duck answer: {e}"))?;
        samples.push(t0.elapsed());
    }
    Ok(Some(LatencySummary::from_durations(&samples)))
}

#[cfg(not(feature = "duckdb"))]
fn benchmark_duckdb_queries(
    _cell: &EntityCell,
    _plan: &norn_checker::metric::compute::QuestionPlan,
    _iterations: usize,
) -> Result<Option<LatencySummary>, String> {
    Ok(None)
}

#[cfg(feature = "datafusion")]
fn benchmark_datafusion_queries(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    iterations: usize,
) -> Result<Option<LatencySummary>, String> {
    use norn_cell::analytics::TenantId;
    use norn_cell::analytics::datafusion::{Engine as DfEngine, Settings as DfSettings};

    let dir = tempfile::tempdir().map_err(|e| format!("df tempdir: {e}"))?;
    let df = DfEngine::new(DfSettings {
        root: dir.path().join("datafusion"),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .map_err(|e| format!("df init: {e}"))?;
    let store = cell.cell()?.store();
    let tenant = TenantId::of_scope(&cell.scope);
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
            .analytics_delta(&tenant, "main", frontier, 128)
            .map_err(|e| format!("delta: {e}"))?;
        frontier = df
            .sync(&batch.request)
            .map_err(|e| format!("df sync: {} {}", e.code, e.detail))?
            .frontier;
        if batch.complete {
            break;
        }
    }
    let ctx = cell.query_context();
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let prepared = store
            .analytics_prepare(plan, ctx.clone(), &tenant)
            .map_err(|e| format!("df prepare: {e}"))?;
        let response = df
            .evaluate(prepared.request())
            .map_err(|e| format!("df eval: {} {}", e.code, e.detail))?;
        let _ = store
            .analytics_answer(prepared, response)
            .map_err(|e| format!("df answer: {e}"))?;
        samples.push(t0.elapsed());
    }
    Ok(Some(LatencySummary::from_durations(&samples)))
}

#[cfg(not(feature = "datafusion"))]
fn benchmark_datafusion_queries(
    _cell: &EntityCell,
    _plan: &norn_checker::metric::compute::QuestionPlan,
    _iterations: usize,
) -> Result<Option<LatencySummary>, String> {
    Ok(None)
}
