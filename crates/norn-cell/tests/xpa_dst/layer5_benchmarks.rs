//! Layer 5 — Scale & Latency Benchmark Suite (Boat/CI runnable, lightweight smoke
//! in default unit tests).
//!
//! Pure-Rust, zero-GPL synthetic generators and governed cell/OLAP timings:
//! 1. **M5 Forecasting (Walmart hierarchical retail) stream generator**
//!    (in-memory synthetic stream up to `3,049 SKUs x 10 stores x 260 weeks`,
//!    reported separately from cell-loaded facts).
//! 2. **`FPABench`-style multidimensional P&L synthetic stream** (reported
//!    explicitly as `synthetic_facts_generated`, separate from cell-loaded facts).
//! 3. **Governed Cell & OLAP benchmark over the EXACT SAME loaded population**
//!    (`cell_facts_loaded` governed `PosSale` commits loaded into one `EntityCell`
//!    and mirrored identically into `DuckDB` / `DataFusion` as `olap_facts_loaded`),
//!    separating dedicated `cold_query_us` (fresh reader / freshly synced engine)
//!    from warm repeated latency percentiles (`p50`/`p95`/`p99`), sampling live
//!    process `VmRSS` BEFORE dropping the cell or OLAP engines, and verifying
//!    exactness against an independent oracle plus lineage completeness.
//!
//! Evidence class: `sampled` (performance telemetry and bounded synthetic
//! scalability). Never claims formal proof or unexecuted scale runs.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use norn_cell::compute::QueryResult;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, StoreOptions};
use norn_ir::Decimal;
use serde::{Deserialize, Serialize};

use super::cuts::{bucket_decimal_exact, cell_id_for_entity};
use super::env::EntityCell;
use super::prng::{
    CENT_SCALE, LatencySummary, SplitMix64, decimal_from_scaled, div_i128, div_u64, div_usize,
    exact_ratio_of_sums, independent_ratio_scaled, rem_usize, scaled, week_epoch_ms,
    week_monday_iso, week_timestamp_utc,
};

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use super::layer3_parity::assert_query_result_parity;
#[cfg(any(feature = "duckdb", feature = "datafusion"))]
use norn_cell::compute::Engine as ComputeEngine;

/// Scale profile selected via `XPA_BENCH_SCALE=smoke|medium|full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchScale {
    /// Fast CI smoke profile (< 250ms) with enough samples for meaningful p50/p95/p99.
    Smoke,
    /// Medium Boat profile (10 stores x 20 SKUs x 26 weeks; 1,000 governed cell/OLAP facts).
    Medium,
    /// Uncapped full Boat/CI profile (M5 3,049 SKUs x 10 stores x 260 weeks synthetic stream;
    /// full `10 stores x 50 SKUs x 52 weeks = 26,000` governed cell/OLAP facts by default,
    /// overridable via `XPA_BENCH_FACTS`).
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
    /// Full Walmart M5 reference cardinality (`10 stores x 3,049 SKUs x 260 weeks` = 5 years).
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

/// Exact aggregated metrics emitted by one in-memory M5-shaped synthetic stream run.
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

/// Run the pure-Rust M5 hierarchical retail synthetic stream generator (daily POS
/// + sell prices + calendar events -> exact weekly rollups, sell-through, WOS,
/// GMROI, safety stock). Not loaded into a cell; reported separately in scorecard.
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

    for store_ordinal in 0..cfg.stores {
        let store_lift = u64::try_from(rem_usize(store_ordinal, 3))
            .unwrap_or(0)
            .saturating_add(1);
        for sku_ordinal in 0..cfg.skus {
            accumulate_m5_series(
                store_ordinal,
                sku_ordinal,
                store_lift,
                cfg.weeks,
                &mut rng,
                &mut acc,
            )?;
        }
    }

    let weeks_i128 = i128::try_from(cfg.weeks.max(1)).unwrap_or(1);
    let avg_weekly_scaled = div_i128(scaled(i128::from(acc.total_units_sold)), weeks_i128);
    let portfolio_sell_through = exact_ratio_of_sums(
        scaled(i128::from(acc.total_units_sold)),
        scaled(i128::from(acc.total_received)),
    )?
    .as_ref()
    .map(Decimal::canonical);
    let portfolio_wos = exact_ratio_of_sums(
        scaled(i128::from(acc.total_ending_on_hand)),
        avg_weekly_scaled,
    )?
    .as_ref()
    .map(Decimal::canonical);
    let portfolio_gmroi = exact_ratio_of_sums(acc.total_margin_scaled, acc.total_avg_inv_scaled)?
        .as_ref()
        .map(Decimal::canonical);
    let series_count = u64::try_from(cfg.stores.saturating_mul(cfg.skus)).unwrap_or(0);
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
        avg_safety_stock_units: div_u64(acc.total_safety_stock, series_count),
        generation_elapsed_us: elapsed_us,
    })
}

