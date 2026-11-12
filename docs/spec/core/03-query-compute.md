# Core 03: Deterministic query and compute

Purpose [Owner]: Norn is a data engineering tool; the ontology is its semantic layer. Derived concepts, folds and questions declare pipelines; authors do not write DAG execution code. One definition has exact in-cell, incremental IVM and OLAP execution (DuckDB/Iceberg in WellDB). [Common conventions](README.md) apply.

## IR contract

Reuse `RuleRoot`, `RuleDecisionV1`, `JdmGraph`, `SliceDeclaration`, `ZenExpression`, `PropertyType`, `RuntimeValue`, `ViewIrPayload`/`MetricDefinition`. Extend the shared admitted algebra, not parallel SQL/JavaScript semantics.

| Target record | Required fields |
|---|---|
| Query plan | `id: Pin`, typed input/output schemas, `source: typed set/history/link`, `operators`, `grain: [member identity]`, `units`, `time_basis: effective/recorded/both`, `bounds`, `freshness: FreshnessPolicy`, `required_sources: [connector partition]`, `on_unmet: refuse/label` (authoritative reads must refuse). |
| Operators | closed variants: select/filter/project/link/group/aggregate/classify/ordered_fold/as_of; typed member/expression references; each records dependencies and input completeness. No arbitrary function node. |
| Ordered fold | `item_schema`, `accumulator_schema`, `initial: Exact record`, `order: [typed key + direction]` ending in stable ID, `step: pinned RuleIR`, `result: Expr`, `max_items`, `max_steps`, `scale`, `rounding`, residual/allocation provenance fields. Follow #323's wire contract when landed. |
| Freshness policy | Reuse admitted `FreshnessPolicy::Live { max_age_ms }`, `Delayed { max_age_ms }`, `AsOf { at: UtcTimestamp }`; no replacement wire variants. Source completeness and unmet-policy handling are target query-plan fields, not changes to this enum; governed law reads always exact. |
| Result envelope | `query_pin`, `engine: cell/ivm/olap`, `revision`, `effective_as_of`, `known_as_of`, `source_cursors: map(source,partition→cursor)`, `watermarks`, `freshness: exact/fresh/stale/incomplete`, `complete`, `grain`, `lineage_ref`, `exclusions: map(identity → finding_id)`, `rows`. |

Derived concepts and questions share this algebra. Source cursors are opaque and compared only by their connector's declared ordering; never take a meaningless minimum across unrelated sources. Expose a vector frontier, not a fabricated global source sequence.

## Numeric additivity admission (#527)

**METRIC-ADDITIVITY-1:** numeric property types declare `additivity.kind` as
`additive`, `semi_additive`, `non_additive`, or `ratio`. Ratio declarations bind
`numerator` and `denominator` properties at the object's grain. Omission means
`undeclared`: point reads remain legal, but every aggregate over that numeric
property, including counts and extrema, refuses with
`metric.validation.undeclared_additivity` and `FixDefinition` guidance.
Units never imply additivity. Existing metric metadata cannot authorize an
aggregation the property's contract forbids. Stored ratios refuse scalar
aggregation (`metric.validation.ratio_aggregation`); callers must sum their
operands separately before dividing. Non-additive prices require a declared
weighting calculation; this bounded aggregate vocabulary provides none.
Semi-additive balances require the existing snapshot contract, whose unsupported
execution continues to refuse. Inheritance must preserve the classification.

Named scenarios: **METRIC-UNDECLARED** (all numeric aggregates refuse without a
declaration), **METRIC-RATIO-AVERAGE** (stored-ratio aggregation refuses),
**METRIC-BALANCE-TIME** (an additive metric cannot bypass a balance's time
restriction), **METRIC-DECLARED-SUM** (explicit additive quantities use the same
keyed compute admission). Conservation-derived candidates and their law
provenance use the following checked derivation contract.

**METRIC-DERIVE-1:** derive a factorized candidate space from effective numeric
properties, direct/single-link scalar dimensions and existing calendar grains.
Candidates are uncertified; deriving one does not declare a governed KPI or
authorize execution outside the existing compute profile. Missing additivity
still refuses unless an admitted conservation law supplies its provenance.

