# Finite controls verdict contract

Controls describe ordered sensitive action sequences on the enclosing object
 type. They confer no runtime authority. Safety quantifies over singleton roles,
 after an unrestricted reachable prefix; availability is a separate judgment.

The checker supplies a complete overapproximation of actions and role grants.
It does not interpret kernel admission, effects, command bounds or policy scope.
Missing references, unsupported policies or exhausted budgets produce unknown;
no edge or grant may be silently dropped.

| Finite search evidence | Model safety | Checker witness status |
| --- | --- | --- |
| separated | proven | unknown |
| unreachable in complete overapproximation | proven | unknown |
| nonempty violating model trace | refuted | unknown |
| missing trace, invalid graph, exhausted search | unknown | unknown |

Named invariants:

- **Candidate only:** checker availability is always unknown, including unreachable
  models. Its trace is at most a candidate, never evidence of runtime execution.
- **Model refutation:** refuted means a violating finite-model trace, not a claim
  that kernel admission or execution accepts that trace.
- **No promotion:** invalid and exhausted checks remain unknown. The checker
  contains no mirrored kernel-admission or authorization rules.
- **Nonvacuity:** proven separation never implies witnessed availability.
- **Refusal (adapter target):** a refuted model verdict produces an error and refuses installation.
- **Required witness (adapter target):** requireWitness refuses checker admission because a candidate
  alone cannot satisfy a runtime availability requirement.
- **Projection (adapter target):** CheckedArtifactSet.controls carries each declaration's verdict in
  deterministic object type / declaration order; registry consumers preserve it verbatim.
- **Closure (adapter target):** inherited controls cannot disappear through a subtype or an overlay.
- **Kernel promotion (replay target):** only a kernel-side replay using its own admission and commit
  can promote a candidate to witnessed. Every step must commit successfully with
  the candidate's action and role, in order, against the supplied definition and state.
  Promotion is evidence for that replay context, never a reusable authority grant.
- **Refused candidate (replay target):** any admission or commit refusal leaves availability unknown;
  partial replay never establishes a witness.

Conformance scenarios: effect-free actions, over-budget action IDs and restrictive
policy scopes all remain candidate-only in the checker. Kernel replay must refuse
invalid commands and unauthorized scopes without promoting availability.
The evidence truth table is exhaustively exercised by controls module tests.
The adapter slice must cover candidate-only status, refusal, unknown, closure and
required witnesses through artifact-set admission. The kernel replay slice must
cover promotion and refusal. These are target contracts, deferred in the wire and
evidence-table slice; they do not claim those implementations are present here.
Evidence is finite model evidence, never a temporal proof or a live-runtime claim.

Kernel conformance (`candidate_promotion_requires_kernel_admission_and_commit`):
normal upserts promote only after the whole trace commits; missing effects,
create-only collisions, false guards, authorization denial and overlong command
identities stay unknown. A mismatched actor is rejected before any commit.
Replay records must address one object of the declared type in one scope.
