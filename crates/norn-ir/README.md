# norn-ir

`norn-ir` is a ring-0 crate (pure, portable semantic core): typed IR enums, serde, and schemars, with no validation. Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`. Its former `validate_*` functions and the logic built on them (sealing, ontology packs, the quantity-ledger checker and generic posting admission) moved to `norn-checker` in T1-CHK-1 (#2524); the guard's `validate_*` allowlist for this crate is empty.

# Direct artifact-write predicate correspondence

Obligation `INV-ART-DIRECT-WRITE-PREDICATE` is deliberately narrower than the
artifact-promotion protocol. It binds one pure production Rust function:

- source & inline Verus specification: `norn_ir::direct_artifact_write_allowed`,
  `ArtifactKind`, `ArtifactLifecycle`, and the `verus!` truth-table lemmas in
  `crates/norn-ir/src/artifact_governance.rs`;
- promoted gate: `bash scripts/verify-kernel-inline-verus.sh all` (runs in
  `phase/guards` on every pull request and in `scripts/verify-promoted-subsystems.sh`),
  backed by `scripts/kernel_inline_verus.py`, `scripts/kernel_verus_ir.py`, and
  the seeded negative mutants in `verification/mutants/artifact_governance/`.

The inline Verus gate checks the pinned statement digest of
`crates/norn-ir/src/artifact_governance.rs`, verifies the executable predicate
and all five ghost lemmas with `--no-cheating --no-external-by-default
-Zcrate-attr=verus::untrusted`, compares Verus's VIR against Cargo's release
`norn-ir` THIR (D107), and requires each seeded negative mutant
(`allow_approved_law`, `reject_source_schema`, `allow_active_write`) to fail at
the `direct_artifact_write_truth_table` postcondition.

The theorem quantifies over all current 17 `ArtifactKind` variants and all seven
`ArtifactLifecycle` variants (119 combinations, with no preconditions). It
proves that direct writes reject `Approved` and `Active` for the 16
authority-bearing kinds, including `Mapping`, `Law`, `DefinedConcept`, `Labels`,
and `Model`, while the evidence-only `SourceSchema` kind remains directly
writable at every lifecycle.

The promoted Rust integration tests check 115 of those 119 combinations: the 16
authority-bearing kinds for `Draft`, `Proposed`, `Approved`, and `Active`, then
all 17 kinds for `Deprecated`, `Superseded`, and `Rejected`. They do not assert
`SourceSchema` with `Draft`, `Proposed`, `Approved`, or `Active`; the inline
Verus truth-table theorem and `source_schema_always_directly_writable` lemma
cover those four directly.

The claim covers that predicate and its verified Rust source only. It does not
prove that every API or storage callsite uses the predicate, that a caller
derived the kind/lifecycle faithfully, or that the BAC approval,
verification-receipt, active-pointer, persistence, authentication,
authorization, or concurrency protocols are correct. Those remain under
`INV-ART-AUTHORITY-PROMOTION`, which stays `partial`. Any future enum variant or
predicate representation change must update the exhaustive matches, the pinned
statement digest, and the runtime table so both rustc and Verus re-check the
complete truth table.
