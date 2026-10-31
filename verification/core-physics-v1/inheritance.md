# Inheriting the `norn.core` laws

A workflow built only from the `norn.core` primitives inherits their laws; it
does not re-prove them (norn#56, "core laws as axioms"). This page states what
is inherited, under which conditions, and with what evidence. Law ids are in
`obligations.json`; evidence levels are the coverage vocabulary in
`verification/coverage-v1/coverage.json` (subsystem `core-physics`).

## What is inherited

| Law | The primitive that enforces it | Why a workflow inherits it |
|---|---|---|
| `INV-CORE-VALUE-CONSERVATION` | the kernel commit check for every type that declares an accounting leg | the only way to move value is a balanced posting group; `run_balanced` and `run_trial_zero` show every sequence of such steps keeps the trial balance at zero |
| `INV-CORE-POSTED-IMMUTABLE` | the same check refuses updates and deletes of posted legs | every admitted step only adds entries (`run_append_only`) |
| `INV-CORE-LINKED-REVERSAL` | the quantity ledger's linked reversal | a correction is a new, exact, linked negation, at most once (`run_reversed_at_most_once`) |
| `INV-CORE-QUANTITY-CONSERVATION` | the quantity ledger's conserving partitions | received = issued + on hand for every movement history |
| `INV-CORE-ESCROW-NON-NEGATIVE` | the quantity ledger's availability check | holds stay within [0, balance] across every workflow (`qexec_valid`) |
| `INV-CORE-COMMITMENT-COMPLETION` | the commitment lifecycle and the cell deadline timer | terminal states absorb, nothing drops a commitment, and the deadline event ends it |

The runtime face of the first two rows is
`crates/norn-kernel/tests/core_physics_inheritance.rs`. It generates 150
workflows of eight primitive steps (balanced and unbalanced posts, edits and
deletes of posted legs). It checks that admission matches the reference check,
that no posted leg changes, and that the trial balance stays zero in every unit.

## Conditions

- **No bypass.** Value is inherited only for types that declare an accounting
  leg, and quantity only for moves through the quantity ledger. Any other
  write is a boundary, and the inherited claim stops there.
- **Reference, not implementation.** The Lean theorems are about the pack's
  reference model; the Rust kernel is tied to them by tests, not by a
  refinement proof. Nothing here is `implementation_proved`.
- **Commitment completion assumes the timer.** The kernel does not yet check
  that an implementing type's lifecycle refines the commitment lifecycle, and
  the deadline event is assumed to fire; its runtime status is `missing`.

## What still needs its own proof

Cross-primitive and global properties do not follow from the axioms alone:
liveness ("every approved order is eventually invoiced"), totals across cells,
and app-specific invariants. Prove them by assume-guarantee reasoning, with
these laws as the assumptions. A claim that relies on one is labelled
"inherited from law X of pack `norn.core` version Z".
