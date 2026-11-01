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

## Action-order adapter

The alpha adapter forgets lifecycle state, data guards and cross-object effects,
retaining every action as a self edge. This is a complete overapproximation of
ordering, so singleton-role separation remains sound. Role conjuncts intersect;
permit rules union. Resource restrictions and non-role conjuncts are erased.
Generic grants, unbounded role grants, unsupported policy forms and undecodable
inputs yield unknown rather than an incomplete graph.

Only the closed, empty-schema, effect-free action fragment on a property-free,
link-free, lifecycle-free object supplies concrete witnesses. Other collisions
remain unknown. Required witnesses fail closed outside this fragment. Inherited
controls are checked for every subtype against the full base plus candidate pool,
including when a candidate adds grants without editing the controlled entity.
