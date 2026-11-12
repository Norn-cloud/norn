# norn-datalog

Shared portable Datalog contracts for the definition and record backends
(Norn-cloud/norn#565): the rule AST, structural validation, and a small
reference executor that runs an admitted program over finite input facts. It
produces derived relations as data, never production findings or
authorization decisions.

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

`evaluate(program, facts, Budget, ExecBudget)` admits the program with
`validate` first; a refused program never runs. It evaluates bottom-up and
semi-naively, one stratum at a time, so a negated relation is complete before
any rule reads it. Facts may be supplied only for declared input relations,
with the declared width and NFC text (`datalog.derived_in_input`,
`datalog.arity_mismatch`, `datalog.noncanonical_constant`,
`datalog.undefined_predicate`). The active domain is fixed before the first
rule runs and constants are numbered in canonical-encoding order, so the
result and the metered work depend only on the set of input facts: not on
their order, repeats, or rule order. Output relations are in name order and
their tuples in canonical order. `ExecBudget` bounds supplied facts, text
bytes, derived facts and work (one unit per value read, per tuple a join
scans, per negation probe and per fact produced); reaching any limit refuses
with `datalog.budget_exhausted`, naming the running rule, and returns nothing
partial. Joins scan in body order using ordered prefix ranges; there is no
query planner, so a pack should order its premises from most to least bound.

The executor is the reference semantics, not a replacement for generated
backends: the checker's inheritance step still runs as generated Ascent code,
and its exhaustive small-graph test now checks the Ascent step, the old
oracle and this executor running the recursive overlap pack against each
other.

`Provenance` is a witness sidecar at an explained class/frontier, carrying rule
ID and minimal derivation height. Height can rise on retraction. Witnesses do
not participate in fact identity, distinctness or recursion. This crate neither
computes nor authenticates them, and the executor does not emit them. L1
soundness proofs, general executor/Ascent parity and WellDB integration are
later work.

## Boundary

- Ring 0: pure, synchronous, sans-IO, portable to wasm32-unknown-unknown.
- Only Norn edges: norn-canonical (canonical encoding), norn-graph (bounded
  traversal/order), norn-ir (shared fact data/schema). Third party: existing
  serde only; serde_json and proptest are test-only. See docs/platform/datalog-contract.md; the machine-checked rules are in `boundary.json`.
- Rule AST, shared validator/diagnostic codes, provenance shape, fixed AST
  packs and the reference executor. No Ascent generation, DB/service,
  permission inference, authorization, findings plumbing, host API, or kernel
  commit integration.
- The executor is metered and refuses rather than runs away; it takes finite
  facts from its caller and performs no IO.
- All validation work is bounded. No functions/value invention; all aggregates
  are refused until a later finite-domain lattice implementation is admitted.
- norn-ir owns the one declared metadata projection and never validates it.
  Missing semantic coverage cannot be interpreted as success or admission.