At an explicitly selected law root, an unconditional, error-severity `always`
law of the form `self.input == self.output + self.loss` supplies additive terms
and one ratio of sums for each output/input pair. The bounded structural
recognizer accepts distinct same-type/same-unit terms, not coefficients,
conditionals or nonlinear algebra. Rates, nullable/computed terms, explicit conflicting
additivity, state-limited laws and other law roots supply no such contract.
Generated definitions retain the canonical law-envelope digest; changes to a
law change the generated ratio identity. Artifact order does not change identity.
Reordering terms within a law expression does change identity.
The original ontology and every property contract carried by the compute plan
remain unchanged. Only the checker-owned catalog contains the law admission
evidence, and every selection lowers through `compute::admit`. In particular,
law evidence cannot make the plan fail the native cell's model-contract guard.

Named scenarios: **METRIC-RATE-UNKNOWN** (rate terms supply no conservation
additivity), **METRIC-CONSERVATION** (no metric declarations yield recovered
and loss ratios), **METRIC-LAW-SCOPE** (conditional/advisory/other-scope laws
supply no additivity), **METRIC-LAW-PIN** (deterministic derivation retains the
exact governing law), **METRIC-SCRAP-PARITY** (all 80 pinned scrap-yield rows
produce the same contributions as the existing compute path, with showcase
totals 294/300 and ratio 0.980000), and **METRIC-NONLINEAR-UNKNOWN** (unsupported
equalities create no conservation candidate). Unsupported calendar grains,
link execution and snapshot evaluation keep their existing refusals.
**METRIC-MODEL-PARITY** requires every law-derived plan property to retain the
same native model contract as the original, undeclared ontology.

## Typed question construction (#658, #528)

**QUESTION-LOWER-1:** `Norn.question` is syntax-only construction of the existing
`QuestionIR`. It names one admitted metric, an object set, direct Eq/Ne
predicates, direct dimensions, a half-open effective window and live/delayed
freshness. Its explicit `authorized_objects` population grants no access. The
host must supply its resolved source/authorization `QueryContext` and use
ordinary `compute::admit` before
execution. No SQL, callback, row-time surrogate or alternate evaluator is emitted.

`norn-ts question file.ts` emits standalone candidates, not installable artifacts.
The ordinary artifact compiler refuses a question rather than silently dropping
it. Unsupported populations, predicates, unknown members and exhausted compiler
bounds refuse. Names and ontology/type/unit semantics remain Rust admission's
responsibility. Other time grains may be constructed but still face the existing
qualified execution-profile check.

Knowledge-time construction remains refused with `core.compute.unsupported_engine`
until ASOF-KNOWLEDGE-1 admits the declared AsOf policy through its authenticated
cutoff seam. No timestamp filter or new QuestionIR field represents that cutoff.
Named scenario **typed_question_lowering** covers positive lowering, refusal,
symbol identity, object references and bounded construction. This slice adds
neither natural-language rendering nor ambiguity interpretation.

## Typed transforms (ViewIR v1, #341)

A transform is a `derived` artifact whose `calc` is `relational`: a closed DAG of typed operators over declared inputs (`scan`, `filter`, `project`, `join`, `aggregate`, `window`, `union`, `dedupe`). SQL and dataframe surfaces are authoring syntax that compiles to this plan; nothing in it is executed raw, and there is no function node. The wire kind stays `derived` and ViewIR v0's `field_projection` is unchanged (D84). Admission is `norn_checker::transform::admit` over the same artifact pool as every other check; it grants no read authority.

**VIEW-TYPE-1:** every node has a closed row schema: each column's type (nullable as `Optional`), additivity, the object type its ids name (if any), the stored property it is an unchanged copy of (if any), its column lineage (every stored property it derives from), its effective markings and whether a join repeats it. Expressions are ZEN over `input.<column>` under the input-only environment, checked with the `norn-zen` profile: a `filter` predicate is Boolean (null is not a value a predicate may return), a computed column is a scalar, and `input.c` alone passes column `c` through with its full contract. An unknown input, node, column or output, a cycle, a node that does not feed the output and an input scanned twice refuse (`derived.validation.invalid_plan`, `unknown_column`, `type_mismatch`). v1 admits entity inputs, `recompute_on_read` and the current temporal scope only; `outputSchema` is `{}` because the plan infers it (`output_schema_mismatch`).

