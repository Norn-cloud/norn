# Shared Datalog contract

Scope: the layering for generated proof backends (Norn-cloud/norn#565);
this records that direction, with no executor or runtime OWL admission.

`norn-datalog` is a pure ring-0 data/validation crate. Both future backends
consume one serde AST and one refusal taxonomy. It reuses `norn-canonical`
for canonical bytes, `norn-graph` for explicitly bounded traversals/order,
`norn-ir` for the fact constants/schema, and the existing `serde`. The SCC
partition uses intersection of existing forward/backward reachability calls;
each call reserves its full bounded allowance from one validator work budget.
This deliberately favors small bounded programs over another graph engine.
No new third-party package, executor, host, or generic framework is admitted.

The accepted fragment is function-free finite-domain Datalog with head and
negative variables bound by positive premises. Positive recursion terminates
by finite active-domain set semantics; negative dependencies must cross to a
strictly lower stratum. All aggregates, including min/max, are refused until
finite-domain lattice semantics and backend parity are admitted. Arithmetic,
constructors, callbacks and fresh values have no AST representation. A
successful structural plan does not bound execution cost or validate input
facts, and is not a proof receipt. L1 still owns validator soundness evidence.

`norn_ir::facts` is the sole typed metadata projection. It does no checking,
base/extension resolution, admission, authorization or expression analysis.
It preserves declared names, duplicates, source identities/versions and exact
envelope scope. `role_can` and `law_forbids` remain explicit coverage gaps:
roles with conjunctive policies and scoped temporal laws cannot soundly be
flattened into those tuples. General field writers, inherited completeness,
semantic mappings, record cardinality and type disjointness remain incomplete.

The initial AST pack covers declared inheritance cycles, field/link namespace
disjointness, and explicitly redeclared direct-parent link cardinality
widening. It does not claim general OWL disjointness or record counts. Every
join retains exact scope; callers must additionally isolate the chosen catalog
snapshot and its versioned dependencies. Validation and entailment intent are
separate annotations; ancestor closure is ordinary relational derivation,
not adoption of runtime OWL. Witnesses are sidecar data (rule ID, minimal
height, class, frontier), never part of fact identity or recursive feedback.

No Ascent generation, WellDB changes, findings execution, database, service,
authorization policy or kernel commit-path integration is part of this slice.

## Addendum: bounded definition inheritance (first Ascent consumer)

`norn-checker` gains `ascent` as a normal dependency (workspace `=0.8.1`,
defaults off, the vendored `no-clock` profile; see
`third_party/ascent/PATCHES.md`) and `norn-datalog` as a build dependency only:
the build script validates the fixed shared AST and projects it at build time.
`quote` is the only additional third-party build edge. There is no direct
`proc-macro2` edge. No normal Datalog consumer or executor is added to
`norn-datalog`, the kernel, or the storage layer.

Evaluation is a metered per-root, one-edge frontier over the complete selected
registered catalog, not all-pairs closure or an interpretation of incomplete
IR facts. Budget refusal stops validation before ancestry/table construction.
Cycle evidence is structural, with the fixed AST rule identity, not certified
proof. No new role, law, separation-of-duties or dimensional semantics are
admitted.

## Addendum: reference executor (2026-11-12, Norn-cloud/norn#45)

`norn-datalog` gains `evaluate`: semi-naive bottom-up evaluation over finite
input facts, stratum by stratum, behind `validate`, with its own metered
`ExecBudget` that refuses rather than runs away. Output order is canonical and
independent of fact and rule order. It adds no dependency (proptest is
test-only), no IO, no witnesses and no aggregates; the refusal of every
aggregate is unchanged. One code is added, `datalog.derived_in_input`, for
facts supplied to a rule-defined relation. It is the first executor in this
crate, used for rule packs over code and metadata facts that arrive as data.
The checker keeps its generated Ascent step; the executor is a dev-only second
engine in the checker's inheritance differential test. A maintained crate was
considered first: Ascent compiles rules to Rust at build time and cannot run a
pack supplied at run time; crepe is the same kind of macro; datafrog is a join
library without a rule language, stratification or budgets, so a rule AST
interpreter would still be needed on top of it.