fn accumulate_m5_series(
    store_ordinal: usize,
    sku_ordinal: usize,
    store_lift: u64,
    weeks: usize,
    rng: &mut SplitMix64,
    acc: &mut M5Accum,
) -> Result<(), String> {
    let base_cost_cents = rng.range_u64(250, 4_500);
    let sell_price_cents = div_u64(
        base_cost_cents.saturating_mul(rng.range_u64(1_250, 2_100)),
        1_000,
    );
    let unit_cost_scaled = i128::from(base_cost_cents).saturating_mul(CENT_SCALE);
    let sell_price_scaled = i128::from(sell_price_cents).saturating_mul(CENT_SCALE);
    let unit_margin_scaled = sell_price_scaled
        .checked_sub(unit_cost_scaled)
        .ok_or("m5 unit margin underflow")?;

    let mut series_sold: u64 = 0;
    let mut max_weekly: u64 = 0;
    for week in 0..weeks {
        let is_event_week = rem_usize(week, 13) == 6;
        if is_event_week && store_ordinal == 0 && sku_ordinal == 0 {
            acc.calendar_event_weeks = acc.calendar_event_weeks.saturating_add(1);
        }
        let mut weekly_qty: u64 = 0;
        for day in 0_u64..7 {
            let base = rng.range_u64(0, 3).saturating_add(store_lift);
            weekly_qty = weekly_qty
                .saturating_add(base)
                .saturating_add(u64::from(day >= 5))
                .saturating_add(u64::from(is_event_week).saturating_mul(2));
            acc.daily_pos_events = acc.daily_pos_events.saturating_add(1);
        }
        series_sold = series_sold.saturating_add(weekly_qty);
        max_weekly = max_weekly.max(weekly_qty);
    }

    let avg_weekly = div_u64(series_sold, u64::try_from(weeks.max(1)).unwrap_or(1));
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

/// `FPABench`-style multidimensional P&L recalculation and governed cell/OLAP profile.
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
    /// Construct the benchmark shape for `scale`, honoring optional
    /// `XPA_BENCH_FACTS` override for non-smoke Boat scale sweeps.
    ///
    /// # Errors
    /// Returns an error if `XPA_BENCH_FACTS` is set to an invalid integer.
    pub fn for_scale(scale: BenchScale) -> Result<Self, String> {
        let mut cfg = match scale {
            BenchScale::Smoke => Self {
                stores: 2,
                skus: 4,
                weeks: 8,
                cell_sales_commits: 24,
                fork_branches: 10,
                query_iterations: 20,
            },
            BenchScale::Medium => Self {
                stores: 10,
                skus: 20,
                weeks: 26,
                cell_sales_commits: 1_000,
                fork_branches: 20,
                query_iterations: 25,
            },
            BenchScale::Full => Self {
                stores: 10,
                skus: 50,
                weeks: 52,
                cell_sales_commits: 26_000,
                fork_branches: 64,
                query_iterations: 50,
            },
        };
        if scale != BenchScale::Smoke
            && let Ok(raw) = std::env::var("XPA_BENCH_FACTS")
        {
            let parsed = raw
                .trim()
                .parse::<usize>()
                .map_err(|e| format!("invalid XPA_BENCH_FACTS `{raw}`: {e}"))?;
            if parsed == 0 {
                return Err("XPA_BENCH_FACTS must be >= 1".to_owned());
            }
            cfg.cell_sales_commits = parsed;
        }
        Ok(cfg)
    }
}

/// Cold-start vs warm repeated query latency telemetry for one query engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineQueryTiming {
    /// First query latency on a freshly opened reader or freshly synced OLAP engine (microseconds).
    pub cold_query_us: u64,
    /// Repeated warm query latency percentiles (`p50`/`p95`/`p99`/`max`).
    pub warm: LatencySummary,
}

