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

## Shared extension-link contract

POST-LINK-PARITY: link lookup and relation ownership include admitted extension
members on every type in the lineage, using the same nearest-declaration and
furthest-owner rules as ordinary links. The stock checker can admit these paths;
the shared commit shape check must accept their complete read fences too.
`extension_links_share_declaration_owner_and_empty_fence_admission` checks base
and subtype lookup, common owner, successful empty-fence admission and continued
refusal of an undeclared extension member. No link target/cardinality, scope,
CAS or complete-adjacency guard is weakened. This fixes the shared lookup before
stock preparation consumes it; full stock invocation remains subsequent work.

POST-LINK-REQUIRED: the same declared-member inventory supplies required-role
validation. A source with a required extension role refuses creation without
a target and removal of its last target, on base types and subtypes alike.
`required_extension_links_refuse_missing_or_removed_last_targets` exercises
both refusals.

## Authoritative preparation reads (P2a)

POST-READ-ORIGIN: `prepare_stock_snapshot` reads manifest-ordered owned lines and declared master/account paths. Complete parent links must agree with the manifest. Facts and complete link sets survive as fences. Type authorization precedes manifest/adjacency reads; object authorization precedes disclosure, retaining the action-authorized parent snapshot. `post_read_origin_denied_reads_preserve_business_state` checks denied-read no-write behavior. The API grants no write authority.

POST-MASTER-PARTITION: warehouses belong to the voucher company; master kinds follow admitted type lineage, including subtypes. CP1 requires FIFO. POST-EXACT-INPUT: quantities/rates become integer millionths without rounding or negative values (`post_value_exact_millionths`). History/fold planning follows in P2b; atomic commit and alpha acceptance in P3/P4.

## History schema and refusal contract (P2b expansion)

POST-HISTORY-SCHEMA: the typed stock fixture declares a partition witness with
an event count and complete `events` adjacency. Each event names its partition,
identity, dense ordinal, closed movement kind and exact scale-six quantity/value.
Company masters declare the book/graph/version/currency used by planning.
`stock.artifacts.json` is generated from `stock.source.ts` and checked for drift.
POST-REFUSAL-REASON: insufficient stock has a stable typed refusal naming item,
warehouse and exact requested/available display quantities. Stock fold refusals
retain their typed source. Both are ordinary precondition refusals.
This schema expansion activates no reader or writer. The following P2b cut must
fence the partition (including absence), complete adjacency and all event facts,
validate dense history and call the shared stock fold. P3 still owns output
protection, quantity configuration, accounting admission and atomic publication.

## Authoritative history and planning (P2b)

POST-HISTORY-FENCE: concrete master tuple selects the partition; authorized fact (including ABSENT), complete adjacency and event facts are fenced. Identity,
count and dense ordinals agree. POST-RUNTIME-BINDINGS: master book/graph/version/currency/accounts/UOM are pinned; configuration changes refuse.
CP1 templates are receipt/delivery, layer posted; plans grant no commit authority.
`post_history_fences_and_refusal` checks absent/empty-adjacency races, replay and readable over-delivery refusal without writes.
