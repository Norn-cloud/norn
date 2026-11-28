# `xpa-dst-v1` — Whole-Business xP&A DST, Stress-Test & Benchmark Matrix

Reproducible, seeded Deterministic Simulation Testing (DST), differential OLAP
parity, scenario-fork isolation, and synthetic scale benchmark suite over Norn's
embedded core (`norn-cell`, `norn-kernel`, `norn-checker`) and the declared
`registry/packs/supply-lab/` retail ontology pack (Issue #1965, Part of #1950).

All tests run inside `crates/norn-cell/tests/suites.rs` under module
`xpa_dst_harness::` (helpers in `crates/norn-cell/tests/xpa_dst/`), with zero
domain opcodes in the kernel and zero GPL/AGPL dependencies.

## Honest Evidence Labeling

### Executed in Default CI vs. Wired/Pending Lanes

- **Executed automatically in default CI (`core` + `bundle` / `duckdb`):**
  - **Layer 1** (`xpa_dst_harness::layer1_*`): `structural`, `runtime-enforced`, `sampled`
  - **Layer 2** (`xpa_dst_harness::layer2_*`): `sampled`, `runtime-enforced`
  - **Layer 3 DuckDB** (`dataset_laws::xpa_dst_layer3_duckdb_parity` & `xpa_dst_harness::layer3_exact_vs_duckdb_olap_differential_parity`): `sampled` over all 13/13 retail questions, all 9/9 retail transforms, and `retail.coverage.json`
  - **Layer 4** (`xpa_dst_harness::layer4_*`): `runtime-enforced`, `sampled`
  - **Layer 5 Smoke** (`xpa_dst_harness::layer5_benchmark_generators_and_scorecard_smoke`): `sampled`, `bounded` (`smoke` profile only: `cell_facts_loaded = 24`, `query_iterations = 20`, `fork_branches = 10`)
- **Wired under `--features datafusion` (`unknown` in default CI until a DataFusion CI lane runs it):**
  - **Layer 3 DataFusion** (`xpa_dst_harness::layer3_exact_vs_datafusion_olap_differential_parity`): wired independently of `duckdb` for execution via `cargo test -p norn-cell --features datafusion --test suites xpa_dst_harness::layer3_exact_vs_datafusion_olap_differential_parity`. Currently **`unknown`** in default CI because no default CI lane builds or executes `--features datafusion` yet.
- **Pending dedicated Boat sandbox execution (`unknown` until run on Boat with commit SHA & hardware details; tracked under `Refs #1965`):**
  - **Layer 5 Full-Scale Comparative Scorecard** (`xpa_dst_harness::xpa_layer5_scale_benchmark`, marked `#[ignore]`, `XPA_BENCH_SCALE=medium|full` with live pre-drop `VmRSS` and separate cold vs. warm query latency percentiles). Single-question governed cell sweeps are bounded by the `norn.limits/1` `QuestionIR` profile limit (`100,000` scanned records; `XPA_BENCH_FACTS > 100,000` refuses fail-closed upfront rather than silently exceeding the limit). Full `1M+` partitioned / OLAP scale sweeps and external-reference comparative scorecards remain open under **`#1965`** on Boat.

### Per-Layer Evidence Matrix

| Layer | Suite / Target | Evidence Class | What Is Checked | What Is NOT Claimed / Unavailable Seams |
|---|---|---|---|---|
| **Layer 1** | `xpa_dst_harness::layer1_*` | `structural`, `runtime-enforced`, `sampled` | `structural` + `runtime-enforced`: `METRIC-UNIT-PARTITION-1` checker admission refusals (`mixed_currency`, `mixed_unit`); real governed cell KPI ratio-of-sums evaluation (`gross_margin_rate`, `sell_through`, `WOS`, `GMROI`, proving ratio-of-sums != average-of-ratios); UTC ISO-week Monday binding (`period_week` <-> `period_end`); future-cost & count-cutoff refusals; `ASOF-KNOWLEDGE-1` bitemporal recount preservation & `TemporalLeakage` refusal; single-book-per-cell refusal & read-only `CombinedViewReport` (`READ-LOCAL-1`). `sampled` (test-harness model): independent integer ratio oracle over 64 vectors; test-harness 4-tier `OperatingGroup` projection ladder (`side_by_side` -> `fx_translated` -> `inter_entity_netted` -> `statutory_consolidated`) re-verifying every entity slice directly against the owning cell's ledger at `cut_revision` (`evaluate_question_known_at` + `receipt_hash_at`), refusing mutated `QueryResult` rows, verifying bilateral `main` [`ExportRecord`] ledger membership against the owning cell at `commit_seq`, enforcing shared committed transfer identity on BOTH seller and buyer legs, and refusing replayed/reused transfer legs (`duplicate_transfer_leg`). | Not a machine-checked Lean/Kani theorem over all possible pack definitions. Production `OperatingGroup` FX/inter-entity/statutory consolidation kernel & pack seams are **`unknown` / pending S4** (`evaluate_operating_group_tier` is a test-harness model over real cell cuts). |
| **Layer 2** | `xpa_dst_harness::layer2_*` | `sampled`, `runtime-enforced` | Seeded `SplitMix64` multi-cell simulator with explicit `(seed, profile)` replay binding (`XPA_DST_SEED` + `XPA_DST_PROFILE`) driving 16–52 simulated weeks across 3–4 legal-entity cells (POS sales, receipts, cycle counts, genuine **in-place** `update_only` restatements of existing committed facts — `supply.position.restate` on existing `InventorySnapshot`, `supply.stock_lot.update_balance`, `supply.store_count.recount`, `supply.store_kpi.refresh` on existing `StoreCommercialRollup`, and in-place `UnitCostEvidence` feed restatements on the same `cost_id` with restated `cost_ref`/`unit_cost` routed into subsequent sales — verified bitemporally via `evaluate_question_known_at` on the exact same object ID and week bucket; bilateral inter-entity transfers committing real seller `PosSale` + buyer `StockLot` bound by shared transfer correlation keys and owning-cell ledger membership; pinned reporting-view FX shifts evaluated over real multi-cell combined views) checked against an independent exact arithmetic oracle over the **full** 16–52 week horizon; adversarial fault schedules (out-of-order & exact duplicate delivery across **all 4** retail feeds, damaged receipt quarantine & remediation, stale watermark heartbeats, observed-vs-consumed cursor drift, dropped feed partition, authority mismatch, substituted question plan, **real** capture-to-evaluation concurrent SQLite commit interleavings -> `core.compute.stale_input`, uncosted SKU mid-window withholding while keeping unaffected stores exact). | Sampled finite schedules; does not replace formal liveness or crash-recovery proofs over arbitrary disk faults (see `cell_host_dst` for SQLite fault-engine tests). `supply-lab` has no `FxRate` cell entity yet, so FX rate tables are explicitly pinned reporting-view inputs (never claimed as cell writes). |
| **Layer 3** | `xpa_dst_harness::layer3_*` & `dataset_laws::xpa_dst_layer3_duckdb_parity` | `sampled` (DuckDB in CI), `unknown` (DataFusion in default CI) | Feature-gated (`--features duckdb` executed in CI `duckdb` phase; `--features datafusion` wired independently of `duckdb` and `unknown` in default CI until a DataFusion CI lane runs it) differential parity comparing Exact Cell (`ComputeEngine::Cell`) vs DuckDB and DataFusion across **all 13/13** declared retail questions in `retail.questions.json` (with nonempty seeded rows including `StoreCommercialRollup` so WOS and GMROI never pass vacuously), the full pack-authored `retail.coverage.json` combined-view spec (honoring spec-projected `plan` and `context`), and **all 9/9** declared retail transforms (`VIEW-ENGINE-1`), asserting **complete envelope equality** normalizing only `engine`. | Differential testing over seeded states, not a symbolic SQL/DataFusion compiler equivalence proof. DataFusion parity is **`unknown` in default CI** until executed in a dedicated `--features datafusion` CI lane or Boat run. |
| **Layer 4** | `xpa_dst_harness::layer4_*` | `runtime-enforced`, `sampled` | `runtime-enforced` + `sampled`: concurrent what-if forks using real E1–E4 APIs (`CellStore::fork`, `evaluate_question_in_fork`, `compare_main_and_fork`, `compare_forks`, `ForkScenario`, `execute_action_in_fork`, `promote_decision_fork`, `export_fork_commit_evidence`, `verify_fork_commit_evidence`, `verify_main_receipt`, `verify_main_export_record`): pricing promo, tariff/FX shock, parameter-driven overtime shift reallocation (`12h` vs `24h` driving W9/W10 KPI deltas directly from `ForkScenario` parameters across virtual `ScenarioClock` advance), and new store opening + nested sub-fork refusal (`NestedForkForbidden`); `FORK-NONINTERFERE` byte-for-byte `main` invariance; `FORK-EVIDENCE-1` zero main-authoritative evidence minted by forks; Egyptian family-business multi-entity scenario over real `CombinedViewReport` cuts, ledger-bound bilateral `main` `ExportRecord`s with shared transfer correlation IDs, unrelated same-amount refusal, and duplicate-leg refusal. `sampled` (test-harness model): topological exact `DriverTree` ripple convergence and cycle refusal (`CycleDetected`). | Does not constitute the target Lean `FORK-ISOLATE` meta-theorem. Production S2 OTB/Merchandising, S3 Production, S4 Statutory Finance, and S5 Workforce/Payroll seams are **`unknown` / pending S2–S5** (`DriverTree` is a test-harness model, never claimed as `runtime-enforced`). |
| **Layer 5** | `xpa_dst_harness::layer5_*` & `xpa_layer5_scale_benchmark` | `sampled`, `bounded` | Pure-Rust in-memory synthetic stream generators matching Walmart M5 shape (configurable up to `3,049 SKUs x 10 stores x 260 weeks`) and `FPABench` P&L streams (`synthetic_facts_generated`), **explicitly separated** from governed cell & OLAP timings over the **exact same loaded population** (`cell_facts_loaded` governed `PosSale` facts committed into `EntityCell` and mirrored identically as `olap_facts_loaded`, defaulting to `26,000` in `full` scale and configurable via `XPA_BENCH_FACTS` up to the governed `norn.limits/1` single-question scan bound of `100,000` records, refusing fail-closed above it), reporting exactness check (`exactness_verified`), lineage check (`lineage_verified`), dedicated `cold_query_us` (fresh reader / freshly synced engine) separate from warm `p50`/`p95`/`p99` latencies, and live pre-drop process `VmRSS` (`rss_kib`). | Default CI executes the `smoke` profile only (`cell_facts_loaded = 24`, `query_iterations = 20`, `fork_branches = 10`). Synthetic stream totals (`synthetic_facts_generated` / M5 daily events) are **never** advertised as cell-loaded facts. Single-question governed cell sweeps are bounded at `100,000` records by `norn.limits/1`; full `1M+` partitioned / OLAP scale sweeps and external comparative scorecards remain **`unknown` / open under `#1965` (`Refs #1965`)** until executed on a dedicated Boat sandbox and recorded with commit SHA and hardware details. |

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

### Layer 3 Differential OLAP Parity (13/13 Questions, 9/9 Transforms, Combined View)

```bash
# Exact Cell vs DuckDB
cargo test -p norn-cell --features duckdb --test suites xpa_dst_harness::layer3_exact_vs_duckdb_olap_differential_parity

# Exact Cell vs DataFusion (independent of DuckDB; currently `unknown` in default CI)
cargo test -p norn-cell --features datafusion --test suites xpa_dst_harness::layer3_exact_vs_datafusion_olap_differential_parity
```

### Layer 5 Scale & Latency Benchmark (Boat / Dedicated Sandbox)

Do **not** run `XPA_BENCH_SCALE=full` on shared development hosts; run it on a disposable Boat sandbox. Single-question `XPA_BENCH_FACTS` overrides are bounded at `1..=100,000` by `norn.limits/1` (`ResourceLimitProfile::norn_limits_v1().question_limits().max_scanned_records`), refusing fail-closed above `100,000`; full `1M+` partitioned / OLAP sweeps and external comparative scorecards remain tracked under **`#1965`** (`Refs #1965`):

```bash
# Medium profile (1,000 governed cell/OLAP facts across 10 stores x 20 SKUs x 26 weeks)
XPA_BENCH_SCALE=medium cargo test -p norn-cell --release --test suites xpa_dst_harness::xpa_layer5_scale_benchmark -- --ignored --nocapture

# Full single-question profile (26,000 governed cell/OLAP facts by default; XPA_BENCH_FACTS <= 100,000 under norn.limits/1)
XPA_BENCH_SCALE=full cargo test -p norn-cell --release --features duckdb,datafusion --test suites xpa_dst_harness::xpa_layer5_scale_benchmark -- --ignored --nocapture
```