/// Structured Layer 5 benchmark scorecard (JSON & Markdown serializable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkScorecard {
    pub scale: String,
    pub seed: u64,
    pub evidence_class: String,
    pub m5_summary: M5WorkloadSummary,
    /// Facts streamed by the in-memory pure-Rust `FPABench` P&L generator (not loaded into cell).
    pub synthetic_facts_generated: u64,
    pub fpabench_total_net_income: String,
    /// Exact governed `PosSale` facts committed into `EntityCell` and queried by Exact Cell.
    pub cell_facts_loaded: usize,
    /// Exact governed `PosSale` facts mirrored from the same cell into OLAP engine(s) (`0` if OLAP disabled).
    pub olap_facts_loaded: usize,
    /// True only when Exact Cell (and every enabled OLAP engine) matched the exact arithmetic oracle.
    pub exactness_verified: bool,
    /// True only when `lineage_ref` is non-empty and bucket member totals equal `cell_facts_loaded`.
    pub lineage_verified: bool,
    /// Peak live process resident set size (`VmRSS` in `KiB` on Linux) sampled BEFORE dropping engines.
    pub rss_kib: Option<u64>,
    pub ingest_facts_per_sec: u64,
    pub commit_latency: LatencySummary,
    pub fork_branch_latency: LatencySummary,
    pub exact_query_timing: EngineQueryTiming,
    pub duckdb_query_timing: Option<EngineQueryTiming>,
    pub datafusion_query_timing: Option<EngineQueryTiming>,
}

fn format_latency_row(label: &str, lat: &LatencySummary) -> String {
    format!(
        "| {label} (p50 / p95 / p99, n={}) | `{} / {} / {} us` |",
        lat.samples, lat.p50_us, lat.p95_us, lat.p99_us
    )
}

fn format_query_row(label: &str, timing: &EngineQueryTiming, facts: usize) -> String {
    format!(
        "| {label} (cold / warm p50 / p95 / p99, n={}, {facts} facts) | `{} / {} / {} / {} us` |",
        timing.warm.samples,
        timing.cold_query_us,
        timing.warm.p50_us,
        timing.warm.p95_us,
        timing.warm.p99_us
    )
}

impl BenchmarkScorecard {
    /// Render an honest Markdown scorecard table separating in-memory synthetic
    /// stream sizes from exact cell/OLAP loaded populations and cold vs warm timings.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let rss = self
            .rss_kib
            .map_or_else(|| "n/a".to_owned(), |kib| format!("{kib} KiB"));
        let mut lines = vec![
            format!(
                "### xP&A Layer 5 Benchmark Scorecard (scale=`{}`, seed=`0x{:016x}`, evidence=`{}`)",
                self.scale, self.seed, self.evidence_class
            ),
            String::new(),
            "| Metric | Value |".to_owned(),
            "|---|---|".to_owned(),
            format!(
                "| M5 Synthetic Stream (`stores x skus x weeks`, in-memory) | `{} x {} x {}` ({} daily POS events) |",
                self.m5_summary.stores,
                self.m5_summary.skus,
                self.m5_summary.weeks,
                self.m5_summary.daily_pos_events
            ),
            format!(
                "| M5 Stream Revenue / Margin / GMROI | `{}` / `{}` / `{}` |",
                self.m5_summary.total_revenue,
                self.m5_summary.total_gross_margin,
                self.m5_summary.portfolio_gmroi.as_deref().unwrap_or("n/a")
            ),
            format!(
                "| FPABench Synthetic Stream (in-memory, not cell-loaded) | `{}` facts / net income `{}` |",
                self.synthetic_facts_generated, self.fpabench_total_net_income
            ),
            format!(
                "| Loaded Population (`cell_facts_loaded` / `olap_facts_loaded`) | `{}` / `{}` governed `PosSale` facts |",
                self.cell_facts_loaded, self.olap_facts_loaded
            ),
            format!(
                "| Exactness & Lineage Verified | exact=`{}` / lineage=`{}` |",
                self.exactness_verified, self.lineage_verified
            ),
            format!("| Live Process RSS (`VmRSS`, sampled pre-drop) | `{rss}` |"),
            format!(
                "| Governed Cell Ingest Throughput | `{} facts/s` ({} loaded) |",
                self.ingest_facts_per_sec, self.cell_facts_loaded
            ),
            format_latency_row("Incremental Commit Latency", &self.commit_latency),
            format_latency_row("Fork Branching Latency", &self.fork_branch_latency),
            format_query_row(
                "Exact Cell Query Latency",
                &self.exact_query_timing,
                self.cell_facts_loaded,
            ),
        ];
        if let Some(duck) = &self.duckdb_query_timing {
            lines.push(format_query_row(
                "DuckDB OLAP Query Latency",
                duck,
                self.olap_facts_loaded,
            ));
        }
        if let Some(df) = &self.datafusion_query_timing {
            lines.push(format_query_row(
                "DataFusion OLAP Query Latency",
                df,
                self.olap_facts_loaded,
            ));
        }
        lines.join("\n")
    }
}