**VIEW-GRAIN-1:** every node has a row identity. A scan's is the object `id`; a projection must pass every grain column through unchanged (`grain_dropped`); a to-one join keeps the left grain and a to-many join appends the right id; an aggregate's is its group keys (none means exactly one row); a union's is its branch column then the shared input grain; a dedupe's is its key. Output rows are ordered by grain, nulls last, and no two share one identity.

**VIEW-JOIN-1:** a join's right side is one object type's rows at their identity grain, and rows pair only (a) along a forward link the left column's object type declares, whose target admits the right type, or (b) on a declared key equivalence: the left columns, in component order, equal a key the right type declares (or its `id`) under that key's own normalization, with exactly the component types. The right side is unique on its key, so (b) is many-to-one. Anything else is `derived.validation.join_undeclared`. A value is marked repeated when the join can pair it with more than one row: the left side through a to-many link, the right side through a link whose inverse is to-many or through any key join. Right columns carry the join's `prefix`; output names stay unique (`column_conflict`); a left join makes right columns nullable.

**VIEW-AGG-1:** aggregates reuse METRIC-ADDITIVITY-1 through column lineage. Any aggregate of a numeric column with undeclared additivity refuses `metric.validation.undeclared_additivity`, counts and extrema included; `sum` and `running_sum` need `additive`: a stored ratio refuses `metric.validation.ratio_aggregation`, a semi-additive balance `question.validation.snapshot_aggregation` (stocks are never summed across time), a non-additive value `metric.validation.invalid_additivity`. A computed column's additivity is undeclared unless it passes a column through. Summing a repeated value refuses `core.compute.ambiguous_grain` (QUERY-FANOUT: one holding worth 80 linked to two tags is 80, not 160; aggregate before joining). A money value with a per-row currency is summed only when the group keys (or a running sum's partition) include that currency (`metric.validation.mixed_currency`). Calendar grains bucket dates and timestamps in ISO/UTC (weeks start Monday) and produce dates. `sum` skips nulls and is null when nothing was summed.

**VIEW-ORDER-1:** a window's partition and order, and a dedupe's key and order, must cover the input grain (`derived.validation.order_not_total`), so ranks, offsets, running totals and the kept row are deterministic. Nulls sort last in both directions.

**VIEW-UNION-1:** union inputs have the same columns in the same order with the same types, additivity, currency and grain (`derived.validation.union_shape`); the branch column names each row's input node and is part of the union's identity, since ids of different types name different objects.

**VIEW-READ-1:** reading a transform needs the transform grant and the HOST-READ-1 collection gate on every object type it reads, transitively, decided before evaluation with one opaque refusal (the QUESTION-ADHOC-1 union rule).

**VIEW-ROWS-1:** v1 evaluates over the reader's readable rows only (Population and Values, HOST-READ-2), so a transform never shows a value its reader may not read. Rows the reader may count but not read are excluded, counted per input, and make the result incomplete. An aggregate is therefore never more disclosing than a question over the same rows; counting Population-only members with DATA-VALUES-1 withholding is a later, separate law.

**VIEW-MARK-1:** an output column's markings are the union of the markings in its lineage. They describe the column; no read path applies masks yet.

**VIEW-LINEAGE-1:** column lineage is static and part of the admitted plan. Each output row lists the input rows it derives from (alias, id, version): one for a scan, filter, projection or dedupe, both sides for a join, every member for an aggregate; a window row lists its own members (its value is a function of its partition up to that row in the declared order).

**VIEW-ENGINE-1 (QUERY-ENGINE-1 extended):** `norn_checker::transform::eval` is the exact reference interpreter. It refuses the whole result on any bound (100,000 rows per node), overflow or runtime expression error. Other engines lower the same admitted plan and must return canonically equal rows and lineage at the same snapshot; operators they cannot lower exactly refuse `core.compute.unsupported_engine`.

