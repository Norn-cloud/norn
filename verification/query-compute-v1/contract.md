# Keyed question admission — Core 03 cut G1

Implements the admission portion of QUERY-GRAIN-1 and
COMPUTE-DETERMINISTIC-1 from `docs/spec/core/03-query-compute.md`.
`metric::compute::admit` consumes the existing QuestionIR and MetricDefinition
checker, then lowers a bounded direct-property sum or ratio of sums. The
contribution grain is `(root object type, object key)`, independently of
measure values. Ratio operands retain their individual filters, effective
property types/units and numerator/denominator order. ISO-UTC weekly grouping
and the existing freshness declaration survive lowering unchanged.

The plan is immutable, carries a positive full-population bound within `norn.limits/1`, and grants
no read or mutation authority. Link traversal, inline calculations, nested
ratios, multiple outputs, nullable/computed measures, FX, metric-level freshness
or disclosure policies, roll-ups and comparisons remain refused by this
profile. Existing metric admission still diagnoses unsafe fan-out and unit or
time-grain mismatches before lowering. Unsupported lowering uses
`core.compute.unsupported_engine`; unsafe fan-out or incompatible measure
grains additionally emit `core.compute.ambiguous_grain`.

Scenarios in `crates/norn-checker/tests/metric.rs` cover typed keyed lowering,
fan-out refusal with and without symmetric aggregation, duplicate selections,
zero bounds and ambiguous outputs. Generated properties check definition-order
independence of ratio operands and positive-age freshness admission.

Evidence ceiling: structural admission plus sampled pure-function properties.
QUERY-FANOUT's numeric 80 result, QUERY-STALE runtime/commit refusals,
QUERY-THREE, source frontier/result envelopes, exact evaluation and IVM remain
unqualified. This cut does not select an engine. The governing demo fixture
is scrap yield; inventory is post-demo under the ruling on #213.

## G2 arithmetic and projection

COMPUTE-DETERMINISTIC-1 and QUERY-GRAIN-1 use scale-six coefficients: per input ≤9e18, per sum ≤9e21, at most 1,000 objects. Sums never round; ratios round once, midpoint away from zero. Zero divisors refuse in the kernel; analytical zero-input ratios must be unavailable. No fold contract changes.
The shared pure projector checks effective stored types, half-open time windows, Monday ISO-UTC buckets and each operand's own predicates. Missing/computed values and unsupported precision refuse. Timestamp equality uses instants; ordered predicates remain unsupported. Rows matching no operand contribute no membership.
The immutable plan retains effective properties and canonically ordered declarations for store binding and content identity. These primitives alone do not qualify storage/frontier execution or authenticated receipt evidence.
Native scenarios cover 98%/97.5%, signed ties, zero/range refusals, weekly buckets, cutoff exclusion and invalid values. Proptests cover sum commutativity, ratio error/sign and weekly projection.
Kani includes the production module: full-width sum/ratio safety, zero-divisor refusal, and exhaustive i16 numerators with signed 128 divisors (nearest, tie-away and sign correspondence).
The broader symbolic i16-divisor correspondence attempt timed out at 600 seconds; it is not counted as proof. Functional ratio evidence remains bounded; no universal arithmetic theorem is claimed.
Storage/frontier enforcement and #353 cell-fixture correspondence follow in the next G2 cut. IVM and governed-read qualification remain separate cuts.

## G2 result envelope

The QUERY-ENGINE-1 envelope retains exact decimal totals and per-object-version
contributions, declared source-partition cursor vectors, watermarks, scope and
population/epoch/mapping pins. Null denotes an unavailable ratio, never invented
zero. `known_as_of` is an actual knowledge revision; an effective cutoff alone
is not a historical-knowledge API. This cut adds data contracts only: the
producer must validate pins, complete reads and freshness. Exact transport and
opaque cursor separation have conformance tests. There is no new handwritten
pure function or arithmetic primitive requiring another proptest/Kani harness;
the producing evaluator and existing arithmetic kernel own that evidence.

## G2 cell reference execution

COMPUTE-DETERMINISTIC-1 / QUERY-GRAIN-1 use the shared projector and arithmetic
inside one cell transaction. A full bounded metadata/decode scan precedes
publication; budget exhaustion refuses rather than returning a partial total.
Effective model contracts and tenant scope must match. The trusted host supplies
visibility, source progress and Core 07 exclusions; this adds no quality law or
permission authority. Required source partitions retain opaque cursor vectors;
missing coverage/quarantine is incomplete; heartbeat lag above the declared
maximum, or a future heartbeat, is stale. Bound tenant scope is checked before scanning even an empty population.
The envelope binds actual ledger/knowledge revision, model/declarations, window,
mapping/population/epoch, contributions and frontier. This G2 current-world
evaluator does not select historical knowledge; `evaluate_question_known_at`
(ASOF-KNOWLEDGE-1, Core 08) owns authenticated knowledge-cutoff evaluation.
The unchanged #353 CSV is a generated numerical fixture, not authenticated
training knowledge. Native cell scenarios cover 98%/97.5%, immutable prior
results, exact contribution membership, unavailable zero-input ratios and
visibility/quarantine separation. Governing commit-read qualification follows G4.

