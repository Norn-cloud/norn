# Datalog stratification construction: conditional model

Lean 4 model of the max construction that `norn-datalog` uses to assign strata
(`crates/norn-datalog/src/validate.rs`). It reuses Std list induction, natural
number max and `omega`; no graph library, dependency, extraction runtime or
certificate checker is added. The toolchain pin (`lean-toolchain`) matches
`verification/kernel-l1/lean-toolchain`. This is a model of the algorithm, not
a Rust refinement proof, and not a claim that Datalog soundness is complete.
Part of the generated proof backends direction in Norn-cloud/norn#565.

## Law and construction

For each dependency **body → head**, a successful plan must satisfy
`stratum(body) ≤ stratum(head)` for a positive dependency and
`stratum(body) < stratum(head)` for a negative dependency. Equivalently, add
weight zero or one to the body stratum. Summing along a path shows that a
closed path has total negative weight zero. Positive cycles remain admissible.
A resource refusal admits no plan and does not establish a negative cycle.

The model takes a finite list of signed predicate dependencies, a supplied
component-owner map and a supplied component order. `quotient` replaces both
endpoints with their owners. `internalOK` first rejects a negative edge within
one owner. For each component in order, `assign` constructs its value by
`foldMax`: begin at zero, look up each external predecessor, checked-add its
weight, take max, and insert the resulting value. Missing lookups and additions
above `limit` return `none`. This is the max construction, not a post-hoc
inequality checker.

The inductive invariants and named results are:

| Result | Content |
| --- | --- |
| `foldMax_bounds` | The accumulator never decreases; every input edge has a successful predecessor lookup whose value plus weight is at most the constructed result. |
| `foldMax_least` | The constructed result is at most any upper bound on the initial accumulator and those same incoming contributions. These upper bounds are premises of a **leastness** lemma, never premises of the admission soundness theorem. |
| `foldMax_machine_bound` | If the initial accumulator fits, a successful fold result is at most the supplied machine limit. |
| `fresh_tail`, `assign_preserves` | With a duplicate-free order and a table initially absent on that order, each insertion preserves every previously stored predecessor through the remaining iterations. |
| `assign_covers`, `construct_covers` | Successful assignment/construction stores every component listed in the order. |
| `assign_cross_bounds` | For every scheduled head and external dependency, the final table retains both values and their weighted inequality. |
| `internalOK_positive` | Every accepted internal edge has weight zero. |
| `construct_sound`, `stratify_sound` | Successful construction implies the weighted law, first on components and then on the original predicate dependencies through `owner`. |
| `positive_edge`, `negative_edge` | The weighted law gives the requested non-strict and strict inequalities. |
| `path_weight_monotone`, `closed_path_weight_zero` | Inductive paths preserve accumulated weight; every closed path under the law has zero weight. |
| `negative_edge_no_return`, `successful_no_negative_cycle` | An admitted negative edge cannot have a path from its head back to its body, including reflexive return for a self-edge. |
| Refusal/witness lemmas | `budget_abort`, `negative_self_refused`, `missing_predecessor_refused`, `overflowing_increment_refused`; `positive_cycle_admitted` constructs a two-predicate positive cycle at stratum zero. |

The main construction theorems require **only structural order uniqueness and
coverage of dependency heads**, plus successful construction. They never assume
the desired inequalities. These structural premises must be discharged for the
Rust-produced order; they are not new admission preconditions for production.
In particular, an invalid order with an unavailable predecessor cannot produce
`some`. Uniqueness prevents later overwrite of a value used in an earlier max.
`rank` projects stored values, with zero for an absent entry; `construct_covers`
separately establishes that successfully scheduled components are present.

The model theorem needs no claim that owners really are SCCs: internal positive
edges share a value, and successful external lookups enforce the law. SCC
correctness is still required to relate Rust's partition and rejection
diagnostics to graph semantics and to prove completeness. `SCCPartition`
states exact mutual-reachability ownership, including reflexive paths.
`Topology` separately states uniqueness, endpoint coverage and earlier
prerequisites; `Before` uses list positions, not strata. These are definitions
of future obligations, not axioms, supplied proof terms or certified results.
There is no theorem of SCC construction, topological-sort correctness, or
global minimum strata here. Local max leastness does not imply those claims.

## Exact Rust source correspondence inventory

All line numbers below refer to `crates/norn-datalog/src/validate.rs` and
`crates/norn-graph/src/graph.rs` at the git blob IDs pinned below.

