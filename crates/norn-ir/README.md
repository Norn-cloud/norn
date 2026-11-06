# norn-ir

`norn-ir` is a ring-0 crate (pure, portable semantic core): typed IR enums, serde, and schemars, with no validation. Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`. Its former `validate_*` functions and the logic built on them (sealing, ontology packs, the quantity-ledger checker and generic posting admission) moved to `norn-checker` in T1-CHK-1 (#2524); the guard's `validate_*` allowlist for this crate is empty.

# Direct artifact-write predicate correspondence

Obligation `INV-ART-DIRECT-WRITE-PREDICATE` is deliberately narrower than the
artifact-promotion protocol. It binds one pure production Rust function:

- source: `norn_ir::direct_artifact_write_allowed` in
  `crates/norn-ir/src/lib.rs`;
- extracted definition:
  `target/verification-results/artifact-governance-hax/Norn_ir.fst`;
- checked theorem:
  `ArtifactPromotionProof.direct_artifact_write_truth_table` in
  `verification/stripmine-2026-08-22/proofs/fstar/ArtifactPromotionProof.fst`.

The promoted extraction command selects only that Rust symbol with hax, rejects
proof holes/admitted SMT queries, checks the generated F* module, and then
checks the total theorem. The theorem quantifies over all current 15
`ArtifactKind` variants and all seven `ArtifactLifecycle` variants (105
combinations). It proves that direct writes reject `Approved` and `Active` for
the 14 authority-bearing kinds, including `Mapping`, while the evidence-only
`SourceSchema` kind remains directly writable at every lifecycle.

The promoted Rust integration tests check 101 of those 105 combinations: the 14
authority-bearing kinds for `Draft`, `Proposed`, `Approved`, and `Active`, then
all 15 kinds for `Deprecated`, `Superseded`, and `Rejected`. They do not assert
`SourceSchema` with `Draft`, `Proposed`, `Approved`, or `Active`; only the
kind-by-lifecycle theorem covers those four.

Until #969 provides an isolated producer, this obligation is an "F\* re-checked
theorem about the uploaded model, correspondence candidate-reported", recorded
as `partial`. A fresh verifier re-typechecks the uploaded F\* module and the
theorem with its own pinned F\*, but hax runs on a producer that compiles
candidate code, so whether that module is the extraction of the Rust function is
only candidate-reported. `implementation_proved` returns only via #969.

The claim covers that predicate and those extracted representations only. It
does not prove that every API or storage callsite uses
the predicate, that a caller derived the kind/lifecycle faithfully, or that the
BAC approval, verification-receipt, active-pointer, persistence, authentication,
authorization, or concurrency protocols are correct. Those remain under
`INV-ART-AUTHORITY-PROMOTION`, which stays `partial`. A future enum variant or
predicate representation change must regenerate the hax artifact and re-check
the total theorem and the runtime table before this obligation can
return to `implementation_proved`.
