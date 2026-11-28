# `xpa-dst-v1` — Whole-Business xP&A DST, Stress-Test & Benchmark Matrix

Reproducible, seeded Deterministic Simulation Testing (DST), differential OLAP
parity, scenario-fork isolation, and synthetic scale benchmark suite over Norn's
embedded core (`norn-cell`, `norn-kernel`, `norn-checker`) and the declared
`registry/packs/supply-lab/` retail ontology pack (Issue #1965, Part of #1950).

All tests run inside `crates/norn-cell/tests/suites.rs` under module
`xpa_dst_harness::` (helpers in `crates/norn-cell/tests/xpa_dst/`), with zero
domain opcodes in the kernel and zero GPL/AGPL dependencies.

## Honest Evidence Labeling

| Layer | Suite / Target | Evidence Class | What Is Checked | What Is NOT Claimed |
|---|---|---|---|---|
| **Layer 1** | `xpa_dst_harness::layer1_*` | `structural`, `runtime-enforced`, `sampled` | `METRIC-UNIT-PARTITION-1` checker admission refusals (`mixed_currency`, `mixed_unit`); checked 6-decimal `i128` arithmetic & ratio-of-sums exactness (`WOS`, `GMROI`, `gross_margin_rate`, `sell_through_rate`); UTC ISO-week Monday binding (`period_week` <-> `period_end`); future-cost & count-cutoff refusals; `ASOF-KNOWLEDGE-1` bitemporal recount preservation; single-book-per-cell refusal (`READ-LOCAL-1`); explicit 4-tier `OperatingGroup` ladder (`side_by_side` -> `fx_translated` -> `inter_entity_netted` -> `statutory_consolidated`) fail-closed refusals. | Not a machine-checked Lean/Kani theorem over all possible pack definitions; covers the declared `supply-lab` retail pack and 64 sampled arithmetic vectors per run. |
| **Layer 2** | `xpa_dst_harness::layer2_*` | `sampled`, `runtime-enforced` | Seeded SplitMix64 multi-cell simulator (`XPA_DST_SEED`) driving 16–52 simulated weeks across 2–10 legal-entity cells (POS sales, receipts, cycle counts, cost restatements, FX shifts, inter-entity events) checked against an independent exact oracle; adversarial fault schedules (out-of-order/duplicate feed delivery across all 4 retail feeds, damaged receipt quarantine & remediation, stale watermark heartbeats, observed-vs-consumed cursor drift, dropped feed partition, authority mismatch, substituted question plan, mid-read frontier shift -> `core.compute.stale_input`, uncosted SKU mid-window withholding while keeping unaffected stores exact). | Sampled finite schedules; does not replace formal liveness or crash-recovery proofs over arbitrary disk faults (see `cell_host_dst` for SQLite fault-engine tests). |
| **Layer 3** | `xpa_dst_harness::layer3_*` | `sampled` | Feature-gated (`--features duckdb` and `--features datafusion`) differential parity comparing Exact Cell evaluator vs DuckDB and DataFusion across all 7 declared retail questions, `evaluate_combined_management_view_from_spec` (`query_pin`, `revision`, `definition_pin`, `authority_pin`, `read_token`, `lineage_ref`, `row.members`, buckets), and declared retail transforms (`VIEW-ENGINE-1`). | Differential testing over seeded states, not symbolic SQL/DataFusion compiler equivalence proof. |
| **Layer 4** | `xpa_dst_harness::layer4_*` | `sampled`, `runtime-enforced` | Concurrent what-if forks using real E1–E4 APIs (`CellStore::fork`, `evaluate_question_in_fork`, `compare_main_and_fork`, `compare_forks`, `ForkScenario`, `execute_action_in_fork`, `promote_decision_fork`, `export_fork_commit_evidence`, `verify_fork_commit_evidence`, `verify_main_receipt`, `verify_main_export_record`): pricing promo, tariff/FX shock, new store opening + nested sub-fork refusal (`NestedForkForbidden`), overtime shift reallocation (`ScenarioClock` + deterministic `Entropy`); `FORK-NONINTERFERE` byte-for-byte `main` invariance; `FORK-EVIDENCE-1` zero main-authoritative evidence minted by forks; Egyptian family-business multi-entity scenario (sole prop EGP + partnership USD sharing custodian/supplier, bilateral transfer matching & unilateral refusal); topological exact `DriverTree` ripple convergence and cycle refusal (`CycleDetected`). | Runtime & sampled evidence over E1–E4 APIs; does not constitute the target Lean `FORK-ISOLATE` meta-theorem. |
| **Layer 5** | `xpa_dst_harness::layer5_*` & `xpa_layer5_scale_benchmark` | `sampled`, `bounded` | Pure-Rust synthetic generators matching Walmart M5 hierarchical retail shape (configurable up to `3,049 SKUs × 10 stores × 260 weeks`) and FPABench multidimensional P&L recalculation (`smoke` / `medium` / `full`), emitting JSON & Markdown scorecards for ingest throughput, incremental commit p50/p95/p99, fork-branching p50/p95/p99, and exact-vs-OLAP query p50/p95/p99. | Default CI executes the `smoke` profile only. Full-scale (`medium` / `full`, 100k–1M+ facts) numbers are **not claimed** until executed on a dedicated Boat sandbox and recorded with commit SHA and hardware details. |

## Reproduction Commands

### Default CI Suite (Layers 1, 2, 4, and Layer 5 Smoke)

```bash
cargo test -p norn-cell --test suites xpa_dst_harness::
```

### Deterministic Seed Replay

Replay any failing seed immediately with `XPA_DST_SEED` (decimal or `0x`-prefixed hex):

```bash
XPA_DST_SEED=0x5850415f44535401 cargo test -p norn-cell --test suites xpa_dst_harness::
```

### Layer 3 Differential OLAP Parity

```bash
# Exact Cell vs DuckDB (questions, combined-view report, and transforms)
cargo test -p norn-cell --features duckdb --test suites xpa_dst_harness::layer3_exact_vs_duckdb_olap_differential_parity

# Exact Cell vs DataFusion (questions, combined-view report, and transforms)
cargo test -p norn-cell --features datafusion --test suites xpa_dst_harness::layer3_exact_vs_datafusion_olap_differential_parity
```

### Layer 5 Scale & Latency Benchmark (Boat / Dedicated Sandbox)

Do **not** run `XPA_BENCH_SCALE=full` on shared development hosts; run it on a disposable Boat sandbox:

```bash
# Medium profile (~10k generated facts)
XPA_BENCH_SCALE=medium cargo test -p norn-cell --release --test suites xpa_dst_harness::xpa_layer5_scale_benchmark -- --ignored --nocapture

# Full profile (M5 3,049 SKUs × 10 stores × 260 weeks + 1M+ FPABench P&L facts; add --features duckdb or --features datafusion for OLAP timings)
XPA_BENCH_SCALE=full cargo test -p norn-cell --release --features duckdb,datafusion --test suites xpa_dst_harness::xpa_layer5_scale_benchmark -- --ignored --nocapture
```
