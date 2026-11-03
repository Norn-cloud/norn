# Core 04: Checked transactions and effects

Purpose [Astra]: every governed writer lowers into one closed checked calculus with complete read dependencies and one atomic commit. [Common conventions](README.md) apply.

## IR contract

Reuse `DecisionOutput`, `FactChange`, kernel `CellCommit`, `FactExpectation`, `FactWrite`, `Command`, `Timer`, `IdempotencyKey`, `Generation`, `LogicalTime` and `World`. Extend these families rather than inventing application posting opcodes.

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
| TXN-READSET-1: absence/membership/definition/settings/auth dependencies fenced; commit | `core.txn.contention` |
| TXN-INTENT-1: user preconditions survive retries; preparation/commit | `core.txn.stale_intent` |
| TXN-IDEMPOTENT-1: one accepted consequence per exact intent; commit | `core.txn.identity_mismatch` |
| TXN-HISTORY-1: posted facts never overwritten/deleted; admission/commit | `core.txn.immutable_history` |

Expected evidence: runtime atomicity/CAS enforcement, crash/reopen/race correspondence, generated per-writer preservation and explicit external-boundary goals. A safe constructor only certifies its own outputs; arbitrary account selection or imported data does not inherit that proof.

RECEIPT-AUTH-BIND-1: `acceptReceipt` preparation accepts a content-digest reference only. The tenant/world/authority-scoped host receipt store resolves authenticated `Acceptance` plus `FunctionReceipt` and consumption state. A bound result retains the exact receipt/result/command/generation pins and reconciliation reads. Unknown, replayed, mismatched, failed or wrongly typed results refuse; an action-supplied receipt body always refuses. Receipt consumption and destination writes remain one checked commit; binding alone grants no publication authority.

GUARD-DIAGNOSTIC-1: a transition may declare `guardRefusalCode`, naming an error diagnostic registered at runtime tier T6. Admission refuses unknown/non-runtime codes with T2 `lifecycle.validation.invalid_guard_diagnostic`; replacing an inherited alias uses T3 `lifecycle.validation.inherited_guard_diagnostic`. A redeclaration retains an inherited diagnostic; omission inherits it. A refused guard exposes the selected code while retaining the original guard expression, failure reason and underlying code. Diagnostic selection changes no admission, authorization or transaction behavior. Core 08 publication uses `core.projection.publication_guard`.

## Named acceptance scenarios

- **TXN-ALL:** Given receipt quantity/value and matching GL effects, when a final guard fails, then no voucher, stock, GL, command or timer changes; successful replay returns the whole same commit.
- **TXN-PHANTOM:** Given a fold over an empty or populated partition, when another commit inserts/removes a member before finalization, then the stale proposal commits nothing.
- **TXN-TWO:** Given balance 5, when two consumers each request 4, then at most one succeeds and the other recomputes/refuses without negative balance.
- **TXN-RETRY:** Given committed operation O, when O is retried after lost acknowledgement, then return its authorized receipt; changed amount under O refuses.
- **TXN-APPROVAL:** Given approved membership A+B, when membership becomes A+C, then refuse stale intent even if quantity totals match.
- **TXN-CROSS:** Given one immediate law spanning two cells, when admitted as atomic, then refuse unless co-located; a declared pending commitment process remains distinct.

## Main and gaps

Present: `crates/norn-ir/src/{decision_output,fact_change,action}.rs`, `crates/norn-kernel/src/{decision_commit,cell_host,effect}.rs`, `crates/norn-cell/src/{host,store,write,quantity}.rs`. Main has atomic decision/CAS/outbox paths and generic quantity primitives, not demonstrated universal writer closure. Lane A #323/#324 and effect composition must converge here. Host #270/#308–#312 and sampled surface suite #317 are pending, not transaction proofs. Lane C #227 uses declared rules/templates; no production stock helper qualifies.

## Work breakdown (each PR ≤400 lines)

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
Execution stays closed until consumption and effects commit atomically, with no partial persistence on refusal.

RECEIPT-STORE-1: only authenticated reconciliation of the cell’s recorded performer result may populate its scoped, persisted receipt store. Intake records source receipt and knowledge time; it grants no action/publication authority. Unknown, forged, wrong-scope or differently recorded results refuse without a write. Predicate and collection freshness adapters are currently unavailable and refuse closed. Generic event export does not restore private receipt authority.

RECEIPT-ONCE-1: SqliteCell resolves references only from its authenticated scoped store. The accepting transaction checks the intake frontier, claims one unconsumed reference, consumes its performer delivery and writes the accepted command state with the action effects. Any refusal rolls back the claim and effects; replay, unknown, stale or mismatched manifest references refuse. Unsupported hosts refuse. This host boundary alone does not open acceptReceipt action execution or publication.

RECEIPT-EFFECT-1 execution: non-Main calls refuse before retry lookup; only Main exact retries may replay. After current authorization and exact action retry lookup, acceptReceipt resolves a reference through the authenticated host seam. The authenticated acceptance must carry a destination pin for the exact action target; missing or mismatched pins refuse. Its digest and result bindings are typed property changes before the lifecycle transition. Guard/type failure leaves evidence unconsumed. The host atomically consumes only on successful action commit. Historical/fork execution and receipt-plus-quantity combinations refuse.
RECEIPT-STORE-AUTH-1: the intake ledger event binds the canonical digest of the complete stored receipt, acceptance, source and frontier. Preparation captures its frontier before reconciliation; commit rechecks that frontier and actual performer delivery atomically. Resolving or exporting evidence verifies the complete body against that event. Changed acceptance/read pins, a forged frontier and legacy events lacking the authority digest refuse; refusal grants no authority and writes no action effects.

The cell is the trusted committer: receipt rewriting by a party with cell-store write access is out of scope until external receipt anchoring.

RECEIPT-ONCE-1 commit-context limit: acceptance requires the exact authenticated intake frontier. Receipts are not re-authenticated after intervening commits, even unrelated commits; acceptance then refuses without consumption or effects, while retained evidence remains readable. Expected receipt refusals are typed precondition failures (`action.receipt_refused`) with safe reason detail; SQL/storage failures remain internal and redacted. Named scenario: intervening_commit_refuses_without_consuming_receipt. Post-demo receipt refresh is tracked with native memory accounting in https://github.com/Norn-cloud/norn/issues/508.

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
