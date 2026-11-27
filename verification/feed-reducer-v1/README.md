# Feed Observation Reducer & Source-Version Ordering (`FEED-MODE-1`, Lane C3 / #1924)

Standalone Lean 4 reference proofs (`import Std` only, zero `sorry`, no custom
axioms) paired with the zero-dependency production core in
`crates/norn-ir/src/feed_patch_core.rs` (delegated from `crates/norn-ir/src/feed.rs`).

## Laws and evidence

| Law | Evidence | Where |
| --- | --- | --- |
| `FieldPatch` idempotent monoid (left/right identity, associativity, idempotency) and deterministic reducer homomorphism over single patches and arbitrary finite lists | checked proof (reference semantics over polymorphic `Type`, Lean 4.31.0 / 4.34.1 zero warnings / zero `sorryAx`), plus exhaustive-finite Rust tests | `FeedReducer.lean`: `composeField_absent_left`, `composeField_absent_right`, `composeField_assoc`, `composeField_idempotent`, `reduceField_homomorphism`, `reduceField_idempotent`, `reduceField_fold_eq_compose`; `crates/norn-ir/tests/feed_patch_laws.rs` |
| `LinkPatchOp` idempotent monoid and deterministic link reducer homomorphism over single ops and arbitrary finite lists | checked proof (reference semantics over polymorphic `Type`, Lean 4.31.0 / 4.34.1 zero warnings / zero `sorryAx`), plus exhaustive-finite Rust tests | `FeedReducer.lean`: `composeLink_absent_left`, `composeLink_absent_right`, `composeLink_assoc`, `composeLink_idempotent`, `reduceLink_homomorphism`, `reduceLink_idempotent`, `reduceLink_fold_eq_compose`; `crates/norn-ir/tests/feed_patch_laws.rs` |
| Object-level map reducer homomorphism, idempotency, and untouched-key preservation | checked proof (reference semantics over `K → Option V`, Lean 4.31.0 / 4.34.1 zero warnings / zero `sorryAx`), plus Rust integration tests | `FeedReducer.lean`: `composeObj_absent_left`, `composeObj_absent_right`, `composeObj_assoc`, `reduceObj_homomorphism`, `reduceObj_idempotent`, `reduceObj_absent_preserves` |
| Normalized UTC instant `(epochSeconds, nanos)` strict total order agreeing with `totalNanos`, monotonic watermark progression, stale-candidate preservation (leaves watermark unchanged), duplicate idempotency, and opaque non-orderability | checked proof (reference semantics over `Int`, Lean 4.31.0 / 4.34.1 zero warnings / zero `sorryAx`), plus Rust boundary tests | `FeedReducer.lean`: `compareInstant_lt_iff_totalNanos_lt`, `compareInstant_eq_iff_totalNanos_eq`, `compareInstant_gt_iff_totalNanos_gt`, `compareInstant_refl`, `compareInstant_antisymm`, `compareInstant_trans_lt`, `watermarkStep_monotone`, `watermarkStep_stale_ignored`, `watermarkStep_duplicate_idempotent`, `watermarkFold_monotone`, `opaque_unorderable` |

## What is not claimed

- `FeedReducer.lean` proves algebraic properties of its own Lean 4 reference
  definitions (checked on Lean 4.31.0 and 4.34.1 with `-DwarningAsError=true`
  and zero `sorryAx`). It is not yet wired into `scripts/verify-promoted-subsystems.sh`
  and does not yet prove Aeneas translation correspondence against
  `crates/norn-ir/src/feed_patch_core.rs`.
- `crates/norn-ir/src/feed_patch_core.rs` is isolated as a standalone `core`-only
  module (`--crate-type lib --edition 2024`, zero external crate dependencies) so
  a follow-up formal runner can extract it directly with Charon/Aeneas and pin
  correspondence theorems and seeded mutants.

## Running

```sh
lean -DwarningAsError=true verification/feed-reducer-v1/FeedReducer.lean
```
