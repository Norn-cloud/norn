# Core 03: Deterministic query and compute

Purpose [Owner]: Norn is a data engineering tool; the ontology is its semantic layer. Derived concepts, folds and questions declare pipelines; authors do not write DAG execution code. One definition has exact in-cell, incremental IVM and OLAP execution (DuckDB/Iceberg in WellDB). [Common conventions](README.md) apply.

## IR contract

Reuse `RuleRoot`, `RuleDecisionV1`, `JdmGraph`, `SliceDeclaration`, `ZenExpression`, `PropertyType`, `RuntimeValue`, `ViewIrPayload`/`MetricDefinition`. Extend the shared admitted algebra, not parallel SQL/JavaScript semantics.

| Target record | Required fields |
|---|---|
| Query plan | `id: Pin`, typed input/output schemas, `source: typed set/history/link`, `operators`, `grain: [member identity]`, `units`, `time_basis: effective/recorded/both`, `bounds`, `freshness: FreshnessPolicy`. |
| Operators | closed variants: select/filter/project/link/group/aggregate/classify/ordered_fold/as_of; typed member/expression references; each records dependencies and input completeness. No arbitrary function node. |
| Ordered fold | `item_schema`, `accumulator_schema`, `initial: Exact record`, `order: [typed key + direction]` ending in stable ID, `step: pinned RuleIR`, `result: Expr`, `max_items`, `max_steps`, `scale`, `rounding`, residual/allocation provenance fields. Follow #323's wire contract when landed. |
| Freshness policy | `mode: exact/bounded_staleness/best_effort`, `max_lag_ms?` (required for bounded), `required_sources: [connector partition]`, `on_unmet: refuse/label` (exact must refuse); governed law reads always exact. |
| Result envelope | `query_pin`, `engine: cell/ivm/olap`, `revision`, `effective_as_of`, `known_as_of`, `source_cursors: map(source,partition→cursor)`, `watermarks`, `freshness: exact/fresh/stale/incomplete`, `complete`, `grain`, `lineage_ref`, `rows`. |

Derived concepts and questions share this algebra. Source cursors are opaque and compared only by their connector's declared ordering; never take a meaningless minimum across unrelated sources. Expose a vector frontier, not a fabricated global source sequence.

## Evaluation and obligation semantics

1. Resolve the pinned graph and read scope. Materialize bounded complete dependencies, including empty partitions and memberships; pagination cannot stand for a universal domain. Sort by the declared total order before each fold; missing/duplicate final IDs refuse.
2. Fold starts with `a0`; for each item `i[k]`, `a[k+1] = step(a[k], i[k])`. Validate exact accumulator/output types after every step; bounds/overflow/division errors discard the entire result. Residuals and consumed allocations retain original event/lot IDs. Generate base and step obligations from these same equations, not an independent FIFO implementation.
3. Aggregation defines grain before fan-out. A measure is counted once per declared identity grain unless the query explicitly declares a new grain/allocation weighting. Multiplying a holding through two customer links cannot double its value. Group only compatible units/currencies; ratios and time-additivity are declared, not inferred from “numeric”.
4. Exact in-cell evaluation is authoritative for governed commits. IVM maintains inserts, retractions and corrections at a declared frontier. OLAP lowers the same admitted plan and numeric/null/order semantics over pinned Iceberg snapshots. Unsupported operators refuse engine selection; no approximate fallback may masquerade as exact.
5. At equal definition, time, scope, source-frontier and input multiset, all engines must return canonically equal rows and lineage contributions. Physical plans may differ. Freshness uses observed per-source high-water marks and heartbeat time; unavailable sources make completeness unknown. A bounded-stale result is usable analytically only with its label, never to authorize a commit.
6. Changes invalidate dependent questions/derived concepts; schedules and backfills are generated from dependency/freshness declarations. IVM restart resumes durable cursors or performs a labelled full rebuild; never publish a partially rebuilt generation as complete.

| Invariant / layer | Stable target diagnostic |
|---|---|
| COMPUTE-DETERMINISTIC-1: pinned inputs yield same output; construction/evaluation | `core.compute.nondeterministic` |
| FOLD-ORDER-1: total stable order and exact steps; admission/evaluation | `core.compute.invalid_order` |
| FOLD-BOUNDS-1: bounded full result or refusal; evaluation | `core.compute.bound_exceeded` |
| QUERY-GRAIN-1: explicit grain/fan-out and units; admission | `core.compute.ambiguous_grain` |
| QUERY-FRESH-1: authoritative reads complete/exact; evaluation/commit | `core.compute.stale_input` |
| QUERY-ENGINE-1: equal frontier gives equivalent engine results; qualification | `core.compute.unsupported_engine` |

Evidence: structural typing, sampled three-engine differential tests, runtime bounds/read fences; generated fold induction and aggregate preservation goals. IVM tests are not a proof of incremental correctness; WellDB's current evidence ceiling stays explicit.

## Named acceptance scenarios

- **FOLD-ALLOCATE:** Given ordered layers `2×10`, `3×20`, when consuming 4, then FIFO cost 60 and residual 1 valued 20; reorder physical input and obtain the same result.
- **FOLD-CAPACITY:** Given credits 2 and 3, when allocating 4 using the same evaluator, then residual 1; no stock opcode or branch exists.
- **QUERY-FANOUT:** Given one holding worth 80 linked to two tags, when summing at holding grain, then total 80; an unspecified expanded grain refuses.
- **QUERY-STALE:** Given source frontier E=10/S=7 with Shopify heartbeat past max lag, when reading bounded-fresh inventory value, then label stale or refuse per policy; commit-law use refuses.
- **QUERY-THREE:** Given identical frontier and bitemporal selection, when cell/IVM/OLAP evaluate the question, then exact totals and contribution lineage agree, including retractions. See [beat 6](../acceptance/data-engineering.md).

## Main and gaps

Present: `crates/norn-ir/src/{rule,jdm,view,metric}.rs`, `crates/norn-checker/src/{metric,jdm_profile}.rs`, `crates/norn-cell/src/reactive.rs`, `crates/norn-expr/`, `crates/norn-checker/src/obligations/induction.rs`. Ordered fold #323 and membership fencing #324 are open. Full folds, three-engine lowering, freshness frontiers and grain-aware analytical execution are not established on main. #69 introduces `norn-metrics` temporal admission in an open PR; the crate is absent at this baseline. #97/#103 are open WellDB prerequisites, not a shipped IVM/OLAP engine; #71 history-at-cursor is also open.

## Work breakdown (each PR ≤400 lines; beat 6 is demo priority)

1. Land/adapt A1 #323 fold contract with fail-closed unsupported execution; complete accumulator/step typing.
2. A2 exact arithmetic evaluator, then bounded interpreter, then FOLD-ALLOCATE/CAPACITY correspondence as separate cuts; remove refusal only for supported semantics.
3. A3 #324 capture full partition reads; retain them through 04 preparation/commit.
4. **Demo priority:** add grain/freshness/cursor result contract; QUERY-FANOUT/STALE admission tests.
5. **Demo priority:** lower the inventory-value question to exact reference evaluation, then IVM delta/retraction, then OLAP pinned-snapshot evaluation (three separate PRs); each unsupported engine remains refused until qualified.
6. **Demo priority:** QUERY-THREE and beat-6 incremental/full-recompute tests; bind #69 temporal tables/metrics analysis inputs to the same provenance and frontier.
7. Generate fold/aggregate obligations after 05 semantics; add shared classified/derived concept dependency invalidation with 02/08.