type LoadedSalesOracle = BTreeMap<(String, String), (i128, i128)>;

/// Run the full Layer 5 benchmark harness at `scale` and `seed`, timing Exact
/// Cell and enabled OLAP engines over the exact same loaded population,
/// measuring cold vs warm query latencies separately, and sampling live
/// `VmRSS` before dropping engines.
///
/// # Errors
/// Returns an error if any workload generation, cell commit, fork, query, or
/// exactness/lineage verification fails.
pub fn run_benchmark_scorecard(scale: BenchScale, seed: u64) -> Result<BenchmarkScorecard, String> {
    let m5_summary = run_m5_synthetic_workload(seed, M5ShapeConfig::for_scale(scale))?;
    let fpa_cfg = FpaBenchConfig::for_scale(scale)?;
    let (synthetic_facts_generated, fpabench_net_scaled) = run_fpabench_pnl_stream(seed, fpa_cfg)?;
    let (cell, loaded_oracle, ingest_facts_per_sec, commit_latency) =
        benchmark_cell_ingest(seed, fpa_cfg)?;
    let fork_branch_latency = benchmark_fork_branching(&cell, fpa_cfg.fork_branches)?;

    let horizon_weeks = u32::try_from(fpa_cfg.weeks.max(1)).unwrap_or(52);
    let member_bound =
        u32::try_from(fpa_cfg.cell_sales_commits.saturating_add(1_000)).unwrap_or(u32::MAX);
    let plan = cell.pack.retail_question_for_weeks(
        "supply.retail.store_gross_margin_rate_weekly",
        horizon_weeks,
        member_bound,
    )?;
    let (exact_query_timing, exact_result) =
        benchmark_exact_queries(&cell, &plan, fpa_cfg.query_iterations)?;
    verify_loaded_exactness_and_lineage(&exact_result, &loaded_oracle, fpa_cfg.cell_sales_commits)?;
    let mut peak_live_rss_kib = read_process_rss_kib();

    let (duckdb_query_timing, duck_live_rss) =
        benchmark_duckdb_queries(&cell, &plan, fpa_cfg.query_iterations, &exact_result)?;
    peak_live_rss_kib = max_opt_u64(peak_live_rss_kib, duck_live_rss);

    let (datafusion_query_timing, df_live_rss) =
        benchmark_datafusion_queries(&cell, &plan, fpa_cfg.query_iterations, &exact_result)?;
    peak_live_rss_kib = max_opt_u64(peak_live_rss_kib, df_live_rss);

    let olap_facts_loaded = if duckdb_query_timing.is_some() || datafusion_query_timing.is_some() {
        fpa_cfg.cell_sales_commits
    } else {
        0
    };
    peak_live_rss_kib = max_opt_u64(peak_live_rss_kib, read_process_rss_kib());
    drop(cell);

    Ok(BenchmarkScorecard {
        scale: scale.as_str().to_owned(),
        seed,
        evidence_class: "sampled".to_owned(),
        m5_summary,
        synthetic_facts_generated,
        fpabench_total_net_income: decimal_from_scaled(fpabench_net_scaled)?.canonical(),
        cell_facts_loaded: fpa_cfg.cell_sales_commits,
        olap_facts_loaded,
        exactness_verified: true,
        lineage_verified: true,
        rss_kib: peak_live_rss_kib,
        ingest_facts_per_sec,
        commit_latency,
        fork_branch_latency,
        exact_query_timing,
        duckdb_query_timing,
        datafusion_query_timing,
    })
}

