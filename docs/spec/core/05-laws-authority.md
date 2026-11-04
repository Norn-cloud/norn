# Core 05: Laws, authority and evidence

Purpose [Owner]: structured laws state theorems about declared definitions, with construction/admission/commit enforcement and generated evidence. Permissions, access and proof authority remain distinct. [Common conventions](README.md) apply.

## IR contract

Reuse `LawIR`, `LawScope`, `LawRoot`, `LawModality` (temporal), `LawTiming`, `LawResponse`, `DeonticRule`, `StructuredRequirement`, `AccessIr`, existing capability grants, `CheckReceipt` and obligation challenge/evidence types.

| Target record/extension | Required fields |
|---|---|
| Structured law | stable law ID/version, `scope`, `condition?: Expr<bool>` (absent means unconditional), `subject`, `grade: obligation/permission/prohibition`, temporal requirement, typed response, normative source/precedence, severity; existing structured safety fragment is authoritative. |
| Effective instance | original law artifact/digest/ID, concrete subject member identity, attachment origin, all inheritance/containment/mapping paths, explicit typed bindings; original LawIR unchanged. |
| Cross-module binding | `organisation`, `book`, `currency`, `valuation_basis`, `account_roles`, transaction revision, opening balance scope; required references resolve unambiguously. |
| Activation (timed extension) | `id`, triggering event ID, law instance, start time, deadline/calendar pin, cancellation/retrigger policy, accountable subject; unsupported timing refuses admission. |
| Evidence | generated goal/statement digest, full writer/mapping closure, semantics/compiler/tool pins, parameter assumptions, bounds/fairness, result and proof digests, assurance level. |
| Delegation | authenticated principal, acting agent/session, accountable delegator, parent grant, scope/resources/actions/expiry/budget, revocation generation; every child is an attenuation. |

