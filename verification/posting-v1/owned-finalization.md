# Owned-row quantity finalization: kernel expand contract

Part (c) of #214. The existing kernel-owned proof carries an optional prepared
quantity request and its unchanged `action.quantity/1` identity. Preparation
is shared with ordinary actions. The native host feeds both the proof and
quantity intent into its existing `commit_with_authorization` transaction.
No new quantity operation, valuation algorithm or posting authority is added.
Definition admission still rejects owned-row quantity combinations until part
(d) adds the checked posting bindings. The native fixture exercises the kernel
join directly; it does not claim an admitted stock action or alpha surface.

- POST-ATOMIC / OWNED-QUANTITY-JOIN: parent, children, manifests, links, events,
  action receipt and quantity intent publish in the same guarded transaction.
  Quantity refusal publishes none of them.
- POST-AUTHORITY / UNSUPPORTED-HOST: a host lacking combined quantity admission
  refuses before publishing owned rows; it cannot silently discard the intent.
- POST-IDENTITY / SHARED-QUANTITY-KEY: ordinary and owned actions derive the
  same identity from action ID and idempotency key; exact retry replays once.
- QUANTITY-FENCE-SCHEMA: balance fences use closed account/unit/layer/version
  records; incomplete nested object schemas refuse before finalization.
- EXPAND-PARITY: existing actions without quantity intent retain their original
  route, proof, authorization, fences, accounting admission and receipt.

`owned_row_finalization_and_quantity_intent_share_one_commit` exercises native
parent/child creation with a reservation, identical retry and insufficient
funding refusal. Existing action, owned-row, quantity and accounting suites
remain required. This is execution conformance for the join, not stock/GL
lowering; POST-VALUE and the remaining POST-ORDER/IDENTITY scenarios still need
receipt/delivery conformance in part (d). Direct Post and protected rule-output
admission remain subsequent changes.

## Declared direct Post expansion

A quantity-backed action may select `Post` with a definition-pinned template,
posting identity field and exact parameter bindings. The configured ledger's
certificate, funding checks and caller balance fences still govern admission.
The same binding validator covers owned-row and ordinary business effects.
No configuration or opening operation is admitted through this declaration.

POST-ATOMIC and POST-IDENTITY conformance extends the native owned-row witness
to direct Post: the ledger gains exactly one commit containing the parent,
child, membership, manifest, action receipt and quantity posting. Insufficient
funding leaves every one absent, with no new ledger commit or quantity version.
An identical retry adds nothing; a conflicting input refuses without a commit.
This is runtime-enforced native quantity conformance, not CP1 stock acceptance.
Closed Receipt/Delivery selection, master loading, StockEvents, GL legs and
alpha invoke/replay remain obligations of the stock-specific wiring.

Typed TypeScript authoring exposes this direct Post declaration through
`Norn.action.quantityEffect`. The syntax compiler preserves its closed bindings
and rejects unsupported operation kinds, extra members and invalid scales.
Reserve/PostReservation remain available through the existing Rust IR.

POST-REFUSAL-REASON: typed quantity refusals survive the host/commit/decision
boundaries. The public refusal retains insufficiency as a readable precondition;
corrupt state and clock regression retain the internal-details redaction.
