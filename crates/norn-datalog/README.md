# norn-datalog

Shared portable Datalog contracts for the future definition and record
backends (Norn-cloud/norn#565). This crate contains data and structural validation;
it does not execute rules or produce production findings.

`Program` is a closed serde AST; `Program::canonical_json` delegates to
`norn-canonical`. Array order is preserved, so this is canonical wire encoding,
not a logical-equivalence digest. Symbols are ASCII; text constants must be
NFC so canonical encoding cannot conflate two admitted constants.

`validate(program, Budget)` returns deterministic SCCs and minimum strata or
one stable `Code`. Inputs are explicitly declared finite relations; derived
relations require rules. Head and negative variables must occur in positive
premises. Positive recursion is admitted; recursion through negation, undefined
symbols, inconsistent arities and duplicate IDs/signatures are refused.
Function/constructor/arithmetic/fresh-value syntax is absent. Every aggregate
is explicitly rejected, including min/max, pending sound finite-domain lattice
semantics and matching implementations. No termination claim covers arbitrary
executor bags, unbounded inputs or witnesses fed back into facts.

The validator reuses `norn-graph` bounded forward/backward reachability and
topological ordering with one conservative shared work budget. SCC membership
is forward/backward intersection. This is intended for small bounded rule
packs; it does not add a general graph framework. Input decoding and canonical
encoding are separate from validator budgets; callers must bound input bytes.

`norn_ir::facts` owns the versioned declared metadata projection. It preserves
source identity, version and exact scope, without admission or inheritance
resolution. Missing projections (`role_can`, `law_forbids`, general field
writers, semantic mappings, effective closure, record cardinality and type
disjointness) are explicit gaps, never empty relations proving absence.

`overlap::declared_metadata_v1` checks declared inheritance cycles, disjoint
field/link member namespaces and explicit direct-parent cardinality widening.
Its tuple schema comes from `norn-ir`; every join retains scope. Callers must
isolate catalog snapshots as well. Validation and entailment modes label rule
intent; dependency helpers still participate in validation and cannot be
removed by simply filtering modes. Runtime OWL entailment remains unapproved.

`Provenance` is a witness sidecar at an explained class/frontier, carrying rule
ID and minimal derivation height. Height can rise on retraction. Witnesses do
not participate in fact identity, distinctness or recursion. This crate neither
computes nor authenticates them. L1 soundness proofs, executor parity, Ascent
code generation and WellDB integration are later work.

## Boundary

- Ring 0: pure, synchronous, sans-IO, portable to wasm32-unknown-unknown.
- Only Norn edges: norn-canonical (canonical encoding), norn-graph (bounded
  traversal/order), norn-ir (shared fact data/schema). Third party: existing
  serde only; serde_json is test-only. See docs/platform/datalog-contract.md; the machine-checked rules are in `boundary.json`.
- Rule AST, shared validator/diagnostic codes, provenance shape and fixed AST
  packs only. No executor, Ascent generation, DB/service, permission inference,
  authorization, findings plumbing, host API, or kernel commit integration.
- All validation work is bounded. No functions/value invention; all aggregates
  are refused until a later finite-domain lattice implementation is admitted.
- norn-ir owns the one declared metadata projection and never validates it.
  Missing semantic coverage cannot be interpreted as success or admission.
