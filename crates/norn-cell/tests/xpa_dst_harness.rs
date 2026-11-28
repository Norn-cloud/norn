//! Whole-Business xP&A DST, Stress-Test & Benchmark Harness (Layers 1–5, #1965).
//!
//! Folded into `crates/norn-cell/tests/suites.rs` under `xpa_dst_harness::`.
//! Override the deterministic seed batch with `XPA_DST_SEED=<u64|0xHEX>` and
//! optionally `XPA_DST_PROFILE=multi_entity_16w|annual_52w` to replay any
//! failing simulation with the exact same workload.
//!
//! Honest evidence classes:
//! - Layer 1: `structural` + `runtime-enforced` (`METRIC-UNIT-PARTITION-1`,
//!   `ASOF-KNOWLEDGE-1`, `READ-LOCAL-1` single-book & combined views) + `sampled`
//!   (arithmetic oracle & test-harness 4-tier `OperatingGroup` projection model).
//! - Layer 2: `sampled` + `runtime-enforced` (full-horizon multi-cell DST & faults).
//! - Layer 3: `sampled` differential parity (feature-gated `duckdb` / `datafusion`).
//! - Layer 4: `runtime-enforced` + `sampled` (real E1–E4 fork APIs) + `sampled`
//!   (test-harness `DriverTree` ripple model; S2 OTB & S5 Workforce seams are
//!   `unknown` / pending S2–S5).
//! - Layer 5: `sampled` synthetic stream generators + governed cell/OLAP benchmark
//!   over identical loaded populations (`cell_facts_loaded` == `olap_facts_loaded`).

#![cfg(feature = "native")]

#[path = "xpa_dst/actions.rs"]
mod actions;
#[path = "xpa_dst/cuts.rs"]
mod cuts;
#[path = "xpa_dst/driver_tree.rs"]
mod driver_tree;
#[path = "xpa_dst/env.rs"]
mod env;
#[path = "xpa_dst/layer1_invariants.rs"]
mod layer1_invariants;
#[path = "xpa_dst/layer2_faults.rs"]
mod layer2_faults;
#[path = "xpa_dst/layer2_simulator.rs"]
mod layer2_simulator;
#[path = "xpa_dst/layer3_parity.rs"]
mod layer3_parity;
#[path = "xpa_dst/layer4_forks_xpa.rs"]
mod layer4_forks_xpa;
#[path = "xpa_dst/layer5_benchmarks.rs"]
mod layer5_benchmarks;
#[path = "xpa_dst/operating_group.rs"]
mod operating_group;
#[path = "xpa_dst/operating_group_transfers.rs"]
mod operating_group_transfers;
#[path = "xpa_dst/prng.rs"]
mod prng;

use std::io::Write as _;

use layer2_faults::run_adversarial_fault_suite;
use layer2_simulator::{SimConfig, run_seeded_multi_cell_sim};
use layer5_benchmarks::{BenchScale, run_benchmark_scorecard};
use prng::{
    PRIMARY_SEED, active_seeds, active_sim_runs, format_replay_err, verify_prng_and_percentile_laws,
};

// ------------------------------------------------------------------ Helpers

#[test]
fn prng_nearest_rank_percentiles_and_seed_profile_binding() -> Result<(), String> {
    verify_prng_and_percentile_laws()
}

// ------------------------------------------------------------------ Layer 1

#[test]
fn layer1_decimal_and_unit_partition_invariants() -> Result<(), String> {
    layer1_invariants::verify_decimal_and_unit_partition_laws()
}

#[test]
fn layer1_temporal_grain_and_bitemporal_restatement_laws() -> Result<(), String> {
    layer1_invariants::verify_temporal_grain_and_bitemporal_laws()
}

#[test]
fn layer1_single_book_and_operating_group_ladder() -> Result<(), String> {
    layer1_invariants::verify_single_book_and_operating_group_ladder()
}

// ------------------------------------------------------------------ Layer 2

#[test]
fn layer2_seeded_multi_cell_event_log_simulator() -> Result<(), String> {
    for (seed, profile) in active_sim_runs()? {
        let entity_count = profile.entity_count();
        let weeks = profile.weeks();
        let world = run_seeded_multi_cell_sim(SimConfig {
            seed,
            entity_count,
            weeks,
            stores_per_entity: 2,
            skus_per_entity: 2,
        })
        .map_err(|e| format_replay_err(seed, Some(profile), &e))?;
        if world.cells.len() != entity_count {
            return Err(format_replay_err(
                seed,
                Some(profile),
                &format!(
                    "expected {entity_count} entity cells, got {}",
                    world.cells.len()
                ),
            ));
        }
        if world.fx_history.is_empty() || world.inter_entity_transfers.is_empty() {
            return Err(format_replay_err(
                seed,
                Some(profile),
                "simulator must emit pinned FX snapshots and bilateral inter-entity transfers",
            ));
        }
    }
    Ok(())
}

#[test]
fn layer2_adversarial_fault_injection_suite() -> Result<(), String> {
    for seed in active_seeds()? {
        run_adversarial_fault_suite(seed).map_err(|e| format_replay_err(seed, None, &e))?;
    }
    Ok(())
}

// ------------------------------------------------------------------ Layer 3

