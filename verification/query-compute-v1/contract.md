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