const fn max_opt_u64(first: Option<u64>, second: Option<u64>) -> Option<u64> {
    match (first, second) {
        (Some(lhs), Some(rhs)) => Some(if lhs >= rhs { lhs } else { rhs }),
        (Some(val), None) | (None, Some(val)) => Some(val),
        (None, None) => None,
    }
}

fn verify_loaded_exactness_and_lineage(
    result: &QueryResult,
    oracle: &LoadedSalesOracle,
    expected_facts: usize,
) -> Result<(), String> {
    if result.lineage_ref.trim().is_empty() {
        return Err("benchmark QueryResult missing lineage_ref".to_owned());
    }
    if result.rows.len() != oracle.len() {
        return Err(format!(
            "benchmark bucket count mismatch: actual={} oracle={}",
            result.rows.len(),
            oracle.len()
        ));
    }
    for ((store_id, week), (sum_gm, sum_rev)) in oracle {
        let actual = bucket_decimal_exact(result, &["EGP", store_id, "costed", week])
            .ok_or_else(|| format!("missing benchmark bucket ({store_id}, {week})"))?;
        let expected_scaled =
            independent_ratio_scaled(*sum_gm, *sum_rev).ok_or("benchmark oracle ratio None")?;
        let expected = decimal_from_scaled(expected_scaled)?;
        if actual != expected {
            return Err(format!(
                "benchmark exactness mismatch on ({store_id},{week}): actual={} expected={}",
                actual.canonical(),
                expected.canonical()
            ));
        }
    }
    let total_members: usize = result
        .rows
        .values()
        .map(|row| {
            row.member_count
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(row.members.len())
        })
        .sum();
    if total_members != expected_facts {
        return Err(format!(
            "benchmark lineage member count mismatch: actual={total_members} expected={expected_facts}"
        ));
    }
    Ok(())
}

fn seed_benchmark_catalog(
    seed: u64,
    store_count: usize,
    sku_count: usize,
    week_count: usize,
) -> Result<EntityCell, String> {
    let clock_ms = week_epoch_ms(u32::try_from(week_count).unwrap_or(52), 1, 12);
    let mut cell = EntityCell::open(
        &cell_id_for_entity(seed, 500),
        "LE-BENCH",
        "EGP",
        "org_xpa_bench",
        "prod",
        clock_ms,
    )?;
    cell.register_book("xP&A Benchmark Book", "corporation")?;
    for sku_ordinal in 0..sku_count {
        let sku = format!("SKU-B-{sku_ordinal}");
        cell.register_sku(&sku, "dry_goods", "each")?;
        cell.record_unit_cost(
            &sku,
            &format!("COST-B-{sku_ordinal}"),
            "SUP-BENCH",
            "100",
            "2026-07-01T00:00:00Z",
        )?;
    }
    for store_ordinal in 0..store_count {
        let store = format!("ST-B-{store_ordinal}");
        cell.register_store(&store, &store, "CUST-B", "mall_store", Some("MALL-CFC"))?;
    }
    Ok(cell)
}