/// Entrypoint also invoked by `dataset_laws::xpa_dst_layer3_duckdb_parity` so
/// CI's `duckdb` phase runs Layer 3 Exact-vs-`DuckDB` differential parity.
#[cfg(feature = "duckdb")]
pub(crate) fn run_layer3_duckdb_parity() -> Result<(), String> {
    let seed = active_seeds()?.into_iter().next().unwrap_or(PRIMARY_SEED);
    layer3_parity::run_duckdb_parity_suite(seed).map_err(|e| format_replay_err(seed, None, &e))
}

/// Entrypoint invoked by `xpa_dst_harness::layer3_exact_vs_datafusion_olap_differential_parity`
/// when `datafusion` is enabled (compiles and qualifies independently without `duckdb`).
#[cfg(feature = "datafusion")]
pub(crate) fn run_layer3_datafusion_parity() -> Result<(), String> {
    let seed = active_seeds()?.into_iter().next().unwrap_or(PRIMARY_SEED);
    layer3_parity::run_datafusion_parity_suite(seed).map_err(|e| format_replay_err(seed, None, &e))
}

#[cfg(feature = "duckdb")]
#[test]
fn layer3_exact_vs_duckdb_olap_differential_parity() -> Result<(), String> {
    run_layer3_duckdb_parity()
}

#[cfg(feature = "datafusion")]
#[test]
fn layer3_exact_vs_datafusion_olap_differential_parity() -> Result<(), String> {
    run_layer3_datafusion_parity()
}

// ------------------------------------------------------------------ Layer 4

#[test]
fn layer4_concurrent_what_if_forks_isolation_and_promotion() -> Result<(), String> {
    for seed in active_seeds()? {
        layer4_forks_xpa::run_concurrent_what_if_forks(seed)
            .map_err(|e| format_replay_err(seed, None, &e))?;
    }
    Ok(())
}

#[test]
fn layer4_egyptian_family_business_multi_entity_views() -> Result<(), String> {
    for seed in active_seeds()? {
        layer4_forks_xpa::run_egyptian_family_business_scenario(seed)
            .map_err(|e| format_replay_err(seed, None, &e))?;
    }
    Ok(())
}

#[test]
fn layer4_driver_tree_ripple_convergence_and_cycle_refusal() -> Result<(), String> {
    for seed in active_seeds()? {
        driver_tree::run_driver_tree_ripple_and_cycle_check(seed)
            .map_err(|e| format_replay_err(seed, None, &e))?;
    }
    Ok(())
}

// ------------------------------------------------------------------ Layer 5

#[test]
fn layer5_benchmark_generators_and_scorecard_smoke() -> Result<(), String> {
    let seed = active_seeds()?.into_iter().next().unwrap_or(PRIMARY_SEED);
    let scorecard = run_benchmark_scorecard(BenchScale::Smoke, seed)
        .map_err(|e| format_replay_err(seed, None, &e))?;
    if scorecard.evidence_class != "sampled"
        || scorecard.m5_summary.daily_pos_events == 0
        || scorecard.synthetic_facts_generated == 0
        || scorecard.cell_facts_loaded < 20
        || !scorecard.exactness_verified
        || !scorecard.lineage_verified
        || scorecard.exact_query_timing.warm.samples < 20
        || scorecard.fork_branch_latency.samples < 10
    {
        return Err(format_replay_err(
            seed,
            None,
            &format!("invalid smoke scorecard: {scorecard:?}"),
        ));
    }
    let json_text =
        serde_json::to_string_pretty(&scorecard).map_err(|e| format!("scorecard json: {e}"))?;
    let markdown = scorecard.render_markdown();
    if !json_text.contains("\"evidenceClass\": \"sampled\"")
        || !json_text.contains("\"cellFactsLoaded\":")
        || !json_text.contains("\"olapFactsLoaded\":")
        || !json_text.contains("\"syntheticFactsGenerated\":")
        || !json_text.contains("\"coldQueryUs\":")
        || !markdown.contains("xP&A Layer 5 Benchmark Scorecard")
    {
        return Err(format_replay_err(
            seed,
            None,
            "scorecard serialization missing required fields",
        ));
    }
    Ok(())
}

/// Full-scale Boat/CI benchmark entrypoint (`XPA_BENCH_SCALE=smoke|medium|full`,
/// `XPA_DST_SEED=<u64|0xHEX>`). Marked `#[ignore]` so default CI test shards stay
/// fast and shared dev hosts are never overloaded.
#[test]
#[ignore = "Boat/CI full-scale xP&A benchmark; run with -- --ignored --nocapture and XPA_BENCH_SCALE=medium|full"]
fn xpa_layer5_scale_benchmark() -> Result<(), String> {
    let scale = BenchScale::from_env(BenchScale::Medium)?;
    let seed = active_seeds()?.into_iter().next().unwrap_or(PRIMARY_SEED);
    let scorecard =
        run_benchmark_scorecard(scale, seed).map_err(|e| format_replay_err(seed, None, &e))?;
    let json_text =
        serde_json::to_string_pretty(&scorecard).map_err(|e| format!("scorecard json: {e}"))?;
    let markdown = scorecard.render_markdown();
    let mut out = std::io::stdout();
    writeln!(out, "{markdown}").map_err(|e| format!("write markdown: {e}"))?;
    writeln!(out, "```json\n{json_text}\n```").map_err(|e| format!("write json: {e}"))?;
    Ok(())
}
