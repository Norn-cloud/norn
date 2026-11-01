# Finite controls verdict contract

Controls describe ordered sensitive action sequences on the enclosing object
 type. They confer no runtime authority. Safety quantifies over singleton roles,
 after an unrestricted reachable prefix; availability is a separate judgment.

The adapter supplies a complete overapproximation of effective actions and grants.
Missing references, opaque effects, unsupported inheritance or exhausted budgets
produce unknown, never a successful check. No edge or grant may be silently dropped.

| Finite search evidence | Concrete interpretation | Safety | Availability |
| --- | --- | --- | --- |
| separated | either | proven | unknown |
| unreachable in complete overapproximation | either | proven | unreachable |
| nonempty violating trace | yes | refuted | witnessed |
| possible violating trace | no | unknown | unknown |
| missing trace, invalid graph, exhausted search | either | unknown | unknown |

Named invariants:

- **No promotion:** invalid, exhausted and possible counterexamples remain unknown.
- **Concrete witness:** witnessed requires an inspectable nonempty concrete trace.
- **Nonvacuity:** proven separation never implies witnessed availability.
- **Refusal:** any refuted verdict produces an error and refuses installation.
- **Required witness:** requireWitness refuses admission unless availability is witnessed.
- **Projection:** CheckedArtifactSet.controls carries each declaration's verdict in
  deterministic object type / declaration order; registry consumers preserve it verbatim.
- **Closure:** inherited controls cannot disappear through a subtype or an overlay.

The evidence truth table is exhaustively exercised by controls module tests.
Adapter conformance tests must exercise refusal, unknown, closure and witness
requirements through artifact-set admission, rather than reconstructing this table.
Evidence is finite model evidence, never a temporal proof or a live-runtime claim.