fn benchmark_cell_ingest(
    seed: u64,
    cfg: FpaBenchConfig,
) -> Result<(EntityCell, LoadedSalesOracle, u64, LatencySummary), String> {
    let store_count = cfg.stores.max(1);
    let sku_count = cfg.skus.max(1);
    let week_count = cfg.weeks.max(1);
    let mut cell = seed_benchmark_catalog(seed, store_count, sku_count, week_count)?;

    let mut rng = SplitMix64::new(seed).fork(0xBE4C_0001);
    let mut latencies = Vec::with_capacity(cfg.cell_sales_commits);
    let mut oracle: LoadedSalesOracle = BTreeMap::new();
    let ingest_start = Instant::now();
    for idx in 0..cfg.cell_sales_commits {
        let store_ordinal = rem_usize(idx, store_count);
        let sku_ordinal = rem_usize(div_usize(idx, store_count), sku_count);
        let week = u32::try_from(rem_usize(
            div_usize(idx, store_count.saturating_mul(sku_count).max(1)),
            week_count,
        ))
        .unwrap_or(0);
        let day = u32::try_from(rem_usize(idx, 6)).unwrap_or(0);
        let qty = rng.range_u64(1, 10);
        let price = rng.range_u64(140, 240);
        let rev = qty.saturating_mul(price);
        let gm = qty.saturating_mul(price.saturating_sub(100));
        let store_id = format!("ST-B-{store_ordinal}");
        let week_iso = week_monday_iso(week)?;
        let sold_at = week_timestamp_utc(week, day, 12)?;
        let started = Instant::now();
        cell.record_costed_sale(
            &format!("SALE-B-{idx}"),
            &store_id,
            &format!("SKU-B-{sku_ordinal}"),
            &qty.to_string(),
            "each",
            &rev.to_string(),
            "100",
            &format!("COST-B-{sku_ordinal}"),
            &sold_at,
        )?;
        latencies.push(started.elapsed());
        let entry = oracle.entry((store_id, week_iso)).or_insert((0, 0));
        entry.0 = entry
            .0
            .checked_add(scaled(i128::from(gm)))
            .ok_or("bench oracle gm overflow")?;
        entry.1 = entry
            .1
            .checked_add(scaled(i128::from(rev)))
            .ok_or("bench oracle rev overflow")?;
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
    Ok((
        cell,
        oracle,
        u64::try_from(throughput).unwrap_or(u64::MAX),
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
        let started = Instant::now();
        let forked: CellStore = cell
            .cell()?
            .store()
            .fork(&world, &head)
            .map_err(|e| format!("bench fork {world}: {e}"))?;
        latencies.push(started.elapsed());
        drop(forked);
    }
    Ok(LatencySummary::from_durations(&latencies))
}

fn benchmark_exact_queries(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    iterations: usize,
) -> Result<(EngineQueryTiming, QueryResult), String> {
    let ctx = cell.query_context();
    let cold_engine = NativeEngine::open(&cell.db_path, StoreOptions::default())
        .map_err(|e| format!("open cold reader: {e}"))?;
    let cold_store =
        CellStore::from_engine_bound(cold_engine, cell.pack.model().clone(), &cell.cell_id)
            .map_err(|e| format!("bind cold reader: {e}"))?;
    let cold_start = Instant::now();
    let cold_result = cold_store
        .evaluate_question(plan, ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("bench cold exact query: {e}"))?;
    let cold_query_us = u64::try_from(cold_start.elapsed().as_micros()).unwrap_or(u64::MAX);
    drop(cold_store);

    let mut latencies = Vec::with_capacity(iterations);
    let mut last_result = Some(cold_result);
    for _ in 0..iterations {
        let started = Instant::now();
        let res = cell
            .cell()?
            .store()
            .evaluate_question(plan, ctx.clone(), |_| Ok(true))
            .map_err(|e| format!("bench warm exact query: {e}"))?;
        latencies.push(started.elapsed());
        last_result = Some(res);
    }
    let result = last_result.ok_or("zero exact query iterations")?;
    Ok((
        EngineQueryTiming {
            cold_query_us,
            warm: LatencySummary::from_durations(&latencies),
        },
        result,
    ))
}

fn run_fpabench_pnl_stream(seed: u64, cfg: FpaBenchConfig) -> Result<(u64, i128), String> {
    let mut rng = SplitMix64::new(seed).fork(0xF9AB_E4C4);
    let mut facts: u64 = 0;
    let mut net_income_scaled: i128 = 0;
    for _store in 0..cfg.stores {
        let store_rent_weekly = scaled(i128::from(rng.range_u64(800, 3_500)));
        for _week in 0..cfg.weeks {
            let mut weekly_gross_margin: i128 = 0;
            for _sku in 0..cfg.skus {
                let qty = i128::from(rng.range_u64(1, 25));
                let price = scaled(i128::from(rng.range_u64(50, 350)));
                let cost = scaled(i128::from(rng.range_u64(25, 200)));
                let gm = qty
                    .saturating_mul(price)
                    .checked_sub(qty.saturating_mul(cost))
                    .ok_or("fpa gm underflow")?;
                weekly_gross_margin = weekly_gross_margin
                    .checked_add(gm)
                    .ok_or("fpa weekly_gross_margin overflow")?;
                facts = facts.saturating_add(1);
            }
            let weekly_net_income = weekly_gross_margin
                .checked_sub(store_rent_weekly)
                .ok_or("fpa net income underflow")?;
            net_income_scaled = net_income_scaled
                .checked_add(weekly_net_income)
                .ok_or("fpa total net income overflow")?;
        }
    }
    Ok((facts, net_income_scaled))
}

fn read_process_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

#[cfg(any(feature = "duckdb", feature = "datafusion"))]
fn time_olap_cold_and_warm(
    label: &str,
    expected_engine: ComputeEngine,
    iterations: usize,
    exact_ref: &QueryResult,
    mut run_once: impl FnMut() -> Result<QueryResult, String>,
) -> Result<(EngineQueryTiming, Option<u64>), String> {
    let cold_start = Instant::now();
    let cold_ans = run_once()?;
    let cold_query_us = u64::try_from(cold_start.elapsed().as_micros()).unwrap_or(u64::MAX);
    assert_query_result_parity(
        &format!("{label}/cold"),
        exact_ref,
        &cold_ans,
        expected_engine,
    )?;

    let mut samples = Vec::with_capacity(iterations);
    let mut last_olap = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let ans = run_once()?;
        samples.push(started.elapsed());
        last_olap = Some(ans);
    }
    if let Some(olap) = last_olap {
        assert_query_result_parity(label, exact_ref, &olap, expected_engine)?;
    }
    // Sample live RSS while OLAP engine is still resident in caller's scope.
    let live_rss_kib = read_process_rss_kib();
    Ok((
        EngineQueryTiming {
            cold_query_us,
            warm: LatencySummary::from_durations(&samples),
        },
        live_rss_kib,
    ))
}

#[cfg(feature = "duckdb")]
fn benchmark_duckdb_queries(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    iterations: usize,
    exact_ref: &QueryResult,
) -> Result<(Option<EngineQueryTiming>, Option<u64>), String> {
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
    let (timing, live_rss) = time_olap_cold_and_warm(
        "bench/duckdb",
        ComputeEngine::Duckdb,
        iterations,
        exact_ref,
        || {
            let prepared = store
                .analytics_prepare(plan, ctx.clone(), &tenant)
                .map_err(|e| format!("duck prepare: {e}"))?;
            let response = duck
                .evaluate(prepared.request())
                .map_err(|e| format!("duck eval: {} {}", e.code, e.detail))?;
            store
                .analytics_answer(prepared, response)
                .map_err(|e| format!("duck answer: {e}"))
        },
    )?;
    drop(duck);
    Ok((Some(timing), live_rss))
}

#[cfg(not(feature = "duckdb"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "matches feature-enabled DuckDB benchmark signature"
)]
fn benchmark_duckdb_queries(
    _cell: &EntityCell,
    _plan: &norn_checker::metric::compute::QuestionPlan,
    _iterations: usize,
    _exact_ref: &QueryResult,
) -> Result<(Option<EngineQueryTiming>, Option<u64>), String> {
    Ok((None, None))
}

