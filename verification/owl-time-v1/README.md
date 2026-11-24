# OWL-Time interval algebra and temporal boundary laws

Checks for the algebra Norn owns in its temporal model (#1713): Allen's thirteen
interval relations over half-open intervals, the boundary between adjacent
intervals, canonical UTC timestamps and their ordering key, and the
separation of civil days from physical durations.

Calendar arithmetic and timezone rules are not re-proved here. `jiff` and the
SHA-256-pinned offline tzdb bundles in `crates/norn-expr/data/tzdb/` own leap
years, month lengths and offset transitions.

## The model

An interval is `[start, end)` with `start < end` (Core 01 temporal context):
the start instant is in the interval and the end instant is not. The
`norn-zen/7` builtins `intervals_overlap`, `interval_gap_seconds` and
`interval_intersection_seconds` use this reading and refuse `start >= end`.

`crates/norn-expr/src/temporal/allen.rs` states the thirteen relations by
their endpoint conditions (`AllenRelation::holds`) and classifies a pair by
comparing endpoints (`allen_relation`). `AllenIntervals.lean` states the same
thirteen conditions and the same comparison order over `Int`.

| Relation (`a` to `b`) | Condition | OWL-Time property |
|---|---|---|
| `Before` / `After` | `a.end < b.start` / `b.end < a.start` | `intervalBefore` / `intervalAfter` |
| `Meets` / `MetBy` | `a.end = b.start` / `b.end = a.start` | `intervalMeets` / `intervalMetBy` |
| `Overlaps` / `OverlappedBy` | `a.start < b.start < a.end < b.end` / `b.start < a.start < b.end < a.end` | `intervalOverlaps` / `intervalOverlappedBy` |
| `Starts` / `StartedBy` | `a.start = b.start`, `a.end < b.end` / `b.end < a.end` | `intervalStarts` / `intervalStartedBy` |
| `During` / `Contains` | `b.start < a.start`, `a.end < b.end` / `a.start < b.start`, `b.end < a.end` | `intervalDuring` / `intervalContains` |
| `Finishes` / `FinishedBy` | `a.end = b.end`, `b.start < a.start` / `a.start < b.start` | `intervalFinishes` / `intervalFinishedBy` |
| `Equals` | `a.start = b.start`, `a.end = b.end` | `intervalEquals` |

## Laws and evidence

Each row names what was checked and how far the check reaches.

| Law | Evidence | Where |
|---|---|---|
| Exactly one relation holds for any two proper intervals (jointly exhaustive, all 78 pairs of distinct relations disjoint), and it is the one the comparison classifier returns | checked proof over `Int`, reference semantics | `AllenIntervals.lean`: `thirteen_way`, `pairwise_disjoint`, `exactly_one`, `holds_iff_classify` |
| The same, for the Rust `AllenRelation::holds` and `allen_relation` | bounded: every pair of proper `i64` intervals | `allen_kani.rs`: `exactly_one_relation_holds_and_the_classifier_returns_it` |
| Converse dualities (`before`/`after`, `meets`/`metBy`, `overlaps`/`overlappedBy`, `starts`/`startedBy`, `during`/`contains`, `finishes`/`finishedBy`, `equals`/`equals`) | checked proof (reference); bounded over `i64` (Rust) | `holds_iff_converse`, `classify_swap`; `the_converse_holds_for_the_swapped_intervals` |
| Two intervals share no instant exactly when the relation is before, after, meets or met-by; the `intervals_overlap` test `aStart < bEnd && bStart < aEnd` is true exactly when they share one; adjacent `[a, b)` and `[b, c)` meet without overlapping | checked proof (reference); bounded over `i64` (Rust) | `shareNoInstant_iff`, `intersects_iff_shared_instant`, `intersects_iff_not_apart`, `adjacent_meet_without_overlap`; `sharing_an_instant_is_the_half_open_overlap_test` |
| `before`, `after`, `during`, `contains` and `equals` are transitive; `meets` then `meets`, `before` then `meets`, `meets` then `before`, and `during` then `before` all give `before` | checked proof (reference); bounded over `i64` (Rust) | `before_trans` … `during_before_before`; `before_during_and_meets_compose` |
| An improper interval has no relation | bounded over `i64` | `improper_intervals_have_no_relation` |
| The classifier, the thirteen predicates and the three shipped interval builtins agree | exhaustive-finite: all 225 ordered pairs of the 15 proper intervals with endpoints in `0..6`, and all 3,375 triples for the compositions | `crates/norn-expr/tests/temporal_intervals.rs`: `allen_relations_partition_the_grid_and_agree_with_the_interval_builtins` |
| Lexicographic order on `(seconds, nanoseconds)` keys with `0 ≤ nanoseconds < 10⁹` is the order of the instants, and equal keys are equal instants | checked proof over `Int` | `AllenIntervals.lean`: `key_lt_iff_instant_lt`, `key_eq_iff_instant_eq` |
| `canonical_timestamp` returns the UTC reading of the instant, is idempotent (`canonical(canonical(t)) = canonical(t)`), and `timestamp_ordering_key` is that instant exactly | exhaustive-finite: 115,200 spellings `2026-03-09THH:MM:SS[.FFF]±HH:MM` (every hour, five minutes, two seconds, two fractions, 240 offsets up to ±23:59), against the instant computed by plain arithmetic on the fields | `crates/norn-expr/tests/timezone_corpus.rs`: `canonical_timestamps_and_keys_are_the_exact_instant_on_a_day_grid` |
| The same idempotence, and keys ordering canonical UTC timestamps strictly as their instants, across years `0000`–`9999` and nanosecond fractions | sampled: the admitted inputs of the recorded timezone corpus (38,350 inputs), with the instant read independently by `jiff` | `timezone_corpus.rs`: `canonical_timestamps_are_idempotent_and_keys_preserve_instant_order` |
| A civil day is 23, 24 or 25 physical hours (23.5 and 24.5 on Lord Howe Island) according to the pinned zone rules; a nonexistent wall time is refused under every `DstPolicy`; a repeated wall time is refused under `reject` and its `earlier` and `later` sides are the fold apart | sampled: eight real days in three zones, every wall-clock minute, under tzdb `2025b` and `2026c` | `temporal_intervals.rs`: `civil_days_keep_their_physical_length_and_dst_gaps_fail_closed` |
| Cairo's clocks spring forward at local midnight, so 2025-04-25 has no `00:00`; resolving it is a refusal, not a 24-hour day | sampled: that day, both bundles, all three policies, and the same expression through the evaluator | same test |

## What is not claimed

- The Lean file proves properties of its own definitions over `Int`. It does
  not prove correspondence to the Rust code; `allen_kani.rs` checks the Rust
  code directly, for `i64` endpoints only. Timestamps are compared in
  production as `jiff` civil date-times or as `(seconds, nanoseconds)` keys;
  the Rust classifier is generic over `Ord`, and the Kani run does not cover
  those types' `Ord` implementations.
- No Kani run covers `canonical_timestamp` or `timestamp_ordering_key`; their
  evidence is the exhaustive-finite grid and the sampled corpus above. A Kani
  harness over `2026-03-09THH:MM:SS.FFF±HH:MM` was tried and not kept, for two
  reasons. `cargo kani -p norn-expr` does not build: `norn-ir` drops
  `ConceptRole`'s serde derives under `cfg(kani)` for its standalone harness,
  and the rest of `norn-ir` needs them. With that worked around on a scratch
  copy, CBMC did not finish either harness (key equals instant, idempotence)
  within 20 minutes on a 4-vCPU Boat VM; the cost is in symbolically executing
  `jiff` date arithmetic and `core::fmt`.
- The civil-day checks cover the listed days, not every zone or year.
- Nothing here checks `jiff`'s calendar arithmetic or the tzdb contents.

## Running

`scripts/verify-promoted-subsystems.sh` runs both formal gates:

```sh
# owl-time-allen-lean (fast profile)
lean -DwarningAsError=true verification/owl-time-v1/AllenIntervals.lean

# owl-time-allen-kani (full profile)
env RUSTUP_HOME=target/verification-tools/rustup KANI_HOME=target/verification-tools/kani-home \
  kani --fail-fast --output-format terse verification/owl-time-v1/allen_kani.rs

# Rust tests
cargo test --locked -p norn-expr --test suites -- temporal_intervals:: timezone_corpus::
```

The Lean file has no `sorry` and declares no axiom. Its theorems depend only
on Lean's standard axioms `propext`, `Classical.choice` and `Quot.sound`
(`#print axioms`).
