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
