# Core 04: Checked transactions and effects

Purpose [Astra]: every governed writer lowers into one closed checked calculus with complete read dependencies and one atomic commit. [Common conventions](README.md) apply.

## IR contract

Reuse `DecisionOutput`, `FactChange`, kernel `CellCommit`, `FactExpectation`, `FactWrite`, `Command`, `Timer`, `IdempotencyKey`, `Generation`, `LogicalTime` and `World`. Extend these families rather than inventing application posting opcodes.

**ACTION-ACTOR-1:** Action derivations read only the host-authenticated call principal as typed `actor.subjectId`, preparations and receipts bind that same principal, and caller-supplied `actor`/`approved` fields at any depth or a principal outside the NFC expression profile refuse with `action.invalid_input` before writes, never rewriting an identity; admission rejects those reserved input-schema property names, including nested objects and array items, with `action.validation.invalid_schema`. A field an action derives from the actor without the caller (a derivation with `inputRequired: false` that reads `actor`) is never caller input: a supplied value refuses with `action.invalid_input` rather than being overwritten, and a published caller input schema (`GET /v1/actions`) omits it.

| Target semantic envelope | Required fields |
|---|---|
| Intent | `operation_id`, exact canonical `input_digest`, `actor`, `delegation_chain`, `scope`, `world`, `action: Pin`, `definition_generation`, `effective_at`, `user_preconditions`, `retry_ceiling`. |
| Read set | point versions including absence, source/link partition membership fences including empty sets, definition/settings/authorization versions, order dependencies; use #324's `PartitionSlice` end-to-end. |
| Checked proposal | intent, complete read set, `direct_writers`, `synchronous_closure: [Pin]`, ordered typed fact/link/state/quantity effects, outbox/timer changes, law evaluation results, setting-version bindings, input observation receipts. |
| Commit result | `operation_id`, `input_digest`, committed revision, effect/receipt IDs, exact result digest; duplicate lookup remains authorized. |

Closed effect operations are the existing canonical fact mutations, link mutations, declared lifecycle transitions, generic quantity-template instances, command/timer enqueue/cancel and delivery consumption. Application-specific operations are forbidden. Derived-field writes, undeclared effects and arbitrary callbacks refuse.

## Semantics

1. Resolve current authorization and exact action/definition pins. Reusing an operation ID with a different payload/scope/action/precondition refuses. An identical authorized retry returns the prior receipt without executing again.
2. Read a bounded complete snapshot. Evaluate pure rules and synchronous triggers to a bounded acyclic closure, preserving declared order and recording every writer. Recursive/effect cycles outside the admitted profile refuse; no post-commit trigger repairs immediate safety.
3. Apply effects to a working post-state only. Resolve guards against the specified pre-step state; evaluate cross-object laws over the entire proposed final state. Record intermediate order for replay. Any failed member/guard/law discards all effects.
4. Atomically validate point/absence/membership versions, definitions, settings and authority, then persist facts, links, quantities, ledger, lifecycle, outbox, timers and consumed delivery together. Crash before commit exposes none; crash after commit exposes all via receipt/replay.
5. Contention may trigger a bounded pure re-evaluation with the same operation identity and unchanged user preconditions. Changed approved batch, quote or plan is stale intent, never silently refreshed. Cross-cell atomic requirements refuse; explicit commitment protocols model partial progress.
6. Cancellation before dispatch records cancellation; after external success it creates a compensating action under 07. Posted economic history is append-only even where ordinary draft objects support delete. Imported observations enter quarantine/finding processing before any optional governed promotion.

| Invariant / layer | Stable target diagnostic |
|---|---|
| TXN-ATOMIC-1: all synchronous effects and outbox/timers commit together; commit | `core.txn.atomic_boundary` |
| TXN-CLOSED-1: every writer admitted in full closure; admission/preparation | `core.txn.unchecked_writer` |
| TXN-READSET-1: absence/membership/definition/settings/auth dependencies fenced; commit | `core.txn.contention` (reserved, unimplemented) |
| TXN-INTENT-1: user preconditions survive retries; preparation/commit | `core.txn.stale_intent` (reserved, unimplemented) |
| TXN-IDEMPOTENT-1: one accepted consequence per exact intent; commit | `core.txn.identity_mismatch` (reserved, unimplemented) |
| TXN-HISTORY-1: posted facts never overwritten/deleted; admission/commit | `core.txn.immutable_history` (reserved, unimplemented) |

Expected evidence: runtime atomicity/CAS enforcement, crash/reopen/race correspondence, generated per-writer preservation and explicit external-boundary goals. A safe constructor only certifies its own outputs; arbitrary account selection or imported data does not inherit that proof.

RECEIPT-AUTH-BIND-1: `acceptReceipt` preparation accepts a content-digest reference only. The tenant/world/authority-scoped host receipt store resolves authenticated `Acceptance` plus `FunctionReceipt` and consumption state. A bound result retains the exact receipt/result/command/generation pins and reconciliation reads. Unknown, replayed, mismatched, failed or wrongly typed results refuse; an action-supplied receipt body always refuses. Receipt consumption and destination writes remain one checked commit; binding alone grants no publication authority.