**VIEW-SQL-1:** SQL is an authoring surface, never an execution path. `norn_checker::transform::sql` parses a restricted dialect (`SELECT … FROM type [AS a] [[LEFT|INNER] JOIN type [AS b] ON …] [WHERE …] [GROUP BY …]`, joined by `UNION ALL`), resolves every name against the effective catalog and lowers it to the operators above; the checker then types that plan like any authored one, so every law applies to SQL unchanged. A join's `ON` names a declared link (`o.customer = c.id`) or a declared key of the joined type (`l.email = c.email`); aggregates are `SUM`, `COUNT`, `COUNT(DISTINCT …)`, `MIN` and `MAX`, `DATE_TRUNC('week', column)` buckets a group key, and windows are `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD` and a running `SUM … OVER`. Every source's id passes through so the grain survives. An ad-hoc join, `ORDER BY`/`LIMIT`, `DISTINCT`, `UNION` without `ALL`, `HAVING`, window frames and any function without an operator refuse `derived.validation.sql_unsupported` and are never passed through. Nothing is lowered to something close to what was written: division (SQL truncates integers), a comparison with a `NULL` literal (never true in SQL), a constant or position as a group or window key (`GROUP BY 1`), and a column or alias that is not a plain identifier (it would become part of a ZEN path) refuse too, and the SQL keywords the dialect lacks are never read as names (`FROM t FULL JOIN …`). Named scenarios (`crates/norn-checker/tests/transform_sql.rs`): `a_declared_link_join_grouped_by_week_lowers_admits_and_evaluates`, `a_key_join_and_windows_lower_to_typed_operators`, `what_has_no_typed_operator_is_refused_never_run`, `real_world_sql_outside_the_dialect_refuses_cleanly` (a corpus of real-world SQL outside the dialect). Properties (`norn_checker::transform::sql`): `a_printed_query_parses_back_to_itself`, `an_accepted_query_is_admitted_or_refused_with_a_registered_code`, `no_text_panics_and_every_refusal_is_registered`.

Named scenarios (`crates/norn-checker/tests/transform.rs`): `link_and_key_joins_are_the_only_joins`, `fanned_sums_refuse_and_aggregating_first_counts_once`, `aggregates_follow_the_metric_additivity_law`, `windows_and_dedupes_need_a_total_order`, `unions_need_one_shape`, `projections_keep_the_grain`, `evaluation_is_deterministic_under_input_order`, `calendar_buckets_follow_iso_utc`.

## Evaluation and obligation semantics

1. Resolve the pinned graph and read scope. Materialize bounded complete dependencies, including empty partitions and memberships; pagination cannot stand for a universal domain. Sort by the declared total order before each fold; missing/duplicate final IDs refuse.
2. Fold starts with `a0`; for each item `i[k]`, `a[k+1] = step(a[k], i[k])`. Validate exact accumulator/output types after every step; bounds/overflow/division errors discard the entire result. Residuals and consumed allocations retain original event/lot IDs. Generate base and step obligations from these same equations, not an independent FIFO implementation.
3. Aggregation defines grain before fan-out. A measure is counted once per declared identity grain unless the query explicitly declares a new grain/allocation weighting. Multiplying a holding through two customer links cannot double its value. Group only compatible units/currencies; ratios and time-additivity are declared, not inferred from “numeric”.
4. Exact in-cell evaluation is authoritative for governed commits. IVM maintains inserts, retractions and corrections at a declared frontier. OLAP lowers the same admitted plan and numeric/null/order semantics over pinned Iceberg snapshots. Unsupported operators refuse engine selection; no approximate fallback may masquerade as exact.
5. At equal definition, time, scope, source-frontier and input multiset, all engines must return canonically equal rows and lineage contributions. Physical plans may differ. Freshness uses observed per-source high-water marks and heartbeat time; unavailable sources make completeness unknown. A bounded-stale result is usable analytically only with its label, never to authorize a commit.
6. Changes invalidate dependent questions/derived concepts; schedules and backfills are generated from dependency/freshness declarations. IVM restart resumes durable cursors or performs a labelled full rebuild; never publish a partially rebuilt generation as complete.

Under QUERY-ENGINE-1, IVM retains unchanged finding exclusions and reevaluates subjects of authenticated changed receipts; source evidence arriving without a receipt invalidates affected members through the existing rechecked index, with rebuild cache pins captured in the snapshot transaction. The named `finding_lookups_are_limited_to_changed_receipt_members`, `unchanged_receipt_preserves_quarantine_without_rechecking_members` and `same_frontier_untrusted_source_evidence_refuses_atomically_like_recompute` scenarios supplement delta/recompute correspondence and quarantine→restatement→rebuild evidence; the bounded benchmark claims lookup reduction, not constant-time maintenance.