Display wording is an ordered BCP 47 locale map, separate from executable semantics. Each locale declares direction, templates and the full renderer vocabulary; all declared locales must be complete. See [the wording contract](../../platform/structured-requirements.md#locale-wording-law-wording-locale-1).

B0 #316 admits a closed pointwise safety/commit fragment. #322 renders EN/AR without changing semantic IDs. Event occurrence, arbitrary temporal logic and progress are not implied by these PRs.

## Semantics and obligations

Admission validates structured slots and grounded normative precedence, then resolves law subjects through `EffectiveCatalog::target_types` and explicit retained pack/app membership. Add a pack root while retaining `LawRoot::App`; unknown roots never mean global. Check interfaces even with zero implementors. Deduplicate a diamond's identical binding by law identity + concrete subject + substitutions, preserving all paths. Different substitutions remain distinct instances.

Specialisation/containment adds conjunctions. Refuse inherited-law replacement, precondition strengthening, input-domain narrowing or removal through changed containment. A solver result cannot authorize weakening. Permissions do not require an event, defeat physical laws or grant runtime access. Default deny and mandatory scope are enforced by the existing server/kernel authority; no UI-specific permission engine.

**AUTH-SYNC-WRITER-1:** the Cedar schema for a synchronous transition action
includes its primary target and the declared writer types, including effective
subtypes. Catalog invocation entry points remain primary targets. Schema
projection grants no permission: the actual policy, scope and capability grant
still authorize every current/proposed resource, and the kernel still checks
the closed writer footprint. Unbound or non-transition actions retain their
existing target projection. The finite four-type writer domain is checked over
all subsets with and without a synchronous binding; real PDP cases retain
wrong-scope, wrong-role, absent-policy and absent-grant refusals.

Generate initialization, non-vacuity, each-writer preservation, mapping totality/identity/unit/transition simulation/law preservation and open-boundary obligations from retained contracts. An empty implementor/domain is not non-vacuity evidence. Every writer introduced by rule, migration, connector or mapping changes the challenge. Missing/unsupported semantics remains unknown; runtime admission refuses safety predicates it cannot actually enforce.

At governed commit evaluate supported safety requirements on the whole proposed state/effect closure. Deontic permission is checked separately from action authorization. Recheck revocation and delegated limits at commit and dispatch; distinct-agent sessions of one accountable person do not satisfy independent-person approval. Timed liveness needs an activation model and scheduler/environment assumptions; no single commit proves eventuality.

**LAW-INTERFACE-1:** a retained law whose subject, entity root and sole scoped object type name one object type or interface T governs T and every type that extends or implements T, directly or transitively, as one instance per governed type; a diamond adds no second instance. An `Invariant` response binds only when T's effective contract and the governed type's effective contract each hold exactly one invariant with that id and both name the same owner. Findings keep the law's own pin and the invariant's original owner. Every other inherited, mixed or effective-instance scope still refuses with `core.laws.unsupported_semantics`.

**LAW-COMMIT-1 / LAW-SUPPORTED-1:** an unconditional always `Invariant` response resolves exactly one named invariant in its retained subject's effective contract and uses the shared invariant evaluator and budget; a violation retains the governing law pin and existing refusal-journal/delivery-consumption behavior, missing or misbound references refuse, and other unsupported responses remain unsupported.

[Owner] For imported/observed data, a law violation is a finding with quarantine and evidence because the source fact already happened. Its capture is not refused as a governed transaction. Promotion into authoritative business facts is a separate governed action that can refuse. The same law ID/predicate explains both outcomes; authority mode selects enforcement, never changes truth.

| Invariant / layer | Stable target diagnostic |
|---|---|
| LAW-WORDING-LOCALE-1: complete, unique well-formed locale entries; admission/render/inspection | `law.wording.invalid_locale` / `law.wording.duplicate_locale` / `law.wording.incomplete_locale` / `law.wording.unknown_locale` |
| LAW-RETAIN-1: inherited law identity/domain unchanged; admission | `core.laws.weakening` |
| LAW-COMPLETE-1: complete effective subject/writer/mapping closure; admission | `core.laws.unresolved_scope` |
| LAW-SUPPORTED-1: exact predicate/timing supported, never omitted; admission | `core.laws.unsupported_semantics` |
| LAW-COMMIT-1: whole proposed state satisfies immediate safety; commit | `core.laws.violated` |
| AUTH-ATTENUATE-1: current scoped authority, no escalation; admission/commit/dispatch | `core.authority.denied` / `core.authority.revoked` |
| PROOF-BIND-1: exact generated statement and checked result; evidence admission | `core.laws.stale_evidence` |

Evidence levels remain separate. Lean proof bodies must check the generated theorem with declared axioms and no admitted holes; bounded model results carry bounds. Admission receipts, source reviews and echoed challenges are structural evidence only. Report eligible auto-proof goals solved/total separately from all goals and unknown/refuted counts.

### Exact result identity (PROOF-BIND-1)

The checker-owned `Challenge` supplies `evidence_pins`, `record_evidence` and
`evidence_identity`; there is no parallel goal type. Pins select an existing
generated obligation by index and digest and retain the exact challenge digest.
The record retains the complete challenge: generated statements, full definition
and writer/mapping closure, unit interpretation, semantics/compiler profile,
assumptions and assurance requirements. Any source change conservatively
invalidates identity, including edits outside a narrower semantic dependency set.

The controller supplies tool ID, version and lowercase implementation SHA-256,
an exact nonempty bounds/fairness JSON object (at most 64 KiB), and a `u64` seed.
The result is opaque UTF-8 JSON (at most 1 MiB) with its raw SHA-256. Whitespace,
Unicode spelling and numeric spelling remain significant. The identity check
also requires the independently retained result digest from the trusted checked
driver; taking that expected digest from the record under inspection is invalid.

`Current` establishes identity only. It authenticates no tool execution and
confers no bounded-pass, counterexample, non-vacuity or proof status. The real
checked driver supplies those results and actual bounds; unsupported generated
proof assessments remain unknown. A mismatched current pin, malformed record or
changed payload yields `Unknown` / `core.laws.stale_evidence` (PROOF-STALE), never
a pass. Replacing both a stored result and its co-located hash still fails the
independent result pin.

Named identity scenarios: matching pass-shaped and counterexample-shaped payloads
stay identity-only; writer, unit and mapping edits stale either result kind;
tool/version/implementation, bounds, seed and goal edits stale evidence; joint
payload/hash substitution fails; malformed or oversized profiles/results refuse.
These scenarios do not simulate a solver or claim a completed constitution drop.

## Named acceptance scenarios

- **LAW-LOCALE-DATA/INCOMPLETE/UNKNOWN:** A complete French entry admits and renders through data only; missing text, labels or terms in any declared locale refuse. An unavailable requested locale refuses without fallback, including during direction inspection. EN/AR retain their existing display bytes and semantic identities.
- **LAW-INTERFACE/TRANSITIVE/DIAMOND:** Given direct and transitive implementors, when B2 resolves laws, then retain each origin and one instance per identical substitution; missing subject refuses.
- **LAW-CONTAINED/EMPTY:** Given nested pack/app laws and zero implementors, when admitted, then validate bindings but leave non-vacuity unknown; an unknown app refuses.
- **LAW-TIGHTEN/WEAKEN:** Given inherited conservation, when adding an additional loss bound, then conjunction is allowed; replacing conservation or excluding failing inputs refuses.
- **LAW-COUNTEREXAMPLE:** Given 100 fine g in and 99.2 out with no loss, when checking generated semantics, then produce a failing scenario; the live write refuses atomically and imported evidence is quarantined.
- **AUTH-REVOKE:** Given delegated approval at generation 1, when revoked before commit/dispatch, then no new governed action/external attempt; already dispatched results are recorded and reconciled.
- **PROOF-STALE:** Given a passing old statement, when a writer/unit/mapping changes, then old evidence is unknown for the new goal, never proved.

## Main and gaps

Present: `crates/norn-ir/src/{verification,deontic,access}.rs`, `crates/norn-checker/src/{deontic,verification,law_enrollment}.rs`, `obligations/`, `crates/norn-authz/src/grants.rs`, `crates/norn-kernel/src/decision_commit.rs`. #316 merged; #322 open. Narrow induction generation exists; general aggregate laws, retained interface/containment closure, generated solver dispatch and full delegation chains are not established. Core physics declarations in `registry/core/src/physics.ts` are not complete runtime law enforcement.

## Work breakdown (each PR ≤400 lines)

1. Complete B0b #322 deterministic EN/AR slot rendering; retain #316 safety refusals.
2. B2 effective law instance contract, then interface resolution, then containment/diamond/weakening tests as separate cuts; coordinate 06 B1.
3. B4 typed aggregate/pre-post safety admission, then whole-commit evaluator using 03/04, then LAW-COUNTEREXAMPLE tests.
4. B5 generate initialization/writer/non-vacuity statements; B6 generated Quint jobs; B7 exact result binding; B8 scenario/explanation projection, each separately gated.
5. Attenuation/revocation chain contract and commit/dispatch enforcement in separate cuts; AUTH-REVOKE and independent-person approvals.
6. Bind law evaluation to observed finding/quarantine mode under 07, then analytical publication guards under 08; preserve governing law identity in both modes.

## Shared refusal diagnostics (#517)

**DIAG-REASON-1:** commit and action invariant refusals retain the owning
object/interface, invariant identity and canonical owner-definition digest in
`RefusalReason.pin`. Lifecycle reasons pin their built-in semantics version.
Typed operands share the existing `RuntimeValue` model. Evaluator failure text
and non-boolean values are represented by outcome categories, not raw values.

**DIAG-REDACT-1:** API `norn/refusal@2` projections require proven disclosure
authority to expose a reason. Without it, the entire pin, subject and operands
are absent, and the explanation is fixed. Visibility/internal details remain
hidden even with a disclosure grant. Retryability defaults to false; only
engine-defined stale preparation and concurrency paths set it true.

**DIAG-HISTORY-1:** stored receipt formats, diagnostics and content hashes are
unchanged by transport projection versions. Historical receipts still verify.

Named scenarios: invariant refusal retains a declaration pin through the API;
unauthorized projections are independent of arbitrary secret operands;
a pre-change checker receipt verifies against its original hash.

### Conditional linear certificate kernel (#711)

**CONS-LINEAR-1:** the mathematical coefficient checker verifies every affine
writer column against the declared rational functional, independently per exact
scope and unit. Altered weights, malformed matrices and duplicate coordinates or
writers refuse. Nonzero residuals identify writer, partition and symbolic column;
unsupported writers and arithmetic/resource limits remain unknown. A residual is
an algebraic witness, not a reachable runtime counterexample.

This kernel alone establishes no application evidence. Declaration lowering must
bind the complete writer closure and functional to a generated Challenge before
its result can discharge a preservation goal. Initialization, non-vacuity and
open boundaries remain separate obligations. Named kernel scenarios: valid affine
balance, omitted loss, unit mismatch, cross-book cancellation, substituted/zero
weights, unsupported writer, malformed shape and exact fractional balance.

**CONS-LINEAR-DISCOVERY-1:** candidate weight discovery solves exact rational
equalities per scope/unit partition, plus explicit coordinate anchors (the
equality-only LP feasibility fragment). Require a nonzero anchor, choose zero
for unanchored free variables, and return unknown on infeasibility or limits.
Discovery is untrusted and never refutes a law. Every candidate must separately
pass CONS-LINEAR-1 against the declared functional. Named scenarios: fractional
exchange, conflicting/zero anchors, free coordinates, unit/book separation,
opaque writer, duplicate identity and generated scaled exchanges.

### Constraint diagnosis (DIAG-CORE-1 / DIAG-REPAIR-1 / DIAG-UNKNOWN-1)

The chase-backed diagnosis profile enumerates irreducible declaration conflicts
and correction proposals relative to fixed open-world facts. Every core is
unsatisfiable and each one-member deletion is satisfiable. Every correction
relaxes only explicitly editable declarations and is independently checked on
the complete remaining input; restoring any member makes it unsatisfiable.
These proposals grant no authority: concrete edits require ordinary admission.
Protected laws are never repair candidates. Whole-search and per-chase ceilings
are disclosed; an exhausted or refused subset query makes the diagnosis unknown.

The `norn.finite-conjunction/1` diagnostic fragment uses the shared closed finite
schema enumerator and expression evaluator. The universe and roster snapshot
are explicit closed-world assumptions, never inferred from the open-world chase.
Ceilings are 256 contexts, 65536 encoded domain bytes, 8192 predicate evaluations
and 1000 expression fuel. A diagnosis discloses these plus its query ceiling.
Unsupported/unbounded domains or exhausted enumeration return unknown, not a
conflict. This fragment grants no admission or proof authority.

DIAG-PIN-1: refused candidates receive diagnostics only: exact attempted-source/base digests,
member identities/pointers, checker/expression/fragment profiles, enumeration and
query budgets, explicit closed-world assumptions and retained editability
policy are pinned together. The report type has no Challenge/EvidencePins or
PROOF-BIND-1 conversion. Ordinary admission is unchanged; corrected candidates
must be resubmitted. Editable/protected overlap refuses diagnosis. Duplicate or
incomplete source identities are unknown rather than an unbound explanation.
Generated profiles use sorted-key exact-text canonical serialization. If encoding
fails, diagnosis is unknown with a reason and its profile digest is absent (the
last native digest slot is empty); exact source/base digests remain available.
Stable codes: `consistency.diagnosis.conflict`, `.no_editable_repair`, `.unknown`.

### Path equations (PATH-EQ-1, #712)

Authors declare `pathEquations: [{ id: "provider", left: ["fulfils", "provider"],
right: ["provider"] }]` on `Norn.type`. EntityIR retains these declarations in
`extensions["norn.path-equations/1.0.0"]`. Each path begins at the declaring
concept, traverses functional forward links with concrete targets, and ends at the same concrete type;
empty paths denote identity. Missing links, interface targets (including those with one implementor),
non-functional steps, duplicate IDs and different endpoint types refuse.

PATH-EQ-1 lowers each equation to an existing chase EGD, with one shared root
variable and equality between the two endpoints. Optional links give conditional
equality when both paths exist; equations do not assert existence. Inherited
link contracts resolve through the effective catalog. Admission limits are 128
equations per concept and 64 links per path: at most 128 path-link atoms plus
one shared-root concept atom (129 body atoms) per generated EGD. This declaration/lowering
unit establishes structural evidence only: mapping preservation requires the
exchange admission bridge and its separately qualified source/mapping lowering.
No successful declaration is a preservation proof. The existing exchange search
is bounded; absence of a witness is bounded-no-counterexample, never exhaustive.

Named scenarios: PATH-EQ-VALID/IDENTITY; PATH-EQ-UNKNOWN-LINK/MANY/ENDPOINT/ID.

PATH-MAP-1: a `PathMapping` `F: target -> source` names a source concept per
target concept and a functional source path per mapped target link; data moves
by pullback. `admit_path_mapping` lowers both catalogs' PATH-EQ-1 equations,
functional EGDs for every reachable to-one link, source totality TGDs for
required links, and one TGD per object and link image into an `Exchange`, then
runs the existing schema-compatibility search. Ill-typed images and overlapping
catalogs refuse (`consistency.path_mapping.invalid`); every verified witness
refuses as `consistency.schema.unpreserved_constraint`. `compose_path_mappings`
substitutes images and refuses components with two images of one link.
Admission is bounded-no-counterexample, never exhaustive. A witness is
attributed to the first EGD its mapped image breaks, so equations sharing a
witness are named once; the mapping still refuses. Target totality and
source endpoint typing are not lowered: the first leaves existence unchecked,
the second can only add spurious witnesses. Named scenarios reproduce #574:
swapped roles, dropped join, non-functional image and bad composite refuse;
identity, equivalent path and identity composites admit.

### Declared-only normative resolution (D1, #767)

LAW-RETAIN-1 / OVERLAY-MONOTONE-1: `check_norm_precedence` requires exact source,
layer, priority-edge and protection bindings over trusted exhaustive masks for
one action/subject. Protection and cross-layer checks precede precedence. Any
conflict core refuses the entire resolution with its exact rule indices; no
partial survivors are returned. Same-layer opposing rules require explicit
comparability, including the transitive closure of declared edges. Cycles refuse.
Cross-layer clashes refuse even with an explicit higher-layer win. Lower-to-higher
or protected-law defeat declarations refuse even when inactive or excepted.

D1 applies everywhere: authority level, enactment and applicability specificity
never break an undeclared tie. No within-layer opt-in exists. Missing optional
legacy ranks do not prevent explicitly declared resolution. The old implicit
comparison is private and used only for Info `consistency.normative.legacy_resolution`
notes; it cannot select effective survivors or suppress conflict cores.

Select effective versions before grounding; latest recorded is not lex posterior.
Exceptions belong to their own rule and cannot narrow a higher law. A future
specialis policy would need proven strict containment; equal specificity has no
winner. CD diagnosis supplies exact protected cores and source locations;
malformed bindings and exhausted budgets remain unknown. This is conditional
finite-model consistency, not fulfilment, CTD/Datalog lowering or proof authority.

Named scenarios: undeclared superior/posterior/specialis ties refuse; explicit
transitive edges resolve; removing an edge refuses; cross-layer wins and protected
defeats refuse; numeric-rank removal leaves declared results unchanged; exhaustive
three-rule groundings and generated catalog permutations preserve exact survivors.
## Generated bounded creation profile (#565)

**PROOF-QUINT-1:** the checker reconstructs the admitted Challenge before lowering
a selected create-only writer, ordered derivations, preconditions, stored defaults
and a pointwise predicate law. One empty store, singleton object identity and
explicit finite decimal input intervals bound the claim. Unsupported constructs
leave each affected goal unknown with a reason. Supported numeric fields share one effective fixed scale/unit interpretation.
Currency sources, rates, rounding, mixed interpretations and inexact literals
remain unsupported.

Initialization is vacuous over the empty store. Writer preservation checks the
proposal before law enforcement; non-vacuity needs a reachable law-satisfying
witness. Neither a generated module nor structural correspondence discharges a
goal: only an independently pinned backend result supplies bounded evidence.
Named scenarios: original/mutant/restored lowering, unknown input bounds, exact
decimal correspondence and unsupported expression/inexact literal refusal.

**PROOF-TRANSITION-1:** declaration lowering first constructs the native
`norn-ir::verification::finite::CreateRelation`. This shared relation carries
source law/writer/object/property identities, effective scale/unit contracts,
finite input intervals, substituted native guards, simultaneous updates and
the guarded law predicate. Its empty-to-singleton semantics and stuttering frame
are independent of backend syntax. Expressions remain in the same closed Norn
Boolean/additive fragment; unsupported constructs keep their existing reasons.
Quint emission consumes this relation without reading declarations again.

Named scenario: `finite_relation_is_backend_neutral` compares the complete
neutral and Quint goal inventory, bounds and Challenge identities, then checks
the original/omitted-derivation update correspondence. Original/mutant model
bytes must also match the pre-refactor generator's check-time golden outputs.
Constructing or inspecting the relation supplies no solver verdict or authority.

Each generated obligation selects its governing
LawIR by the retained Challenge law identity; a subject may have multiple laws.
All generated goals remain in Challenge order. An opaque law remains Unknown
without suppressing independent supported laws. Native correspondence scenarios:
`multiple_governing_laws_keep_distinct_goals_and_mutant_semantics` and
`an_unsupported_governing_law_does_not_suppress_supported_laws`. This lowering
unit adds no solver discharge authority.

`norn_checker::obligations::transition` retains ordered derivations, assignment
origins and omitted caller fields, and projects exact integer affine creation
deltas/equality residuals. Unsupported arithmetic, overflow and non-equalities
remain Unknown; guards and law conditions are never erased. Input bounds,
absence, singleton identity and empty-state coverage remain explicit assumptions:
this view grants no unbounded writer-closure proof or evidence authority.
Named cases check native affine evaluation, ordered substitutions/input requirements,
mutant default origins, explicit refusals and unchanged Quint golden bytes.

**PROOF-QUINT-RUN-1:** the outer Python harness owns process execution. The native
producer/binder reconstructs every Challenge, checks controller-retained model
and result digests, and binds exact result bytes through EvidencePins. Current
identity supplies no verdict. The primary tla-ts 0.0.7 result must agree with the
pinned Apalache oracle. Every unsupported, failed or disagreeing result is unknown.

Qualification is restricted to integer/Boolean states and one worker. Raw
successful universal searches require nonempty initialization, a drained queue
and declared state cardinality. Counterexamples and non-vacuity witnesses stop
early and require agreeing backend verdicts and traces. A
conservative arithmetic bound must fit signed 32-bit integers; this is not a
claim about unbounded TLA+ arithmetic. Exact Apalache/Variants module sources
come from the pinned compiler JAR. The constitution profile excludes caller loss
and zero output. Named acceptance: original pass, omitted derivation refutation,
restored model/Challenge/pass, stale evidence Unknown; the seven other generated
obligations remain individually unknown. Source inspection and parity qualify
this bounded fragment only, not general translator or model-checker soundness.

**LAW-COMPLETE-1:** Unresolved declared interface ancestry refuses law resolution before filtering applicability; a missing ancestor can never silently remove a retained law.

**PROOF-GUARANTEE-1:** the native binder emits one typed document per law, with
all generated obligations, exact bound records and typed trace quantities. A
recording is written only after the complete original/mutant/restored acceptance
passes; failed runs retain Unknown in their output and produce no recording.
Runtime and parity links remain explicitly not-yet unless separately supplied.
Violation results require a non-empty reconstructed counterexample or witness;
missing/null/empty traces remain Unknown. Named refusal scenario:
`qualifying_violations_require_a_nonempty_reconstructed_scenario`.
Named scenarios: `counterexample_reconstruction_uses_declared_exact_units_and_empty_state`
and `recorded_guarantees_match_bound_results`. Captured original/mutant documents
are bless-gated fixtures from actual backend output, not fresh solver verdicts.
Tool assets use logical name/version identities and exact content digests, without
installation paths. Compiler stages retain exit codes; oracle records retain verdicts and exact scalar
trace values in state order. Run-specific diagnostics and ITF creation metadata
stay outside recorded evidence. Structured primary results remain bound.
The binder and replay use `norn_canonical::serialize_json_verbatim` for recording bytes.
Law-level bounds use sorted-key exact-text canonical JSON and retain the complete
shared profile, toolchain, expected state count, worker count and exclusions. Model digests, invariant selectors and step
selectors stay in each goal's unchanged evidence pins, never in law-level bounds.

**PROOF-GUARANTEE-1 contract:** the typed `norn/guarantees@1` per-law presentation retains
law artifact/version, bounded verdict and evidence level as closed enums,
Challenge identity, exact bounds JSON, actual tool/result pins and the complete
generated goal inventory. Unsupported goals keep explicit Unknown reasons.
An ineligible goal must have Unknown verdict and level; encoding, decoding and
the emitted schema reject every other combination. Named refusal scenario:
`ineligible_goals_require_unknown_on_both_wire_boundaries`.
Rust also enforces document count relations at both wire boundaries:
`total == goals.len()`, `eligible == count(goals where eligible)`, and
`discharged <= eligible`. JSON Schema cannot express these cross-field relations.
Rust also rejects duplicate goal indices at both boundaries. Indices refer to
the retained Challenge, so a per-law document may contain non-contiguous indices.
Named scenarios: `document_total_matches_complete_goal_inventory_at_both_wire_boundaries`,
`document_eligible_matches_goal_flags_at_both_wire_boundaries`, and
`document_discharged_cannot_exceed_eligible_at_both_wire_boundaries`.
Named index scenario: `document_goal_indices_are_distinct_challenge_indices_at_both_wire_boundaries`.
Exact trace quantities serialize as canonical integer text with declared scale
and units; counterexamples and non-vacuity witnesses have distinct kinds.
Runtime enforcement and application parity links are either independently
supplied references or explicit `not-yet` values. A typed document is a
presentation of the checked driver's result, never proof or activation authority.
Its OpenAPI projection adds new components without modifying existing schemas.
Named scenarios: exact coefficient roundtrip; unknown requires a reason;
unknown enum/field, fractional coefficient and invented `proved` level refuse;
`guarantee_projection_matches_typed_eligibility_and_exact_scalars` checks wire/schema
correspondence, including integer endpoints and trailing line terminators.

### HOST-AUTH-1: explicit server policy before host access

The host authorizer denies every operation unless a trusted server policy admits it. Supplying an actor or granting a definition read does not grant action mutation, object disclosure or collection disclosure; those kernel authorization callbacks deny independently by default. The server supplies scope and authenticated actor; the client never grants authority. Named scenarios: `absent_host_policy_denies_authenticated_and_anonymous_operations` and `host_read_grant_does_not_supply_kernel_action_or_disclosure_grants`. This is the shared authorization boundary; host admission and route callers follow in separate units.

### DIAG-DISCLOSE-1: explicit diagnostic permission

Complete refusal detail (law/invariant pin, reason, subject and operands) requires the server's independent scoped `norn.disclose_refusal_detail` decision through the existing AccessIR → Cedar PDP and capability conjunction. Its resource is `norn.RefusalDetail`, identified by the admitted action id; trusted context binds `governedAction`. A grant covers that action's complete diagnostic in that physical scope, not general business reads. Action permission, `norn.read`, and marking grants never imply it. Missing policy, principal, role, capability, exact scope or a changed mapper binding denies. Visibility and internal refusals remain opaque even with the grant. Fixture claims enter through the same trusted mapper that later resolves production principals; no client claims establish authority.

Named cases: `explicit_grant_preserves_the_complete_shared_reason`, `absent_capability_policy_role_principal_or_scope_stays_redacted`, and `trusted_mapper_cannot_change_the_bound_identity_action_resource_or_scope`. The fixture policy is typed TypeScript and its generated policy bytes are checked against recompilation; decisions use the real scoped PDP. Evidence is runtime correspondence and sampled scope mutation, not universal proof. Deeper engine proof reruns are deferred under the demo deadline; this adapter changes no evaluator or stored receipt semantics.


### Conditional lifecycle goals (PROOF-LIFECYCLE-1)

The challenge appends lifecycle goals after every existing law obligation. For
an effective chart it emits one reachability goal per terminal, one no-stuck
non-terminal goal, and one non-vacuity goal per transition. Each statement binds
the concrete object type, lifecycle property, complete effective chart digest,
one-object bound and chart-state-count depth bound. The existing constitution
has no lifecycle: its ten obligations and complete challenge use one sorted-key
exact-text canonical golden across package, insertion-order, arbitrary-precision
and workspace builds. PROOF-BIND-1 generated commitments sort keys recursively
without normalizing Unicode spelling or number tokens. Raw source and solver
result bytes remain exact; Norn1 artifact identities retain their own profile.

This is conditional transition-only reachability from an admitted initialized
object. Scalar context and external references stay frozen. All supported
applicable pointwise laws filter initialization and post-states. Unsupported
guards, laws, timing, cross-lifecycle reads and extra effects yield Unknown with
a reason. Inventory reasons name an applicable unsupported governing-law root
and its law identity (the native `UnsupportedUntilB2` boundary); otherwise they
state that qualification or checked results are missing. A generated goal alone
is Unknown; it never authorizes discharge.
The full action/writer closure is a separate qualification profile (#951).

Named scenarios: `generated_lifecycle_reachability` retains a disabled transition
in the inventory and binds exact guard changes; `constitution_ten_goal_inventory_golden`
pins the old inventory and challenge; `jewelry_app_law_keeps_all_lifecycle_goals_unknown`
uses the actual authored source golden without rewriting its App-scoped law;
`lifecycle_goal_cap_is_checked_before_append` refuses an overflowing denominator.

### Observed conservation suggestions (MINE-CONS-1, #736)

The runtime `norn_cell::mining::suggest_conservation` question discovers one
anchored exact rational functional from an explicit training prefix, then checks
all supplied observed deltas with CONS-LINEAR-1. Its source-window identity,
scope/unit coordinates, training count, supporting record/version identities and
exact residual counterexamples remain visible. The adapter owns authorization,
immutable window retention and faithful lowering; the function fetches no data.

Results are always `SuggestedNotAdmitted`, including an entirely balanced
window. Finite observed arithmetic supports validation but never certifies
business truth, future behavior or writer coverage. No admission, law edit or
commit follows from a result. Saving or accepting a proposal requires ordinary
data actions and definition admission, outside this first cut. Empty/all-zero
training, infeasible anchors, malformed input and exhausted exact/resource bounds
yield no proposal. Scope/unit partitions cannot cancel each other. Limits: 256
records, 64 coordinates, 64 scope bindings per coordinate, 4096 identity bytes,
and the existing discovery coefficient/work ceilings. Named scenarios: true
fractional candidate with full evidence, holdout near-miss with exact residual,
no-signal control, duplicate record, unit separation, oversize window and measured
256-record fixture. Law drift, misfit rules and statistical guards remain open.
