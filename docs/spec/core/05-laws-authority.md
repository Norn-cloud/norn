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

B0 #316 admits a closed pointwise safety/commit fragment. #322 renders EN/AR without changing semantic IDs. Event occurrence, arbitrary temporal logic and progress are not implied by these PRs.

## Semantics and obligations

Admission validates structured slots and grounded normative precedence, then resolves law subjects through `EffectiveCatalog::target_types` and explicit retained pack/app membership. Add a pack root while retaining `LawRoot::App`; unknown roots never mean global. Check interfaces even with zero implementors. Deduplicate a diamond's identical binding by law identity + concrete subject + substitutions, preserving all paths. Different substitutions remain distinct instances.

Specialisation/containment adds conjunctions. Refuse inherited-law replacement, precondition strengthening, input-domain narrowing or removal through changed containment. A solver result cannot authorize weakening. Permissions do not require an event, defeat physical laws or grant runtime access. Default deny and mandatory scope are enforced by the existing server/kernel authority; no UI-specific permission engine.

Generate initialization, non-vacuity, each-writer preservation, mapping totality/identity/unit/transition simulation/law preservation and open-boundary obligations from retained contracts. An empty implementor/domain is not non-vacuity evidence. Every writer introduced by rule, migration, connector or mapping changes the challenge. Missing/unsupported semantics remains unknown; runtime admission refuses safety predicates it cannot actually enforce.

At governed commit evaluate supported safety requirements on the whole proposed state/effect closure. Deontic permission is checked separately from action authorization. Recheck revocation and delegated limits at commit and dispatch; distinct-agent sessions of one accountable person do not satisfy independent-person approval. Timed liveness needs an activation model and scheduler/environment assumptions; no single commit proves eventuality.

[Owner] For imported/observed data, a law violation is a finding with quarantine and evidence because the source fact already happened. Its capture is not refused as a governed transaction. Promotion into authoritative business facts is a separate governed action that can refuse. The same law ID/predicate explains both outcomes; authority mode selects enforcement, never changes truth.

| Invariant / layer | Stable target diagnostic |
|---|---|
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
