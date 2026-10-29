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
checks the total theorem. The theorem quantifies over all current 13
`ArtifactKind` variants and all seven `ArtifactLifecycle` variants. It proves
that direct writes reject `Approved` and `Active` for the 12 authority-bearing
kinds, including `Mapping`, while the evidence-only `SourceSchema` kind remains
directly writable at every lifecycle.

The promoted Rust integration tests cover the 12 authority-bearing kinds for
`Draft`, `Proposed`, `Approved`, and `Active`, then cover all 13 kinds for
`Deprecated`, `Superseded`, and `Rejected`. They do not currently assert the
`SourceSchema` behavior for `Approved` or `Active`; the unconditional
kind-by-lifecycle theorem is the exact table for those cases.

This is an exact correspondence claim for that predicate and those extracted
representations only. It does not prove that every API or storage callsite uses
the predicate, that a caller derived the kind/lifecycle faithfully, or that the
BAC approval, verification-receipt, active-pointer, persistence, authentication,
authorization, or concurrency protocols are correct. Those remain under
`INV-ART-AUTHORITY-PROMOTION`, which stays `partial`. A future enum variant or
predicate representation change must regenerate the hax artifact and re-check
the total theorem and exhaustive runtime table before this obligation can remain
`implementation_proved`.