QUERY-FRESH-1 analytical labels: complete coverage at the observed frontier with zero heartbeat lag is exact; positive lag within the admitted maximum is fresh; excessive/future heartbeat age is stale; missing coverage or quarantine is incomplete. Governed freshness refusal remains G4.

G3 aggregate mechanism (COMPUTE-DETERMINISTIC-1 / QUERY-ENGINE-1): cell recomputation uses exact signed contribution adjustment designed for delta-maintenance reuse. Retraction reverses an admitted addition without rounding; only the final ratio rounds. A generated sequence/reverse-sequence property and production arithmetic Kani retraction harness qualify this primitive. Receipt-driven maintenance and restart correspondence are the next cut; no IVM qualification is claimed here.

## G3 receipt-maintained generations
QUERY-ENGINE-1 / COMPUTE-DETERMINISTIC-1: maintenance follows gapless verified cell receipts, retracts each changed identity's prior contribution and adds its new projection. Duplicate written IDs and same-frontier delivery cannot double count. Complete source membership enforces the original population bound even for hidden/noncontributing rows. Definition, world/scope, mapping, population or authorization changes require explicit rebuild; a skipped receipt refuses with `core.compute.stale_input`.
A staged generation publishes atomically only after all reads and exact adjustments succeed. Restart closes/reopens the real cell and performs a labelled complete rebuild; no partial generation is exposed. The generated DATA-DELTA correspondence scenario compares the full envelope/rows/member versions and lineage with native recompute after corrections, insertions, deletions, effective-time changes, quarantine/recovery, duplicate delivery and restart. This is sampled implementation correspondence, not a universal IVM proof. OLAP and governed read fencing remain separate qualifications.


## G7 embedded SQL correspondence (CP3 stretch)

QUERY-ENGINE-1 / QUERY-GRAIN-1 / COMPUTE-DETERMINISTIC-1: the optional
`norn-cell/duckdb` feature lowers the same admitted sum/ratio plan over its
authorized, bounded exact contribution snapshot. Shared projection supplies
identity, predicates and time buckets. Generated SQL stores each operand in
DECIMAL at its own declared scale, sums at identity grain and rounds ratios
once with HUGEINT quotient/remainder. Values use bound parameters; extension
loading and external access are disabled. Unsupported kinds/scales refuse
`core.compute.unsupported_engine`.

The obligation is complete equality with exact cell envelopes, totals, member
amounts, versioned contribution IDs, lineage and frontier, normalizing only the
engine label. `duckdb_question.rs` covers the unchanged scrap-yield fixture,
correction/deletion with receipt-maintained IVM equality, signed rounding, zero denominators, large coefficients,
hidden contributions, unsupported precision and native feature selection.
Unequal-declared-scale scenarios exercise correction/retraction at scale pairs
0/3, 6/0 and 0/6, including zero input. A generated property varies distinct
operand scales 0–6, exact signed coefficients, visibility and sum/ratio plans.

This carries the approved #426 adapter onto the current query prerequisite;
#422's lineage-dependent fan-out test remains separate. Evidence is runtime
and sampled SQL/FFI correspondence; no new Rust arithmetic kernel or Kani
input is introduced. Historical selection, governed SQL authority and
Iceberg/WellDB persistence remain TARGET.

## Scrap-yield incremental acceptance

DATA-INCREMENTAL / DATA-RESTATEMENT-DELTA bind QUERY-ENGINE-1,
COMPUTE-DETERMINISTIC-1 and RESTATE-HISTORY-1 to the unchanged pinned CSV.
`crates/norn-cell/tests/scrap_yield_incremental.rs` starts without G4, adds
83.300/85.000 g, and requires K1 totals 294.000/300.000 g, ratio 0.98 and loss
6.000 g. The K2 correction requires 292.500/300.000 g, ratio 0.975 and loss
7.500 g: signed recovered delta -1.500 g, input delta zero and loss delta
+1.500 g. Only G2's versioned contribution ID changes; K1 remains reproducible
after K2 and a real SQLite reopen.

At every receipt and duplicate delivery, both live and knowledge-cutoff IVM
must equal full recomputation at that exact frontier. Equality covers the
entire result envelope, including decimal values, member versions, exclusions,
scope, definitions, mappings and source progress; only the engine label differs.
Every generated sequence includes insertion, update, deletion, redelivery and
restart, followed by generated amount, metal and effective-week changes.
Forward/reverse runs reorder independent identities within each batch and the
initial fixture, preserving each identity's update order. Their values and
versioned contribution IDs must agree; their physical ledger hashes may differ.
This does not qualify arbitrary out-of-order corrections to one identity.

`restatement_activation.rs` also checks these signed deltas and full same-slice
recomputation after the existing governed atomic activation. The new evidence
is runtime-enforced scenario assertions plus sampled generated correspondence.
It adds no evaluator or arithmetic kernel; the existing bounded arithmetic
Kani harness remains its arithmetic evidence. Synthetic native receipts do not
qualify mapped-feed authority, training knowledge, OLAP or a demo surface.
