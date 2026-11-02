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

The cell is the trusted committer: receipt rewriting by a party with cell-store write access is out of scope until external receipt anchoring.

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
committed effect. Provider retries use the separate, attempt-independent operation
key only with an explicit provider deduplication/lookup contract; uncertain
non-idempotent outcomes require reconciliation. No universal external exactly-once
claim follows from a local receipt.

Reopen resumes committed pending timers using the persisted clock floor. Restore
must reconstruct timers from the verified committed prefix while fencing automatic
timer dispatch, as it fences pending external commands. Replay itself performs
nothing. Only explicit governed resumption may clear the restore fence after
current scope, world, authority and pinned definitions are validated. These firing
and restore requirements are not implemented by the pure helpers alone.