GUARD-DIAGNOSTIC-1: a transition may declare `guardRefusalCode`, naming an error diagnostic registered at runtime tier T6. Admission refuses unknown/non-runtime codes. A redeclaration retains an inherited diagnostic; omission inherits it. A refused guard exposes the selected code while retaining the original guard expression, failure reason and underlying code. Diagnostic selection changes no admission, authorization or transaction behavior. Core 08 publication uses `core.projection.publication_guard`.

## Named acceptance scenarios

- **TXN-ALL:** Given receipt quantity/value and matching GL effects, when a final guard fails, then no voucher, stock, GL, command or timer changes; successful replay returns the whole same commit.
- **TXN-PHANTOM:** Given a fold over an empty or populated partition, when another commit inserts/removes a member before finalization, then the stale proposal commits nothing.
- **TXN-TWO:** Given balance 5, when two consumers each request 4, then at most one succeeds and the other recomputes/refuses without negative balance.
- **TXN-RETRY:** Given committed operation O, when O is retried after lost acknowledgement, then return its authorized receipt; changed amount under O refuses.
- **TXN-APPROVAL:** Given approved membership A+B, when membership becomes A+C, then refuse stale intent even if quantity totals match.
- **TXN-CROSS:** Given one immediate law spanning two cells, when admitted as atomic, then refuse unless co-located; a declared pending commitment process remains distinct.

## Main and gaps

Present: `crates/norn-ir/src/{decision_output,fact_change,action}.rs`, `crates/norn-kernel/src/{decision_commit,cell_host,effect}.rs`, `crates/norn-cell/src/{host,store,write,quantity}.rs`. Main has atomic decision/CAS/outbox paths and generic quantity primitives, not demonstrated universal writer closure. Lane A #323/#324 and effect composition must converge here. Host #270/#308–#312 and sampled surface suite #317 are pending, not transaction proofs. Lane C #227 uses declared rules/templates; no production stock helper qualifies.

## Work breakdown (one coherent unit per PR)

1. Thread #324 complete reads through existing preparation and finalization; TXN-PHANTOM/TWO race tests (lane A).
2. Bind admitted rule effects/quantity-template instances to one authority envelope; reject uncovered writers (A4).
3. Integrate atomic finalization and crash/reopen/idempotent receipts; TXN-ALL/RETRY (A5).
4. Add definition/settings/authorization conflict pins and stale-intent diagnostics with 05/06; TXN-APPROVAL/CROSS.
5. Qualify host/surfaces #270, #308–#312, #317 against exact same generic actions/refusals; no transport-local permissions.
6. Lane C receipt/delivery, then transfers/returns/corrections as declared rules after 03 folds and 05 safety; retain ERPNext oracle vectors.

### RECEIPT-EFFECT-1 — reference-only receipt effects

A declared transition may carry `acceptReceipt`: a required string reference input,
an immutable Function manifest digest, a writable digest property and typed result bindings. Action JSON never supplies a receipt body or result.
Admission rejects unknown, computed, reserved and lifecycle binding targets.
Both destinations exclude objectIdField and every effective identity-key component; a result destination cannot equal digestProperty. The exact writer closure remains required; catalogs preserve the declaration.
Only the cell host resolves authenticated, scoped evidence; unsupported hosts refuse.
Execution is open only on the single-commit path of Core 07 RECEIPT-CONSUME-1:
consumption and effects commit atomically, with no partial persistence on refusal.

The cell is the trusted committer: receipt rewriting by a party with cell-store write access is out of scope until external receipt anchoring.

### TXN-CELL-1 — single-cell execution profile

Every supported proposal key and actual admission read belongs to the bound
host's cell, including link endpoints, membership/absence partitions, and keys
discovered during graph traversal. Ownership is host evidence under the commit
snapshot, never a law applicability label or request hint. Foreign or unknown
ownership refuses `core.txn.atomic_boundary` before the host applies any effect.
Outbound commands are local outbox enqueues, not remote atomic writes.
Consumed timer incarnations, command generations and observations also require
local ownership before any pending-delivery read or effect.

The current supported immediate laws are pointwise; inherited/effective or
otherwise unresolved dependencies retain their existing unsupported refusal.
This profile produces no splittability label or distributed guarantee. A host
supporting external dependencies must resolve ownership for both facts and
complete membership partitions; unknown is not local.

Named scenarios: **TXN-CELL-CROSS** refuses a foreign immediate-law read with
unchanged facts, ledger, outbox and timers; **TXN-CELL-UNKNOWN** refuses unknown
inherited ownership; **TXN-CELL-LOCAL** retains ordinary local admission.
**TXN-CELL-DELIVERY** refuses foreign/unknown timer, result and observation
consumption before pending lookup, leaving facts, ledger, outbox and timers unchanged.

### RUN-LEDGER-1 — the cell's run ledger

A run of an admitted flow is the object of the flow's generated Run type; its
lifecycle property is the flow's only state machine. A commit starts a run by
carrying one `norn.run.started` ledger event (run id, flow artifact id and
version, Run type, lifecycle property) together with the write that creates the
run object. The store records the run (generation 1) in `_norn_flow_run` in that
commit's transaction, and refreshes its state, version and update time in the
transaction of every later commit that writes the run object. A start whose run object
already exists before the commit (a creating write is required), whose Run type
is undeclared, or whose run id was already started in the world refuses
(`run.start_invalid`, `run.exists`) and nothing commits. The run-start event stays
in the hash-chained ledger; the typed commit reader skips it as bookkeeping. The ledger derives only
from committed batches, so every engine that runs the store's SQL maintains it and
export replay rebuilds it. It is a read index, not authority: reads go through the
caller's read gates on the Run type. `live_runs(flow)` counts the runs whose object
exists in a non-terminal state of its Run type's lifecycle (RUN-LIVE-1), for a retire
guard; an undeclared Run type counts as live. Named scenarios (`crates/norn-cell/tests/flow_run.rs`):
**RUN-LEDGER-START**, **RUN-LEDGER-REFUSE**, **RUN-LEDGER-PAGE**, **RUN-LEDGER-REOPEN**, **RUN-LIVE**.

