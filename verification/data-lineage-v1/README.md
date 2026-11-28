# `data-lineage-v1` Formal Specification Proofs

Standalone Lean 4 (`-DwarningAsError=true`, zero `sorry`, `import Std` only;
CI checks the Lean version via `scripts/check-verification-tools.sh`) reference
proofs for the constitutional `data` ontology pack
([`registry/packs/data/data.ts`](../../registry/packs/data/data.ts)).

## Scope and Honest Evidence Labeling

- **Evidence class:** `checked proof` (reference specification model).
- **What is proved here:** Mathematical properties of the abstract lineage,
  interval-algebra, and role-partition models, with explicit separation between
  laws enforced by the current `data.*` IR and stronger conditional hypotheses.
- **What is NOT proved here:** Declaration lowering from TypeScript to IR,
  correspondence to Rust checker/kernel execution, runtime commit enforcement,
  or non-vacuity of concrete packs. Those are checked separately by
  `tools/norn-ts/test/data-pack.test.ts` (`structural` + `sampled`) and
  `crates/norn-checker/tests/data_ontology.rs` (`structural` + `exhaustive-finite`
  for pinned lifecycle bounds).

## Proved Theorems (`DataLineage.lean`)

1. **`DATA-CAUSAL-1` (`data_causal_1_monotonic_transitive`,
   `data_causal_1_no_backward_leakage_monotonic`,
   `data_causal_1_transitive_no_future_leakage`,
   `window_overlap_alone_allows_retrograde_output`)**:
   - **Enforced by `data.Derivation`:** `data_causal_1_monotonic_transitive`
     matches `data.Derivation.monotonic_knowledge_time` (`input.knownAt ≤
     output.knownAt`) and proves `s₀.knownAt ≤ sₙ.knownAt` along any finite
     derivation path `s₀ →* sₙ`.
   - **Stronger sufficient condition (not enforced by the base pack):**
     `data_causal_1_transitive_no_future_leakage` shows `input.knownAt ≤
     activity.endsAt ∧ activity.endsAt ≤ output.knownAt` also entails monotonic
     knowledge time. `window_overlap_alone_allows_retrograde_output` witnesses
     why `input.knownAt ≤ activity.endsAt ∧ activity.startsAt ≤ output.knownAt`
     alone is insufficient without `monotonic_knowledge_time`.
2. **`DATA-ACYCLIC-1` — Conditional (`data_acyclic_1_strict_progression`,
   `data_acyclic_1_from_proper_activity`,
   `monotonic_alone_allows_two_snapshot_cycle`)**:
   - **Conditional (stronger than the base pack):** Holds if a composing pack
     strengthens `monotonic_knowledge_time` to strict progression
     (`input.knownAt < output.knownAt`) or requires `input.knownAt ≤
     activity.startsAt ∧ activity.endsAt ≤ output.knownAt` over proper
     intervals (`startsAt < endsAt`), ruling out every non-empty cycle
     (`∀ s, ¬ NonEmptyPath G s s`).
   - **Gap witness:** `monotonic_alone_allows_two_snapshot_cycle` proves that
     the base pack's four `data.Derivation` laws admit a 2-cycle `A → B → A`
     when `A ≠ B` share the same `knownAt`. (`acyclic: true` on
     `data.SnapshotRecord.supersedes` governs snapshot revisions, not derivation
     edges.)
3. **`TIME-ALLEN-1` (`time_allen_1_exhaustive`, `time_allen_1_pairwise_disjoint`,
   `time_allen_1_partition`)**:
   Self-contained partition slice mirroring
   [`verification/owl-time-v1/AllenIntervals.lean`](../owl-time-v1/AllenIntervals.lean)
   (already checked by `owl-time-allen-lean`, which also proves the classifier,
   converse dualities, scheduling compositions, and pairs with `owl-time-allen-verus`):
   every pair of proper half-open intervals `[startsAt, endsAt)` satisfies
   *exactly one* of Allen's 13 relations.
4. **`BFO-DISJOINT-1` (`GRAPH-ROLE-1`) & Target `DECLARED-DISJOINT`**:
   - **Derived from `GRAPH-ROLE-1` (enforced by `norn-checker` today):**
     `bfo_role_partition`, `bfo_continuant_occurrent_disjoint`,
     `bfo_disjoint_1_observation_claim` (`Observation ⊓ Claim = ∅`), and
     `bfo_disjoint_1_snapshot_activity` (`DatasetSnapshot ⊓ Activity = ∅`).
     (`data.QualityFinding` is modeled in Norn as a recorded assessment act with
     role `happening` rather than a pure BFO information continuant.)
   - **Target `disjointWith` contract (pending PR #1781):**
     `declared_disjoint_observation_economic_event` (`Observation ⊓ EconomicEvent = ∅`)
     and `declared_disjoint_observation_commitment` (`Observation ⊓ Commitment = ∅`)
     project the assumed `declaredDisjoint` hypothesis since both sides have
     role `happening`.

## Gate

Checked in CI by the `data-lineage-lean` fast gate in
`scripts/verify-promoted-subsystems.sh` (enrolled as a `supporting-check` under
`assurance-certificates` in `verification/coverage-v1/promoted-gates.json`):

```bash
lean -DwarningAsError=true verification/data-lineage-v1/DataLineage.lean
```
