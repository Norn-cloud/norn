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
- **Kernel correspondence:** every reported witness replays through kernel action
  admission and commit to the witnessed state. A refused step makes the collision
  unknown, never witnessed or refuted.
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
The alpha adapter retains every action as a self edge, forgetting state, guards and cross-object effects.
This complete overapproximation preserves singleton-role separation.
Role conjuncts intersect; permit rules union. Resource restrictions and non-role conjuncts are erased.
Generic or unbounded role grants, unsupported policies and undecodable inputs yield unknown.
Concrete witnesses use only `upsert_input_object` in unrestricted upsert mode,
on an existing object whose sole property is mutable text `id`. Every step takes
the closed input `{id: "witness"}` and preserves that same object state.
The object has no identity constraints, inheritance, links, invariants, lifecycle,
interfaces or owned collections. Actions require only org scope, have empty output
schemas and no guards, derivations, slices, commands, quantity effects or link creation.
Effect-free actions, create-only upserts, mismatched targets and all other effects
remain abstract. Kernel correspondence tests execute every reported trace step
through `commit_action` on a schema-backed host and check the committed state;
refused effects and guarded steps must remain unknown.
Other collisions remain unknown; required witnesses then fail admission.
Inherited controls are checked on every subtype against the full pool, including candidate grant additions.