### RUN-ROUTES-1 — starting and reading runs

`POST /v1/runs` with `{flow, runId}` starts a run of an installed, active flow,
named by its definition id; the flow's rule is `<flow>.flow.rule`. Scope and
authority are decided together and refuse with one opaque 403: the host
authorizer decides `StartRun(flow)` (in the DO cell, the declared starters of
`norn-cell-do-run-grants/1` that also hold the Values grant). A run holds no grants
of its own. `runId` is 1-128 bytes of `[A-Za-z0-9._:-]`. The flow's `decide` rule
evaluates its start turn (`{kind: "start", runId}` over an absent run,
FLOW-TURN-1), and the cell commits the run object, its first commands and its
ledger row in one commit (RUN-LEDGER-1). An exact retry returns the existing run
with 200; the same id under another flow refuses `run.exists`; an unknown or
retired flow refuses `run.flow_not_found`; a refused turn returns
`run.start_refused`, `run.flow_inadmissible` or the kernel's code, without its
detail. `GET /v1/runs/{id}` reads a run. A run passes the ordinary `values` read gate
on its object type; an unreadable run is `run.not_found`, exactly like an absent one. A run document carries
`terminal` (its state is a terminal state of its lifecycle) and its step
timeline, folded from the run object's committed versions: a lifecycle state
that waits on a step opens it, the next state closes it, and `completed` or
`failed_<tag>` ends the run. A step the cell does not move itself waits on a
`decision`, an `executor` or a `timer`; that is a state, not an error. An install
that changes the model tells the host's authorizer under the definitions write
epoch (`Authorizer::model_changed`), so the DO cell's read rules cover a flow's
Run type before any request sees it. `GET /v1/definitions:list?kind=flow`
carries, per flow, `canStart`: whether the caller may start it now, decided by
exactly the gate `POST /v1/runs` applies (an active flow and `StartRun`), so a
client offers only the starts the cell allows and the cell still decides at the
call. Named scenarios (`crates/norn-cell-do/tests/runs.rs`): **RUN-START-ONCE**,
**RUN-START-REFUSE**, **RUN-READ-GATED**, **RUN-CAN-START**.

### DECISIONS-1 — deciding what a run waits on

