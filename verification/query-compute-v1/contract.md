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

## G2a arithmetic profile

COMPUTE-DETERMINISTIC-1 uses signed scale-six coefficients, bounded per input
by 9e18 and per sum by 9e21 (the 1,000-object query bound). Sums never round.
A same-unit ratio rounds exactly once, midpoint away from zero, at scale six;
zero divisors refuse in the arithmetic kernel. The analytical evaluator must
represent zero-input yield as unavailable. This profile covers #353 without
adding a domain operation or changing Lane A's ordered-fold contract.

Native scenarios cover 98%/97.5%, signed ties, zero and range refusals.
Generated properties cover commutative sum and ratio error/sign symmetry.
The Kani harness includes the production module: full-width checked-sum/ratio safety,
zero-divisor refusal, and exhaustive i16 numerators with signed 128 divisors
(nearest, tie-away and sign correspondence). This does not claim a
universal arithmetic proof or an evaluator/frontier qualification.
