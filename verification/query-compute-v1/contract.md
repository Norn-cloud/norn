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