A flow lowers "ask a person" to a perform of `norn.decision@1`; the cell parks the
command (PARKED-1) and the run waits in `awaiting_perform_<step>`. The parked
command is the open decision: its request (question, risk, options, agent pick,
decider roles and law, expiry) is the step's input, its id is opaque
(`dec_`, bound to the command's generation), it was asked at its dispatching
commit's logical time, and it expires at most seven days later. `GET
/v1/decisions?state=pending` lists the open, unexpired decisions the caller may
decide, soonest deadline first, ties to the oldest request; `limit=0` returns only
the count. Whether a caller may decide is the host operation `Decide` over the
request's declared roles and law (in the DO cell, a verified role named by the
request); a decision that names a law the host cannot evaluate refuses
`decision.law_not_satisfied`. `POST /v1/decisions/{id}:decide` with `{choice,
note?, idempotencyKey}` refuses, in order: `decision.not_found` (also for a caller
who may not decide it, so existence is not disclosed), `decision.resolved` (with
who and when, and the choice only for a reader of the run), `decision.expired`,
`decision.law_not_satisfied`, `decision.unknown_choice` (`/choice`),
`decision.note_too_long` (`/note`), `request.invalid`, and
`decision.idempotency_conflict`. An accepted answer is checked against the flow's
result turn before anything is recorded; the cell then records `{choice, note?}` as
the parked command's result and commits the run's result turn (FLOW-DRIVER-1) with
the decision's resolution in one commit (DECISION-LEDGER-1). The same key and body
replays the original commit. A decider adds a condition and grants nothing: the
result turn runs as the cell, and the run's later steps act under the run
starter's authority, never the decider's. The receipt is disclosed by the
cell's question runtime; a cell without one decides nothing. A run document names
the open decision it waits on (`decision: {id, expiresAtMs}`) when the caller may
decide it. Named scenarios (`crates/norn-cell-do/tests/decisions.rs`):
**DECIDE-ONCE**, **DECIDE-REFUSE**, **DECIDE-EXPIRED**.

### ACTION-STEP-1 — a run's action steps act as its starter

A flow step that calls a governed action is a perform of the built-in
`norn.action@1` with `{action, input}`. Like a decision, the cell parks it
(PARKED-1), and the cell executes it itself through the ordinary governed action
path. The host records the verified principal who started the run (subject, actor
kind and the roles of its declared role table) in the run's start event. After a
start or a decision, the host runs the run's pending action steps, making that
starter the verified caller for each one call and restoring the request's own
caller on every path. A run therefore acts under its starter's authority for its
whole life: a decider's approval is a condition and lends the run none of the
decider's rights, so a starter without a right cannot obtain it by routing the
action after someone else's decision. The step's output is the action's target
`{objectType, objectId, version}`. A denial, or a run with no recorded starter or
one the host cannot act as, is the typed failure `Denied`; any other refusal is
`Refused` with its code; a host failure leaves the step pending for the next
drive, whose per-attempt idempotency key replays the call. The roles are those
verified at start, so a role revoked later still holds for the run until it ends;
a waiting run is bounded by its decisions' seven-day expiry. Named scenarios
(`crates/norn-cell-do/tests/action_steps.rs`): **STEP-AS-STARTER**,
**STEP-NO-LAUNDERING**.

### APPROVAL-1 — approval-gated action calls

An `approval` retained definition (`{action, decider: {roles, law?}, question,
risk, expiresInMs?}`) makes every call of one admitted action wait for a person.
At most one active approval gates an action; its decider roles are roles of the
cell's declared role table; installing or revising one changes who may complete
an action, so it is installed only by signing the server's restatement (#1094), as
a flow is. While it is active, the shared action dispatch (every surface: HTTP,
MCP's loopback, a flow's action step) runs the kernel's action pipeline for the
call against a host that serves every read and stops at the commit point. A call
the kernel would refuse is refused exactly as it would be ungated (the same
opaque denial for permission and visibility) and parks nothing. A call that
reaches the commit point parks: one `norn.decision@1` command carrying the
admitted call (action, input, idempotency key) and the requester's verified
principal, committed alone, answered `202 {outcome: "pending", decision: {id,
expiresAtMs}}`; a host with no verified principal for the caller denies it. The
decision's identity binds the action, the input, the key and the requester, so
the same call parks once and an approval approves exactly the call it was asked
about. `GET /v1/decisions` lists it with its `call` to holders of a decider role,
never to its requester, who cannot decide it either (#618). Deciding records the
resolution in DECISION-LEDGER-1 and runs no flow turn. The resolution is the
record the dispatch consults: an approved call commits through the same dispatch
under the requester's current authority, with everything (including the laws
the cell checks at commit) decided again there. That authority is never the
roles recorded when the call parked: the approver's decide runs the call at
once under the requester's membership and roles as the organization's
authority answers them now (APPROVAL-LIVE-1), and a host that cannot ask
reports `call: {committed: false, state: "not_executed"}` until the
requester's own retry commits it once (APPROVAL-RECOVERY-1). A rejected call's retry refuses
`approval.rejected` (409), an expired one's `approval.expired` (410); neither
ever commits. A flow step's call parks the same way; the step waits, and once
the call is approved the starter's own retry of the run (the exact start
retry drives it) runs the call and moves the run. Named scenarios (`crates/norn-cell-do/tests/approvals.rs`):
**APPROVAL-NO-RIGHT-NO-PARK**, **APPROVAL-COMMITS-AS-REQUESTER**,
**APPROVAL-BINDS-INPUT**, **APPROVAL-REJECTED**, **APPROVAL-EXPIRED**,
**APPROVAL-STEP-PARKS**, **APPROVAL-STEP-REJECTED**, and on hosts serving MCP
**APPROVAL-MCP-PARKS** (a tool call answers the same pending decision).

### APPROVAL-RECOVERY-1 — an approved call survives a crash

PARKED-CRASH for approvals (#1481). The approved call commits after the commit
that records its approval, so a host can stop in between. Nothing ever runs
an action call without a request: no wake, alarm or cold open dispatches one.

The commit that records an approval of a parked call also records the call
(`_norn_approved_call`, derived from the decision ledger in the same
transaction). The commit that runs an approved call carries one
`norn.approval.consumed` ledger event naming the approval it consumed, in the
same transaction, so every commit is attributed to exactly one approval. Where
the call stands is derived, never a log line: **refused** once a refusal is
recorded for it (final: no later commit under the same key, under another
gate revision's approval or not, turns it into committed); **committed** when
the commit that consumed this approval wrote its idempotency record (a cell's
approvals recorded before consumption was are matched by their exact record:
action, requester and input); `action.idempotency_conflict` when another
commit used the key; and otherwise **not executed** (approved, not yet
executed). The decide answer carries it as `call: {committed, code?, state}`
(`state` is `committed`, `refused` or `not_executed`), on a first decide and on
every replay; the approver's decide with another key answers
`decision.resolved` with the same `call`, read only.

An approved call that is not executed runs only on the requester's own
request, under the requester's current authority: the requester's retry of
the call (for a flow step, the starter's retry of the run). A request by
anyone else, the approver's decide and its replay included, cannot establish
that authority and runs nothing: the call stays `not_executed` for the
requester's authenticated retry, so a role revoked after the call parked is
revoked for it. The dispatch takes the request's authenticated principal apart
from the subject the call runs as, and consumes an approval only when that
principal is the parked requester (the same subject, actor kind and role
table), compared under its own hold; any other principal gets the opaque
action denial and nothing is recorded. A flow drive chooses whose credentials
try a step's call before it holds the cell, so a try under the starter's
recorded authority carries no authenticated principal and never consumes an
approval recorded meanwhile: the step stays pending for its requester. The try runs through the shared dispatch, and only as exactly
the approved call: its command id (requester, input, idempotency key, action
digest, gate definition and revision, definitions pin) must be the approved
one, compared inside the dispatch's epoch and cell hold (APPROVAL-REVISION-1,
APPROVAL-PIN-1); a flow step's call carries the exact step attempt it was made
for (its run, its parked command and generation) from the moment it parks,
named by the flow's own dispatch and never derived from the idempotency key,
and runs only by that dispatch and only while that attempt waits on it
(`approval.attempt_ended` otherwise, also when the attempt ended before anyone
decided). A step attempt's call key (`step-` and 48 hex digits) is the host's
own: a direct call under it is refused `request.invalid` before anything parks
or commits, so a direct call never supplies a step's outcome. A refusal that decides the
call there (anything but a host failure or a denial of authority, which is
decided again on every try) is recorded in its own commit, under the same
cell hold, as one `norn.approval.refused` ledger event carrying its status,
code and category, so it is final: a later try gets the recorded refusal,
served with its recorded category, without anything being decided again, and
export replay rebuilds it, so a restored cell never makes a refused call due
again. Before recording, the dispatch reads the call again under that hold:
an exact commit is never masked by a later refusal. A replay of a committed or
refused call answers its recorded outcome and runs nothing. A cell whose
approvals were recorded before approved calls were gains their records from
its decision rows when it opens, exactly as export replay rebuilds them. The
commit that runs an approved call names the approval it consumed, and whether
a commit consumed an approval is read from those ledger-derived records alone,
so an opened file and its restored export answer alike: the exact call's commit
that consumed no approval (one made before consumption was recorded, or an
ungated one) is the approved call's commit; one that consumed another approval
never is. One commit never both refuses and consumes the same approval.

A flow step's approved call is the same call. An approval binds a step only
when its parked call is exactly the call the step attempt makes (parked for
that attempt, under its idempotency key, the action, the input, no definitions
pin, and the run's starter, its whole recorded identity, as requester);
another call under the same key never delivers its result to the step, and of
several bound approvals the one the ledger committed last supersedes. A call
that commits without rewriting its target reports the target's version as of
its own commit, on its replay and its recovery alike. A step whose call
already has an outcome (committed, or refused other than
`approval.definition_changed`) takes that outcome without the action being
called again: the run's drive and the host's wake (TIMER-ALARM-1) deliver it
the same way, as due work until the run consumes it, including a result
recorded but not consumed. A denial of authority on the starter's try leaves
the step pending and its approval executable. What a try of a step's call ends
with is reconciled with the step's approval and recorded under one cell hold:
an approved call that has its outcome by then (another request committed it)
gives the step that outcome, never the try's denial.
A step whose approval no longer covers the call (`approval.definition_changed`)
parks anew for a fresh approval on its next drive. Named scenarios
(`crates/norn-cell-do/tests/approval_recovery.rs`): **APPROVAL-RECOVERY-BEFORE**
(a restart before the commit: visible as not executed, nothing due, the
requester's retry commits it once), **APPROVAL-RECOVERY-REPLAY** (the
approver's replay runs nothing), **APPROVAL-RECOVERY-REVOKED** (a requester
revoked after parking never gets the call run), **APPROVAL-RECOVERY-CONFLICT**
(another call under the key is a conflict, never a commit),
**APPROVAL-RECOVERY-REFUSED** (a recorded refusal keeps its category and stays
final after a restore), **APPROVAL-RECOVERY-UPGRADE** (an in-place upgrade
backfills approved calls as a restore does), **APPROVAL-RECOVERY-STEP-BEFORE**
(a step's call runs once on the starter's retry) and
**APPROVAL-RECOVERY-STEP-AFTER** (a committed step call's outcome reaches its
run on the wake or the drive, with no second call).

### APPROVAL-LIVE-1 — an approval runs the call under the requester's current authority

Approving a parked call runs it at once, on the approver's decide (fresh or
replayed) and on the drive of a flow step waiting on it, under the
requester's authority as the organization's authority answers it at that
moment. The approver's request carries no credential of the requester's, so
the host asks:

- **Hosted cell.** Studio's auth (Better Auth organizations and API keys) is
  the authority. The cell issues a fresh one-use challenge (derived from host
  entropy, never predictable) bound to the exact attempt: the approval
  (`norn.approval/1:<decision>:<parked command>`, the exact call) or a run's
  step call (`norn.step/1:<run>:<call key>`). The cell Worker posts
  `{orgId, subject, kind, challenge, binding, onBehalfOf}` to Studio's
  `/api/internal/authority` with a service credential
  (`NORN_AUTHORITY_TOKEN`, held by both, never a `NORN_SECRET_*` variable a
  connection may name). The credential alone queries nothing: `onBehalfOf`
  is the host token of the request the cell is serving, which Studio
  verifies as its own, and Studio looks up only that token's organization
  (`403 authority.out_of_scope` otherwise). Both outcomes are statements
  Studio signs with its host-token keys for the audience `norn-authority` and
  the method `membership`: a member now (an enabled, unexpired API key of the
  organization with a service role, for a service) with its role, or not a
  member, each naming the challenge and the binding, living at most one
  minute. The cell verifies the statement as it verifies a host token
  (pinned issuer and JWKS, audience, its own organization, declared role
  table) under the attestation's own bound (60 s, 5 s of skew), and only as
  the answer to that outstanding challenge, for that requester and attempt;
  the challenge is spent on the first answer. A verified answer holds for the
  rest of the request it was provided for; an attestation never
  authenticates a request, and a host token is never an attestation.
- **`norn dev`.** The local principal table is the authority: a local actor
  with a session is a member with every hosted role; anyone else is not.

Every attempt that can run an approved call runs only under that answer while
the host looks requesters up: the approver's decide and its replay, the
drive of a step waiting on it, and the requester's own retry too, which is
checked where the approval is consumed (under the dispatch's hold, before
anything is recorded or committed), so the roles a still-valid token was
issued with never stand in for the current ones. A drive an approval moves
runs the run's later actions under the starter's current authority as well.

A decide is validated first (scope, decider, expiry, choice, exact replay); a
rejection asks nothing. An approval that would run a call asks the host before
anything is recorded: without a verified answer the decide is refused
`503 approval.authority_unavailable` (retryable) and records nothing, and the
hosted Worker looks the requester up and serves the same decide again. An
exact replay that would still run the call runs it only while its decider may
still decide it (else `403 decision.law_not_satisfied`, nothing runs); a
replay of a recorded outcome is read-only. A drive that reaches an action
needing an answer asks for that step, and the host resumes only the drive, so
nothing the request recorded is replayed.

- **A member now.** The call runs once, as the requester with its current
  roles, through the shared dispatch under the exact-intent fence of
  APPROVAL-RECOVERY-1.
- **A signed "no member", or current roles the policy denies.** The approval
  stays recorded and the call is refused for good,
  `approval.requester_unauthorized` (403, `permission`), recorded as a
  `norn.approval.refused` event like any final refusal; a flow step waiting
  on it takes that refusal as its outcome. Nothing runs with the roles
  recorded when the call parked.
- **No verified answer** (unreachable, a timeout, an unsigned or forged
  negative, an answer to another challenge or attempt, an expired one).
  Nothing runs and nothing final is recorded
  (`approval.authority_unavailable`, retryable).
- **A host without a lookup** leaves the call `not_executed` for the
  requester's own retry, which runs it under its own verified credential, as
  APPROVAL-RECOVERY-1 states.

Named scenarios (`crates/norn-cell-do/tests/approvals.rs`):
**APPROVAL-LIVE-CURRENT**, **APPROVAL-LIVE-REVOKED**,
**APPROVAL-LIVE-UNAVAILABLE**, **APPROVAL-LIVE-STEP**,
**APPROVAL-LIVE-STEP-REVOKED**, **APPROVAL-LIVE-CONTINUATION**, the demoted
decider's replay, the requester's own retry under stale token roles, the
action an approval reaches, the decide validated before it asks, and the
resumed drive; the statement's verification is in
`crates/norn-cell-do/tests/tokens.rs` and `crates/norn-api/src/principal_tests.rs`,
and Studio's answers in `apps/studio/worker/test/authority.test.ts`.

### DECISION-LEDGER-1 — resolved decisions

The commit that consumes a parked decision's result carries one
`norn.decision.resolved` ledger event (decision id, command and generation, run,
step, resolution `decided | expired | withdrawn`, and for a decision the choice,
note, deciding principal, idempotency key and request digest). The store records
it in `_norn_decision` in that commit's transaction. A resolution refuses
(`decision.resolution_invalid`) unless its commit consumes exactly that command's
result, and a second resolution of the same decision refuses `decision.resolved`;
nothing commits either way. The ledger derives only from committed batches, so
export replay rebuilds it; it is a read index for replays and "who was first", not
authority, and the typed commit reader skips the event as bookkeeping.

### PARKED-1 — parked perform commands

A perform of a built-in that a person resolves (`norn.decision@1`,
`norn_builtins::parks`) is a parked command. It is enqueued in the outbox like
any command, in the same commit, but it never occupies the FIFO perform head:
`perform`, the head lookup and perform claims skip it, so a waiting decision
never blocks other work. The cell completes it out of order with the result it
produced itself (`record_parked`, `SqliteCell::complete_parked`), at most once
per generation; a second or concurrent resolution, another generation, or a
command that is not parked writes nothing. Its delivery is then pending like
any result (`Consumed::Result`), and the ordinary delivery fence consumes it
exactly once. A recorded, unconsumed parked result is redelivered from the store
(`SqliteCell::parked_results`), so a crash between recording and consuming never
strands it. A perform claim on a parked command, live or in a retained journal, refuses
`restore.parked_claim_unsupported`; no build before parked commands could record
one. Every other command keeps strict FIFO order. Parking is derived
only from the command's own payload with the compiled-in registry, so every host
and every export replay classifies a command identically; replay checks a parked
consumption at its own outbox position and every other at the FIFO head. Named
scenarios (`crates/norn-cell/tests/parked.rs`): **PARKED-HEAD**, **PARKED-FIFO**,
**PARKED-ONCE** (decide then expire, replay), **PARKED-RACE** (decide racing expire,
then two consumers, on two connections to one database file), **PARKED-CRASH**
(restart before and after the consuming commit, redelivery from the persisted
result), **PARKED-INTERLEAVING** (property).

### Durable flow scheduling and identity (#515)

Flows remain compiled lifecycle/rule declarations. The pure kernel helpers in
`flow_identity` and `timer_schedule` implement these named invariants:

- **FLOW-KEY-1:** a durable attempt key is the Norn1 digest of a canonical,
  domain-separated tuple containing scope, world, pinned flow/version, instance,
  admitted step site and attempt. Identical delivery preserves the key; changing
  any component changes the identity (subject to SHA-256 collision resistance).
  Structural framing prevents delimiter aliases. Reusing a durable step object
  at two authoring sites remains invalid; hashing does not replace admission.
- **FLOW-TIME-1:** the scheduling time is `max(host clock sample, persisted cell
  clock floor)`. Delay addition is checked; overflow refuses the whole proposal.
  No evaluator reads wall time. A virtual clock can supply every test sample.
- **FLOW-ORDER-1:** among live timers due at that time, select the least `(due,
  timer id)`, independent of enumeration order. The cell retains one incarnation
  per timer identity; selection grants neither permission nor consumption.

Named scenarios: **FLOW-KEY-FRAMING** separates ambiguous colon-delimited
components and each identity dimension; **FLOW-KEY-REDELIVERY** returns the same
SQLite receipt after reopen without a second event; **FLOW-KEY-MISMATCH** refuses
changed payload under an existing key; **FLOW-TIME-BOUNDARY** checks exact due
boundaries, clock rollback, empty queues and overflow. Generated tests sample key
uniqueness, arbitrary redelivery orders and scheduling correspondence. The bounded
Kani harness checks full-width deadline arithmetic and ordering over three live
identities, not cryptographic collision freedom or scheduler liveness. Run it with
`kani --fail-fast --output-format terse verification/kernel-l1/timer_schedule_kani.rs`.
This harness is enrolled source evidence, not a promoted full-profile gate.

The dispatch integration is a subsequent implementation unit. It must persist
schedule/cancel alongside lifecycle state, commands and receipts, re-enter the
same governed action path with current authority/laws, and consume the exact timer
incarnation atomically with the accepted consequence. A timer never confers
permission. At-least-once firing plus durable attempt identity provides one
committed effect: a retry after a lost acknowledgement reuses the original
commit's attempt idempotency key, so the existing receipt lookup returns the
stored receipt instead of committing again. Provider retries use the separate, attempt-independent operation
key only with an explicit provider deduplication/lookup contract; uncertain
non-idempotent outcomes require reconciliation. No universal external exactly-once
claim follows from a local receipt.

Reopen resumes committed pending timers using the persisted clock floor. Restore
must reconstruct timers from the verified committed prefix while fencing automatic
timer dispatch, as it fences pending external commands. Replay itself performs
nothing. Only explicit governed resumption may clear the restore fence after
current scope, world, authority and pinned definitions are validated. These firing
and restore requirements are not implemented by the pure helpers alone.

## Bounded synchronous action profile

TXN-RULE-1 / TXN-CLOSED-1: an action may name one exact `synchronousRule`
artifactId/version and must declare the same required Rule dependency. Initial
admission accepts this contract only on transition actions and requires one
matching `decide` artifact whose required scope is covered by the action.
Missing, ambiguous, wrong-version, wrong-contract or uncovered-scope bindings
refuse. Unknown pin members and null pins refuse strict decoding.

Execution resolves the exact admitted writer before host access. A missing or
ambiguous closure refuses rather than executing only the primary transition.
The immutable `SynchronousRule` seals its RuleIR root, helper closure and decision
binding. The ActionCall receipt binds their canonical digest together with the
action, scope and world; changed writer intent cannot replay an earlier success.
Named round-trip/refusal scenarios, generated version-pin cases and authoring
dependency correspondence bind this contract to the code. Pin resolution
traverses heap-backed artifact catalogs; the existing Kani profiles cannot call
that catalog/serde admission path. No new bounded arithmetic primitive or
handwritten application proof is introduced.

The pinned pure graph evaluates over the same authorized captured slice and
effective input, with the authenticated call actor and operation ID. It shares
the action's remaining fuel; exhaustion refuses. Rule facts are applied before
the primary transition so its guards see the computed proposal. Each actual
writer must be declared, and modifications require a captured object. Reserved
framework writes, deletes and a second primary transition refuse. Every current
and proposed object is authorized; fact, absence and membership fences remain
in force. Facts, the primary transition and the ActionCall receipt finalize in
one cell commit. Rule output is facts-only in this profile; existing action
command declarations enqueue payment intent in that same commit. Dispatch and
payment success remain external.


### Atomic proposal links (TXN-ATOMIC-1, TXN-CLOSED-1)

The synchronous writer's closed fact schema has finite nonempty `objectType`
and supported `op` enums. Admission conservatively takes their Cartesian product
and unions it with the primary effect; this must equal the action's write set.
Open targets and unsupported operations refuse admission. Preparation still
checks every actual effect against that set.

A create-link binding optionally names a concrete source type and required
string input key. Without it the primary object remains the source. Explicit
sources require the transition/synchronous-rule profile. Every source must be
written in the same checked proposal; each target is the captured primary, a
required slice object, or an object created in that proposal. The link must be declared with the exact
target type. All proposed facts are authorized before commit. Duplicate concrete
links and dangling endpoints refuse. Link versions and all fact/absence/slice
fences join the existing single cell commit; no link is persisted earlier.
These rules are domain-neutral. The correspondence scenarios cover bidirectional
links to a newly created secondary fact, exact replay, and refusal before writes.

The link closure is bounded at sixteen bindings, checked before host access.
The source is an object-only closed binding; explicit nulls, sequence aliases,
unknown members and duplicate members refuse. Bounded inline list indexes in
authored expressions retain the declared item schema and resolve at compilation.
Malformed or out-of-range inline indexes refuse. Indexes into declared runtime
arrays refuse during authoring because the admitted expression profile supports
only indexes into list literals; no nullable missing-item semantics are inferred.

`typed_atomic_writer_admits_and_an_uncovered_creation_refuses` checks full
artifact-set admission, including the action's dependency pin, against the
compiler-generated fixture. Separately,
`atomic_action_links_outbox_and_receipt_survive_reopen_and_exact_retry` checks
kernel/SQLite correspondence using extracted object types and an independently
admitted rule; it does not recheck the action envelope's dependency pin.
Both directions of the link,
the secondary fact, primary transition, receipt and one pending payment intent
survive reopening; exact retry changes no table and changed principal refuses.
`late_link_failure_and_secondary_denial_persist_no_partial_proposal` covers a
host-side cardinality failure and denied secondary authority, with complete
SQLite dumps unchanged before and after reopening. No provider executes.
This is runtime-enforced correspondence plus sampled generated identities and
writer footprints, not a proof of application conservation or approval policy.

`txn_all_refuses_atomically_and_preserves_the_retained_receipt_after_reopen`
composes the same catalog in typed Rust with the existing `ActionQuantityEffect`.
Full artifact-set admission precedes execution. A final link-cardinality refusal
leaves every SQLite table unchanged, including quantity heads, postings, outbox
and the reservation timer, before and after reopen. Success commits the facts,
both links, quantity posting, command intent and reservation-timer cancellation;
an exact retry after reopen preserves the action record and original retained
receipt, while changing the posting identity refuses. This is runtime-enforced
correspondence for that local effect composition, not universal writer closure
or application laws.


### Literal patch target correspondence

**RULE-PATCH-SHAPE-1:** a closed, complete, duplicate-free literal patch is
checked against the object type that its record actually targets. Optional
shared-schema members owned by other declared targets do not become writes on
that record. Actual members still obey that target's effective stored-property
contract; a schema member declared by no target still refuses. Computed,
non-literal, incomplete or duplicate-key shapes retain the full-schema check.
The scanner must prove closure before applying this precision.

Named correspondence scenarios cover the Exchange `eligible` patch alongside
Ledger `account_code` and Conversion `output_gross`, invalid members/values on
the actual target, and computed/incomplete fallbacks. Generated patch sets over
two or three targets check that each record's errors depend on its own target.
This is sampled checker correspondence; runtime stored-property checks remain
mandatory. The allocating scanner and effective catalogs are outside the
bounded Kani harness profile.

### FA-PROTECT-1: caller writes and kernel initialization

A protected property is inherited conjunctively: a subtype or extension cannot remove an ancestor's rule-only write requirement. Unsealed actions, decisions and observations cannot supply protected fields, transition a protected lifecycle or delete an existing object with protected properties. Owned-row admission validates the caller's patch first, then may initialize declared defaults under its private kernel origin; callers cannot claim that origin. Until sealed-rule evaluation is connected, these writes fail closed. Named scenarios: `protected_delete_then_recreate_is_refused`, `owned_row_defaults_are_kernel_values_but_protected_inputs_are_refused`, and the generated-chain `protected_fields_cannot_be_weakened_anywhere_in_a_subtype_chain`. Admission-time action-target and kernel row-field diagnostics follow separately; runtime enforcement does not depend on that earlier rejection.

Observed deletes cannot transition or tombstone objects with protected properties. Owned-row caller-selected identity and order never count as kernel default initialization. Observation creation retains unique initial-state initialization after caller input is checked. Named regressions: `an_observed_delete_cannot_transition_a_protected_lifecycle`, `owned_row_actions_refuse_protected_framework_identity_and_order`, and `an_observed_create_keeps_unique_initialization_without_a_property_default`.

The current protected-property profile applies to object properties. Link values have no sealed writer-authority path, so `protected: true` on a link property refuses definition admission with `object_type.validation.link_property_protection_unsupported`, including extension declarations. `link_protection_is_refused_for_definitions_and_extensions_until_authority_exists` covers both the refusal and unchanged unprotected link admission.

### FA-IMM-1: append-only object declarations

`ObjectTypeDef.immutable` defaults to false. An immutable declaration is inherited through every supertype and cannot be weakened by a subtype. New objects may reach their final state within their first decision; an instance that existed before the decision cannot be set, transitioned or deleted. The final commit rechecks the stored concrete type, including direct host writes. A refused write leaves facts, ledger, commands and timers unchanged.

Named scenarios: `immutable_objects_use_pre_decision_existence_for_change_sequences` (new/existing decisions); `inherited_immutable_objects_refuse_raw_overwrite_and_delete_atomically` (subtype creation and raw mutation refusal); `immutable_ancestry_controls_every_existing_instance_write` (generated values and inherited flags). Raw commits use `commit.immutable_object`; decision preparation uses the existing `decision.invalid_output` with an immutable reason. This implements the append-only part of TXN-HISTORY-1, without claiming event-sourcing or historical-query completeness.
