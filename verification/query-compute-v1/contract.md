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
mapping/population/epoch, contributions and frontier. Historical knowledge
selection refuses; retained old results are not a historical-query API.
The unchanged #353 CSV is a generated numerical fixture, not authenticated
training knowledge. Native cell scenarios cover 98%/97.5%, immutable prior
results, exact contribution membership, unavailable zero-input ratios and
visibility/quarantine separation. Governing commit-read qualification follows G4.

QUERY-FRESH-1 analytical labels: complete coverage at the observed frontier with zero heartbeat lag is exact; positive lag within the admitted maximum is fresh; excessive/future heartbeat age is stale; missing coverage or quarantine is incomplete. Governed freshness refusal remains G4.

G3 aggregate mechanism (COMPUTE-DETERMINISTIC-1 / QUERY-ENGINE-1): cell recomputation uses exact signed contribution adjustment designed for delta-maintenance reuse. Retraction reverses an admitted addition without rounding; only the final ratio rounds. A generated sequence/reverse-sequence property and production arithmetic Kani retraction harness qualify this primitive. Receipt-driven maintenance and restart correspondence are the next cut; no IVM qualification is claimed here.

## G3 receipt-maintained generations
QUERY-ENGINE-1 / COMPUTE-DETERMINISTIC-1: maintenance follows gapless verified cell receipts, retracts each changed identity's prior contribution and adds its new projection. Duplicate written IDs and same-frontier delivery cannot double count. Complete source membership enforces the original population bound even for hidden/noncontributing rows. Definition, world/scope, mapping, population or authorization changes require explicit rebuild; a skipped receipt refuses with `core.compute.stale_input`.
A staged generation publishes atomically only after all reads and exact adjustments succeed. Restart closes/reopens the real cell and performs a labelled complete rebuild; no partial generation is exposed. The generated DATA-DELTA correspondence scenario compares the full envelope/rows/member versions and lineage with native recompute after corrections, insertions, deletions, effective-time changes, quarantine/recovery, duplicate delivery and restart. This is sampled implementation correspondence, not a universal IVM proof. OLAP and governed read fencing remain separate qualifications.

## G4 governed-use boundary
QUERY-FRESH-1 / QUERY-STALE: `bind_governed_read` consumes the kernel's one-use `ActionReferenceReads` proof. Fixed point dependencies delegate to `with_read_guards`; staged attachment preserves the original proposal on any refusal. The existing cell-host commit path performs the actual version check.
Every current question has root membership without a captured Lane A partition proof. Governed root use therefore refuses with `core.compute.stale_input`, including exact/complete empty roots. Fresh, stale, incomplete and IVM envelopes also refuse. Metadata or forged envelope labels cannot grant authority. This adapter introduces no new collection fence or authorization decision; Lane A owns the extension on #324.
Named tests exercise a real kernel-prepared action, successful point fencing, a concurrent point writer, no partial guard attachment, and native root-result refusals. Generated flags and a production-policy Kani harness establish the finite refusal classification; the host transaction and opaque proof are covered by conformance, not a Kani proof.

## G5 native numeric drill-through
LINEAGE-SUM-1 / DATA-LINEAGE (numeric path): `trace_question` re-evaluates the admitted plan under current host population visibility and requires the expected exact/complete result lineage pin. One transaction and shared decode budget cover membership, immutable canonical fact incarnations and authenticated emitting receipts. A revision/frontier/population/epoch change requires a fresh result. Exact operand sums are reconciled using the existing aggregate arithmetic; fact-version IDs remain ordered and catalog enrichment cannot enter numeric membership. Canonical values require a separate whole-record disclosure grant; denial remains explicit and does not alter amounts.
Native scenarios cover the unchanged 294/300 fixture, receipt/version correspondence, masked values, revocation and changed/nonexact generations with no disclosure or writes. Generated populations independently sum input/recovered grams and check identity/amount preservation under redaction. No new pure arithmetic or bounded standalone kernel is added: existing arithmetic proptests/Kani cover exact adjustment; the new transaction/SQL/callback operation is runtime correspondence, not a Kani proof. The returned `source_edge_gap` is explicitly `source edge: not yet (#401)` until D2 exposes its authority-checked bundle-provenance read API. This cut stops at canonical facts/receipt references: source-document, mapping and source-digest resolution and knowledge-time exports remain separate qualifications coordinated with Core 07/Lane H.

## G6 numeric QUERY-FANOUT correspondence
QUERY-GRAIN-1: the native root-grain sum returns 80 for one Holding linked to two real Tag objects; two distinct holdings each valued at 80 return 160. Numeric contributions are keyed by holding identity/version, not measure value or catalog multiplicity. Typed Rust fixture definitions compile through the existing IR/checker and use actual native objects/links. No monetary/currency or joined-dimension execution is qualified.
The named expanded-tag case refuses ambiguous grain; symmetric linked grouping stays unsupported. A generated native population varies scalar amounts and tag multiplicities, commits link-only receipts and compares complete IVM/recompute envelopes plus independent root sums and unchanged numeric rows. This test-only cut adds no pure engine function or Kani target; the existing exact-arithmetic harnesses remain unchanged.

## G7 embedded SQL correspondence (CP3 stretch)
QUERY-ENGINE-1: the optional `norn-cell/duckdb` feature lowers the same admitted sum/ratio plan over its authorized, bounded, exact contribution snapshot. Shared projection retains predicates/time buckets; generated SQL stores each identity/version and nullable operand in declared-scale DECIMAL, computes keyed sums and rounds ratios once using HUGEINT quotient/remainder. Values are bound parameters; arbitrary SQL, extension loading, external access, integer measures and declared scales above six refuse. Iceberg/WellDB persistence remains TARGET.
The correspondence obligation is canonical equality with cell rows, exact operand totals/amounts, contribution identities/versions, lineage and frontier after normalizing only the physical engine label. Named runtime scenarios cover #353 scrap-yield, correction/retraction, signs/midpoints, zero denominators, large coefficients and unsupported precision. Generated populations cover sums/ratios and visibility filtering, including an empty visible population. Evidence is sampled embedded-engine correspondence; SQL/FFI cannot be modeled by the existing Kani harness, which still qualifies only shared Rust arithmetic. No historical knowledge selector or governed SQL read is qualified.