| Invariant / layer | Stable target diagnostic |
|---|---|
| COMPUTE-DETERMINISTIC-1: pinned inputs yield same output; construction/evaluation | `core.compute.nondeterministic` |
| FOLD-ORDER-1: total stable order and exact steps; admission/evaluation | `core.compute.invalid_order` |
| FOLD-BOUNDS-1: bounded full result or refusal; evaluation | `core.compute.bound_exceeded` |
| QUERY-GRAIN-1: explicit grain/fan-out and units; admission | `core.compute.ambiguous_grain` |
| QUERY-FRESH-1: authoritative reads complete/exact; evaluation/commit | `core.compute.stale_input` |
| QUERY-ENGINE-1: equal frontier gives equivalent engine results; qualification | `core.compute.unsupported_engine` |

Current evidence: structural typing and narrow induction substrate at the baseline below. Target evidence: sampled three-engine differential tests, runtime fold bounds/full read fences, and generated fold induction and aggregate preservation goals. IVM tests are not a proof of incremental correctness; WellDB's current evidence ceiling stays explicit.

## Named acceptance scenarios

- **FOLD-ALLOCATE:** Given ordered layers `2×10`, `3×20`, when consuming 4, then FIFO cost 60 and residual 1 valued 20; reorder physical input and obtain the same result.
- **FOLD-MEMBERSHIP:** A declared fold consumes only complete captured members. An incomplete first page (including an empty prefix) refuses before evaluation/preparation; complete empty and populated sets retain their membership fence through commit, so a concurrent insertion cannot publish the folded result. The reference-host witness uses the existing typed accumulation declaration; it does not claim a native paginated backend.
- **FOLD-CAPACITY:** Given credits 2 and 3, when allocating 4 using the same evaluator, then residual 1; no stock opcode or branch exists.
- **QUERY-FANOUT:** Given one holding worth 80 linked to two tags, when summing at holding grain, then total 80; an unspecified expanded grain refuses.
- **QUERY-STALE:** Given source frontier E=10/S=7 with Shopify heartbeat past max lag, when reading bounded-fresh inventory value, then label stale or refuse per policy; commit-law use refuses.
- **QUERY-THREE:** Given identical frontier and bitemporal selection, when cell/IVM/OLAP evaluate the question, then exact totals and contribution lineage agree, including retractions. See [beat 6](../acceptance/data-engineering.md).

## Main and gaps

Present: `crates/norn-ir/src/{rule,jdm,view,metric}.rs`, `crates/norn-checker/src/{metric,jdm_profile}.rs`, `crates/norn-cell/src/reactive.rs`, `crates/norn-expr/`, `crates/norn-checker/src/obligations/induction.rs`. Ordered fold #323 and membership fencing #324 are open. Full folds, three-engine lowering, freshness frontiers and grain-aware analytical execution are not established on main. #69 introduces `norn-metrics` temporal admission in an open PR; the crate is absent at this baseline. #97/#103 are open WellDB prerequisites, not a shipped IVM/OLAP engine; #71 history-at-cursor is also open.

## Work breakdown (one coherent unit per PR; beat 6 is demo priority)

1. Land/adapt A1 #323 fold contract with fail-closed unsupported execution; complete accumulator/step typing.
2. A2 exact arithmetic evaluator, then bounded interpreter, then FOLD-ALLOCATE/CAPACITY correspondence as separate cuts; remove refusal only for supported semantics.
3. A3 #324 capture full partition reads; retain them through 04 preparation/commit.
4. **Demo priority:** add grain/freshness/cursor result contract; QUERY-FANOUT and freshness-policy shape admission tests, plus QUERY-STALE evaluation/commit tests for stale labels, stale-result refusal and commit-law refusal.
5. **Demo priority:** lower the inventory-value question to exact reference evaluation, then IVM delta/retraction, then OLAP pinned-snapshot evaluation (three separate PRs); each unsupported engine remains refused until qualified.
6. **Demo priority:** QUERY-THREE and beat-6 incremental/full-recompute tests; bind #69 temporal tables/metrics analysis inputs to the same provenance and frontier.
7. Generate fold/aggregate obligations after 05 semantics; add shared classified/derived concept dependency invalidation with 02/08.

## Operator certificates (assurance PR 1)

