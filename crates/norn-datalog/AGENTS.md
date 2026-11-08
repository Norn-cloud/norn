# norn-datalog boundary

- Ring 0: pure, synchronous, sans-IO, portable to wasm32-unknown-unknown.
- Only Norn edges: norn-canonical (canonical encoding), norn-graph (bounded
  traversal/order), norn-ir (shared fact data/schema). Third party: existing
  serde only; serde_json is test-only. See docs/architecture/decisions/2026-10-24-datalog-contract.md.
- Rule AST, shared validator/diagnostic codes, provenance shape and fixed AST
  packs only. No executor, Ascent generation, DB/service, permission inference,
  authorization, findings plumbing, host API, or kernel commit integration.
- All validation work is bounded. No functions/value invention; all aggregates
  are refused until a later finite-domain lattice implementation is admitted.
- norn-ir owns the one declared metadata projection and never validates it.
  Missing semantic coverage cannot be interpreted as success or admission.