#[cfg(feature = "datafusion")]
fn benchmark_datafusion_queries(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    iterations: usize,
    exact_ref: &QueryResult,
) -> Result<(Option<EngineQueryTiming>, Option<u64>), String> {
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
    let (timing, live_rss) = time_olap_cold_and_warm(
        "bench/datafusion",
        ComputeEngine::DataFusion,
        iterations,
        exact_ref,
        || {
            let prepared = store
                .analytics_prepare(plan, ctx.clone(), &tenant)
                .map_err(|e| format!("df prepare: {e}"))?;
            let response = df
                .evaluate(prepared.request())
                .map_err(|e| format!("df eval: {} {}", e.code, e.detail))?;
            store
                .analytics_answer(prepared, response)
                .map_err(|e| format!("df answer: {e}"))
        },
    )?;
    drop(df);
    Ok((Some(timing), live_rss))
}

#[cfg(not(feature = "datafusion"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "matches feature-enabled DataFusion benchmark signature"
)]
fn benchmark_datafusion_queries(
    _cell: &EntityCell,
    _plan: &norn_checker::metric::compute::QuestionPlan,
    _iterations: usize,
    _exact_ref: &QueryResult,
) -> Result<(Option<EngineQueryTiming>, Option<u64>), String> {
    Ok((None, None))
}