COMPUTE-DETERMINISTIC-1 uses `norn.ivm-algebra/1`: fixed selection,
projection and exact grouped sums are homomorphisms over signed multisets.
Bilinear joins require all three old-snapshot terms
`J(dL,R) + J(L,dR) + J(dL,dR)`; an omitted or duplicated coefficient
refutes the certificate. Composition requires certified prior dependencies.
Only closed declared homomorphisms are supported. Undeclared/opaque folds
are unknown; monoid-only folds cover inserts, while deletions need a group.
Rounded ratios are shared finalization of maintained sums, not linear operators.
Arithmetic refusals and stable interpretation/frontier pins remain assumptions.

Evidence levels differ: **proved-algebraic** describes the certificate's
symbolic rule check, conditional on faithful lowering into this algebra.
The existing generated lane G tests supply **sampled runtime correspondence**
for admitted sums and their shared ratio finalizer, including restatements.
They do not certify arbitrary ordered folds. Join runtime correspondence is
**unknown**, reason: "QuestionPlan does not admit joins". The exhaustive
small signed-relation join test is algebraic evidence only. Wire runtime
correspondence when #422/#341 admit joins. `Composition.lean` states and
proves the abstract local-equation composition rule; it does not establish
Rust implementation correspondence or source-bound Challenge evidence.
## Aggregation changes during model migration

**METRIC-ADDITIVITY-MIGRATION:** additivity changes aggregation semantics and the full plan/model contract, not which stored values are admitted. An additivity-only change remains a governed declared model change in `additivityChanges`, with previous/candidate classes, and never triggers a live-value narrowing scan. Actual kind, scale or constraint narrowing still scans, including when additivity changes in the same update. Object, link and nested-record property changes retain their affected identities; ledger summaries bound rows and identifiers.

Named scenarios: `additivity_only_change_skips_live_scan_over_4096_rows` (Integer and Decimal), `additivity_change_with_value_narrowing_still_scans_live_rows`, `additivity_change_is_declared_separately_from_value_narrowing`, `additivity_changes_preserve_values_but_change_the_plan_contract`. Ratio authoring operands must match the complete `PropertyId` grammar before emission; `ratio operands must be complete PropertyId values` covers malformed operands.

## Cell-owned quarantine exclusions (DATA-QUARANTINE)

Question execution resolves exclusion identity → finding ID from durable open
findings under their retained governing definitions. Recovery evaluates the
selected record incarnation; historical selection admits only findings committed
by its authenticated knowledge frontier. The existing observation findings read
returns the evidence document (rule, source, offending values and reason).
`QueryContext` cannot represent exclusions; unknown wire fields refuse decoding.
Only `QueryResult.exclusions` carries engine-derived finding pins.
Visible candidates only disclose exclusion pins. Incremental maintenance retracts
changes in finding disposition even when the aggregate input is unchanged.
Historical source-dispute overlays remain refused as
`core.world.source_dispute_unqualified` until their frontier is retained.

Named scenario: `data_quarantine_pins_real_findings_and_historical_recovery_equals_ivm`.

Regression: `quarantined_restatement_rebuilds_from_result_context_and_rederives_finding_pins`
checks that reusing an answer context re-derives pins at the historical frontier.
## Structural sum extraction

`EXPR-STRUCTURAL-SUM-1`: `norn-expr` recognizes a bounded AST equality of one direct `self` property to the sum of 2–64 distinct direct properties. Parentheses and literal member notation retain their AST meaning. Coefficients, subtraction, repeated terms, the total on the right, conditionals and other reads return no shape; malformed or over-budget source returns an expression error. Extraction grants no type, unit, scope, additivity or conservation authority: the consuming checker must establish those independently.

Named scenarios: `structural_sum_accepts_parentheses_and_literal_members`, `structural_sum_refuses_other_operations_and_reads`, `structural_sum_bounds_and_malformed_source_fail_closed`, `structural_sum_preserves_generated_terms`. Evidence is structural and sampled, not algebraic proof.

## Host read authority

**HOST-READ-1:** configured host reads distinguish Population, Values and Sources. Metadata/action grants imply none of them. The shared authorizer receives only physical-cell scope, and mismatched original bindings still deny. Authorization and explicit scope admission both execute before storage access under one held definition epoch; every failed read gate returns identical static 403 bytes. Named correspondences: `metadata_grants_do_not_grant_read_capabilities`, `read_gate_checks_policy_and_scope_before_reading`, `physical_scope_is_the_only_policy_scope`, and `denied_read_documents_do_not_reveal_the_physical_scope`.

