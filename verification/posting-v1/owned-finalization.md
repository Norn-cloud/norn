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

## Closed stock binding (P1)

POST-BINDING: `stockPost` is a closed receipt/delivery declaration on an
idempotent, scoped `status.submit` action. Its collection must be owned by the
voucher. Item/warehouse paths start at each owned line and traverse 1–4 required
to-one links to `stock.Item` / `stock.Warehouse`. Account paths end at text
properties on that warehouse or its company. Missing masters, unresolved links,
wrong types, nullable accounts, arbitrary operations and template overrides refuse
admission. The real checker conformance test is `POST-BINDING`.

POST-AUTHORITY / PREPARATION-REQUIRED: admitting a declaration grants no write
authority. Until kernel preparation and atomic commit are implemented, execution
returns UnsupportedEffect before any business or idempotency write.

Lane c: IDs are `stock.receipt.submit` and `stock.delivery.submit`; the alpha seam
is `invoke(surface, action, args, actor)`. Action input is `{ id: voucherId }`;
scope and idempotency key remain invoke transport metadata. Lines and account
identities are read from owned manifests and linked masters, never action input.
P1 is admission evidence only; the genuine alpha acceptance remains P4.
