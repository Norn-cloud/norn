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
properties on that same warehouse or its company. Line quantity (and receipt
rate) must be stored, required scale-six decimals. Missing masters, unresolved links,
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

## Authoritative preparation reads (P2a)

POST-READ-ORIGIN: `prepare_stock_snapshot` accepts only the admitted call, not
caller-supplied lines, accounts or history. It reads stable row IDs and order
from the owned manifest, requires the complete parent-link set to agree, and
resolves each declared master/account path through complete to-one link reads.
Every business object passes the existing type and object read authorization.
The parent snapshot is the same snapshot authorized for the action. All facts
and adjacency sets are retained for commit fencing; this read-only API grants
no write authority, and the P1 execution refusal remains in place.

POST-MASTER-PARTITION: each warehouse must belong to the voucher's company;
only FIFO items are accepted in this CP1 cut. Lines retain manifest order
within deterministic company/item/warehouse groups. Missing or inconsistent
ownership, masters or accounts refuse. POST-EXACT-INPUT: quantities and rates
convert to integer millionths without rounding; negative and finer-scale
values refuse (`post_value_exact_millionths_never_round_or_accept_negative`).
History enumeration, the absent-partition fence and fold planning follow in P2b;
atomic finalization and installed alpha acceptance remain P3 and P4.