**HOST-ANSWER-1:** answer and numeric-trace services select already-admitted questions and server-resolved populations/frontiers through the shared host. Their PDP mappings retain the physical scope, principal and actual row identity. Direct engine serialization preserves the generated SDK's exact-number contract. Exact/fresh/stale/incomplete analytical labels are retained; trace requires an exact complete current result. AsOf evaluation consumes the authenticated native cutoff; historical tracing remains refused until qualified natively. Stable compute refusals become host refusal documents without storage diagnostics. Named correspondences: `unauthorized_answers_and_traces_have_identical_403s_before_resolution`, `live_answers_and_traces_apply_current_independent_population_and_disclosure_grants`, `fresh_stale_and_incomplete_answers_keep_labels_but_nonexact_traces_refuse`, `asof_uses_authenticated_cutoff_and_preserves_the_old_answer_after_correction`, and `live_route_documents_conform_to_the_generated_sdk`. HTTP dispatch belongs to the shared host adapter.

**DATA-VALUES-1 (D52):** a member's contribution amounts are row values. Population decides who is counted and discloses the bucket's aggregate (totals and value); a member's amounts need Values on that member. A bucket in which the reader lacks Values on any member withholds every member's amounts (`amounts: null`, the withheld members `restricted: true`), in the answer, the trace's embedded answer and the trace's contributions alike, so no withheld amount follows from the totals minus the visible ones. The withholding is a projection after evaluation: pins and lineage are unchanged. Named correspondences: `member_amounts_are_values_and_follow_the_values_grant`, `one_restricted_member_withholds_its_whole_bucket`.

**DATA-VALUES-1 (D53) extends D52:**
1. **Value-bearing fields.** In a bucket where the reader lacks Values on any member, every value-bearing field of every member is withheld, not only `amounts`. In the trace that means each contribution's `values`, `factDigest` and `receiptHash` (null). A digest is a guessing oracle for the values it hashes, and the receipt hash commits to the commit batch, values included: for a single-write commit it confirms a guess offline. Receipt hashes and the ledger head (`revision.hash`) are unkeyed hashes over values beyond any one bucket: a receipt commits to its whole commit batch and to the ledger head, and the ledger head chains every event, which carry record values, so a reader who saw the previous revision could confirm a guessed value of one new event offline. They are therefore raw only for a reader with **whole-world Values**: a collection-level Values grant on every object type of the cell, decided by host policy without reading cell data (`QuestionRuntime::collection_access`), and from whom the answer withholds no member. Every other reader, including one who may read every row of its slice, sees keyed handles in their place: HMAC-SHA-256 under the cell's handle key (item 3) of `norn.receipt/1:<hash>` for a contribution's `receiptHash` and a historical answer's cutoff coordinate (`world.historical.commit`), of `norn.revision/1:<hash>` for `revision.hash` in an answer, a historical answer (the cutoff head) and a trace's embedded answer, and of `norn.finding/1:<id>` for each quarantine finding ID in `exclusions` (live, historical and embedded). A finding identity hashes the quarantined record's canonical values without a key, and Population alone shows the exclusion, so the raw ID would confirm a guessed value offline; the excluded record IDs stay, and internal durable finding identities are unchanged. Handles are stable per receipt and revision, so clients compare them only for equality. The decision is constant-time in history: no chain walk and no budget path. The genesis head chains nothing and stays raw; internal pins keep raw hashes. Source provenance follows its own Sources grant.
2. **Minimum bucket size.** To such a reader, a bucket aggregate is disclosed only when the bucket has at least *k* members. *k* is declared server policy (`QuestionRuntime::min_disclosed_bucket`, default 3), never a request parameter. A smaller bucket keeps its membership but comes back `suppressed: true`, with `value: null` and empty `totals`. It is not a refusal.
3. **Keyed lineage handle.** The internal result pin is a content hash of the unredacted rows, so publishing it would let a reader verify a guessed value offline. The wire `lineageRef` is instead HMAC-SHA-256 of that pin under a per-cell secret held by the host (`QuestionRuntime::handle_key`, at least 32 bytes). The trace route maps the handle back by recomputing the pin. The internal pin is never accepted on the wire.
4. **Residual limitation.** Bucket redaction does not prevent differencing *across* admitted questions or time windows. Two overlapping questions, or two windows, whose memberships differ by one restricted member can still expose that member's contribution by subtraction. Preventing this is a property of which questions are admitted, under a disclosure budget or overlap rule, not of one answer's projection. Until such a rule exists, admitting questions to readers without Values is an explicit policy decision.