| Pinned source | Model and outstanding correspondence |
| --- | --- |
| `crates/norn-datalog/src/validate.rs:305–315` | Body-to-head orientation and negative-if-any occurrence. `Edge` models the signed dependency. Predicate-name enumeration, AST traversal, deduplication, and complete negative marking have only been manually inspected. |
| `validate.rs:327–331` | Declared graph nodes and handoff to stratification. The finite model uses natural-number IDs; a bijection from declared names, with full endpoint coverage, remains required. Isolated predicates must also appear in the Rust plan; the edge-law theorem alone does not express that output completeness. |
| `validate.rs:413–449` | `components` intersects downstream and upstream reachability and skips assigned seeds. Neither the traversal implementations nor the partition loop are translated/proved here. Need seed inclusion, exact traversals on successful bounded calls, complete/disjoint/nonempty groups and owner equivalence `SCCPartition`. |
| `validate.rs:456–469` | `owner` projection. Rust reports `GraphRefused` for an absent owner; the model takes a total owner function. Prove owner coverage and correct finite ID mapping before using the model for Rust; no absent owner may be silently defaulted. |
| `validate.rs:476–491` | `quotient`, `internalOK`, `incoming`. Model retains all signed occurrences while Rust deduplicates predicate pairs and marks a pair negative if any occurrence is negative. Prove that this coalescing preserves the incoming max and internal-negative rejection. Collection/order correspondence is not mechanically established. |
| `validate.rs:493–494`; `crates/norn-graph/src/graph.rs:187–214,268–278` | Graphlib uses prerequisite → successor orientation. Need successful order uniqueness, coverage of all condensed nodes, and prerequisite-before-successor (`Topology`). Only the first two are needed by the conditional safety theorem; full topology is needed for construction completeness and execution ordering. |
| `validate.rs:495–513` | `assign` and `foldMax` follow the production zero/max/lookup/add/insert operations. Functional-table lookup/update versus `BTreeMap` and dependency-list equivalence remain Rust refinement obligations. `GraphRefused` on missing predecessor and `BudgetExhausted` on overflow both project to `none`. |
| `validate.rs:515–534` | Group materialization, recursive flags, owned predicate names and stable sort by stratum are outside the modeled table. Need value-preserving output coverage and stable retention of topological order for equal-stratum positive dependencies. No alphabetical tie-order assumption is made. |
| `validate.rs:405–410` and all `meter` calls | `budgetOK = false` projects a resource abort to `none`. It abstracts all successful charges, graph budgets and final output charges, with no cost or diagnostic-precedence equivalence. It must never map a failed call to `some`. |

Pinned Git blob IDs: `validate.rs` =
`0f12c3d3c4dfbf6b931e383220d9c869315d3fa6`; `norn-graph/src/graph.rs` =
`6c10377f34ab36ad822f67cfd21c510be83625fb`. Re-pin both when either file changes.

Machine arithmetic is conditional on `limit = usize::MAX` for the actual Rust
target. The natural-number model explicitly checks `previous + weight ≤ limit`;
it does not replace checked addition with unbounded success. Refinement still
must show that every stored value is representable, `usize::from(negative)` is
zero/one, the target width is bound, and `checked_add` returns exactly the
corresponding value or overflow refusal. Neither `foldMax_machine_bound` nor
the zero-limit example is a theorem about compiled `usize`. Diagnostic
distinctions, allocation failure and metering are not inferred from `Option`.

## Qualification and axiom audit

Checked on Lean 4.34.1 with `-DwarningAsError=true`: `Stratification.lean`
elaborates with no errors or warnings, and `Audit.lean` prints exactly 22
theorem entries (`axioms.txt`). Seventeen closures are `propext`, sometimes
with `Quot.sound`; the five refusal/witness lemmas depend on no axioms. No
`Classical.choice`, `sorryAx` or other axiom appears.

Reproduce (Lean 4.34.1, from this directory):

```bash
export ELAN_TOOLCHAIN=leanprover/lean4:v4.34.1
test "$(cat lean-toolchain)" = "$ELAN_TOOLCHAIN"
out="$(mktemp -d)"
lean -DwarningAsError=true -o "$out/Stratification.olean" Stratification.lean
LEAN_PATH="$out" lean -DwarningAsError=true Audit.lean | diff -u axioms.txt -
```

Only `propext`, `Classical.choice` and `Quot.sound` are acceptable axioms;
`sorryAx` or any other axiom fails the audit. Rust refinement (the open
obligations in the table above) is not established by this check.

## Status in the coverage manifest

These sources live under `docs/platform`, not `verification/`, on purpose: every
Lean file under `verification/` must be enrolled in the coverage manifest as
evidence for a named obligation claim, and this model does not yet back one.
It is therefore not a promoted gate and credits no theorem to any production
obligation. Promoting it means adding that claim and a gate for the commands
above in a separate change.
