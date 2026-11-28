# `xpa-dst-v1` — Whole-Business xP&A DST, Stress-Test & Benchmark Matrix

Reproducible, seeded Deterministic Simulation Testing (DST), differential OLAP
parity, scenario-fork isolation, and synthetic scale benchmark suite over Norn's
embedded core (`norn-cell`, `norn-kernel`, `norn-checker`) and the declared
`registry/packs/supply-lab/` retail ontology pack (Issue #1965, Part of #1950).

All tests run inside `crates/norn-cell/tests/suites.rs` under module
`xpa_dst_harness::` (helpers in `crates/norn-cell/tests/xpa_dst/`), with zero
domain opcodes in the kernel and zero GPL/AGPL dependencies.

## Honest Evidence Labeling

| Layer | Suite / Target | Evidence Class | What Is Checked | What Is NOT Claimed / Unavailable Seams |
|---|---|---|---|---|
| **Layer 1** | `xpa_dst_harness::layer1_*` | `structural`, `runtime-enforced`, `sampled` | `structural` + `runtime-enforced`: `METRIC-UNIT-PARTITION-1` checker admission refusals (`mixed_currency`, `mixed_unit`); real governed cell KPI ratio-of-sums evaluation (`gross_margin_rate`, `sell_through`, `WOS`, `GMROI`, proving ratio-of-sums != average-of-ratios); UTC ISO-week Monday binding (`period_week` <-> `period_end`); future-cost & count-cutoff refusals; `ASOF-KNOWLEDGE-1` bitemporal recount preservation & `TemporalLeakage` refusal; single-book-per-cell refusal & read-only `CombinedViewReport` (`READ-LOCAL-1`). `sampled` (test-harness model): independent integer ratio oracle over 64 vectors; test-harness 4-tier `OperatingGroup` projection ladder (`side_by_side` -> `fx_translated` -> `inter_entity_netted` -> `statutory_consolidated`) bound fail-closed to real `CellCutSnapshot` pins (`query_pin`, `definition_pin`, `read_token`) and cryptographically verified `main` [`Receipt`]s. | Not a machine-checked Lean/Kani theorem over all possible pack definitions. Production `OperatingGroup` FX/inter-entity/statutory consolidation kernel & pack seams are **`unknown` / pending S4** (`evaluate_operating_group_tier` is a test-harness model over real cell cuts). |
| **Layer 2** | `xpa_dst_harness::layer2_*` | `sampled`, `runtime-enforced` | Seeded `SplitMix64` multi-cell simulator with explicit `(seed, profile)` replay binding (`XPA_DST_SEED` + `XPA_DST_PROFILE`) driving 16–52 simulated weeks across 3–4 legal-entity cells (POS sales, receipts, cycle counts, durable late-arriving cost restatements via `supply.unit_cost.record` + restated lots updating active costs, bilateral inter-entity transfers committing real seller sale + buyer lot receipt with verified `main` receipts, and pinned reporting-view FX shifts evaluated over real multi-cell combined views) checked against an independent exact arithmetic oracle over the **full** 16–52 week horizon (complete bucket-set and lot-set equality); adversarial fault schedules (out-of-order & exact duplicate delivery across **all 4** retail feeds, damaged receipt quarantine & remediation, stale watermark heartbeats, observed-vs-consumed cursor drift, dropped feed partition, authority mismatch, substituted question plan, **real** concurrent SQLite commit during `evaluate_cell_cut` -> `core.compute.stale_input`, uncosted SKU mid-window withholding while keeping unaffected stores exact). | Sampled finite schedules; does not replace formal liveness or crash-recovery proofs over arbitrary disk faults (see `cell_host_dst` for SQLite fault-engine tests). `supply-lab` has no `FxRate` cell entity yet, so FX rate tables are explicitly pinned reporting-view inputs (never claimed as cell writes). |
| **Layer 3** | `xpa_dst_harness::layer3_*` & `dataset_laws::xpa_dst_layer3_*` | `sampled` | Feature-gated (`--features duckdb` and `--features datafusion`) differential parity comparing Exact Cell evaluator vs DuckDB and DataFusion across all 7 declared retail questions (with nonempty seeded `StoreCommercialRollup` rows so WOS and GMROI are strictly nonempty), the full pack-authored `retail.coverage.json` combined-view spec (honoring spec-projected `plan` and `context`), and declared retail transforms (`VIEW-ENGINE-1`), asserting **complete envelope equality** normalizing only `engine`. Wired into `dataset_laws.rs` so CI's `duckdb` phase runs DuckDB parity automatically. | Differential testing over seeded states, not a symbolic SQL/DataFusion compiler equivalence proof. |
| **Layer 4** | `xpa_dst_harness::layer4_*` | `runtime-enforced`, `sampled` | `runtime-enforced` + `sampled`: concurrent what-if forks using real E1–E4 APIs (`CellStore::fork`, `evaluate_question_in_fork`, `compare_main_and_fork`, `compare_forks`, `ForkScenario`, `execute_action_in_fork`, `promote_decision_fork`, `export_fork_commit_evidence`, `verify_fork_commit_evidence`, `verify_main_receipt`, `verify_main_export_record`): pricing promo, tariff/FX shock, overtime shift reallocation asserting real simulated W9/W10 KPI deltas across virtual `ScenarioClock` advance, and new store opening + nested sub-fork refusal (`NestedForkForbidden`); `FORK-NONINTERFERE` byte-for-byte `main` invariance; `FORK-EVIDENCE-1` zero main-authoritative evidence minted by forks; Egyptian family-business multi-entity scenario over real `CombinedViewReport` cuts and verified bilateral `main` receipts. `sampled` (test-harness model): topological exact `DriverTree` ripple convergence and cycle refusal (`CycleDetected`). | Does not constitute the target Lean `FORK-ISOLATE` meta-theorem. Production S2 OTB/Merchandising, S3 Production, S4 Statutory Finance, and S5 Workforce/Payroll seams are **`unknown` / pending S2–S5** (`DriverTree` is a test-harness model, never claimed as `runtime-enforced`). |
| **Layer 5** | `xpa_dst_harness::layer5_*` & `xpa_layer5_scale_benchmark` | `sampled`, `bounded` | Pure-Rust in-memory synthetic stream generators matching Walmart M5 shape (configurable up to `3,049 SKUs x 10 stores x 260 weeks`) and `FPABench` P&L streams (`synthetic_facts_generated`), **explicitly separated** from governed cell & OLAP timings over the **exact same loaded population** (`cell_facts_loaded` governed `PosSale` facts committed into `EntityCell` and mirrored identically as `olap_facts_loaded`), reporting exactness check (`exactness_verified`), lineage check (`lineage_verified`), nearest-rank p50/p95/p99 latencies, and process `VmRSS` (`rss_kib`). | Default CI executes the `smoke` profile only (`cell_facts_loaded = 24`, `query_iterations = 20`, `fork_branches = 10`). Synthetic stream totals (`synthetic_facts_generated` / M5 daily events) are **never** advertised as cell-loaded facts. Full-scale (`medium` / `full`) numbers are **not claimed** until executed on a dedicated Boat sandbox and recorded with commit SHA and hardware details. |

## Reproduction Commands

### Default CI Suite (Layers 1, 2, 4, and Layer 5 Smoke)

```bash
cargo test -p norn-cell --test suites xpa_dst_harness::
```

### Deterministic Seed & Profile Replay

Replay any failing seed with its exact bound workload profile (`XPA_DST_SEED` preserves known default seed profiles automatically; pass `XPA_DST_PROFILE=multi_entity_16w|annual_52w` to override explicitly):

```bash
XPA_DST_SEED=0x5850415f44535402 XPA_DST_PROFILE=annual_52w cargo test -p norn-cell --test suites xpa_dst_harness::
```

### Layer 3 Differential OLAP Parity

```bash
# Exact Cell vs DuckDB (questions, full retail.coverage.json combined-view report, and transforms)
cargo test -p norn-cell --features duckdb --test suites xpa_dst_harness::layer3_exact_vs_duckdb_olap_differential_parity

# Exact Cell vs DataFusion (questions, full retail.coverage.json combined-view report, and transforms)
cargo test -p norn-cell --features datafusion --test suites xpa_dst_harness::layer3_exact_vs_datafusion_olap_differential_parity
```

### Layer 5 Scale & Latency Benchmark (Boat / Dedicated Sandbox)

Do **not** run `XPA_BENCH_SCALE=full` on shared development hosts; run it on a disposable Boat sandbox:

```bash
# Medium profile (200 governed cell/OLAP facts + ~12k FPABench synthetic stream facts)
XPA_BENCH_SCALE=medium cargo test -p norn-cell --release --test suites xpa_dst_harness::xpa_layer5_scale_benchmark -- --ignored --nocapture

# Full profile (2,000 governed cell/OLAP facts + full M5/FPABench synthetic streams; add --features duckdb,datafusion for OLAP timings on the exact same loaded population)
XPA_BENCH_SCALE=full cargo test -p norn-cell --release --features duckdb,datafusion --test suites xpa_dst_harness::xpa_layer5_scale_benchmark -- --ignored --nocapture
```