Named correspondences: `a_mixed_bucket_discloses_no_member_value_from_which_the_hidden_one_follows`, `a_bucket_below_the_minimum_is_suppressed_for_a_reader_missing_values`, `the_public_lineage_handle_is_keyed_and_not_a_guessing_oracle`, `a_restricted_bucket_withholds_the_receipt_hash_of_a_single_write`, `a_shared_commit_withholds_its_receipt_hash_from_every_bucket_it_wrote`, `a_historical_cutoff_on_a_restricted_single_write_withholds_its_receipt_hash`, `a_live_answer_and_its_trace_withhold_the_ledger_head_from_a_reader_without_values`, `a_historical_answer_withholds_its_cutoff_ledger_head_from_a_reader_without_values`, `a_live_answer_withholds_its_quarantine_finding_ids_from_a_reader_without_values`, `a_historical_answer_withholds_its_quarantine_finding_ids_from_a_reader_without_values`.

**HOST-READ-2:** typed object pages and objects pass the HOST-READ-1 collection gate, then decide each row on its own: a record appears only under both Population and Values for that row. Visibility precedes data quality, so quarantine counts cover only visible rows, a cursor names only a visible row, and a page fills from visible rows only: no response depends on how many rows are unreadable (no raw-row bound, which would be a count oracle; latency remains a timing channel). The cell lock is held per scan batch; each locked section checks the cell's version (the receipt head together with the engine's database revision, which changes on every write by any connection, receipt-less evidence included) before and after its reads against the version the page started at; every engine must declare its read isolation (no default): `Revisioned` engines must supply a revision or the page refuses with 503 `object.snapshot_unavailable`, and only the Durable Object engine declares `ExclusiveSynchronous` (its cell is the only connection and a page runs in one synchronous call), and a page that would mix two snapshots refuses with 409 `object.snapshot_moved`, `retryable`. Liveness is best-effort: clients repeat the read with bounded backoff, and the host never retries internally. Absent, invisible, quarantined and undeclared objects share one refusal. Named correspondences: `quarantine_is_counted_only_among_visible_rows_and_never_listed`, `an_unreadable_row_is_absent_from_records_counts_and_cursors`, `absent_invisible_and_quarantined_objects_share_one_refusal`, `denied_and_foreign_scope_reads_share_one_opaque_denial`, `pages_do_not_depend_on_how_many_rows_are_unreadable`, `a_commit_during_the_scan_refuses_the_page_as_retryable`, `a_commit_inside_a_read_section_refuses_the_page`, `a_write_without_a_receipt_inside_a_section_refuses_the_page`, `a_revisioned_engine_without_a_revision_refuses_the_page`.

**QUESTION-ADHOC-1 (F2b, D62):** `POST /v1/questions:evaluate` and `:trace` answer a submitted QuestionIR once, never installing it. An ad-hoc question has no resource of its own, so it is authorized as the union of everything it reads: every catalog metric it names, a ratio's numerator and denominator transitively, and every object type those read. Every member must be granted. This is decided **before admission**, so a refusal's diagnostics never describe an ungranted resource. An admitted question is answered under the same snapshot, row gates and DATA-VALUES-1 redaction as a named question. Rows read and output buckets are bounded; past either bound the request refuses with 413 instead of truncating.

**Undeclared metrics (D62):** that a named metric does not exist is disclosed, as the checker's 422 `question.refused` with a pointer to the metric (for example `/question/aggregate/0/metric`), **only to a caller who can read every metric the cell declares**. For that caller the absence says nothing they could not already see. For any caller missing any metric grant, an undeclared metric is the same opaque 403 as an ungranted one, byte for byte. A 403/422 split between ungranted and undeclared would otherwise be an existence oracle over the catalog. Named correspondences: `an_ungranted_metric_is_the_opaque_denial_never_a_diagnostic`, `an_undeclared_metric_is_a_diagnostic_only_for_a_caller_who_reads_every_metric`.
