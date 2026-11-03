# Three systems, one ontology

`MVP-MAP-THREE`, `MVP-MAP-GAP` and `MVP-ITEM-HOLDING` are exercised by
`cargo test --locked -p norn-checker --test three_systems`. The existing
ontology-pack/link artifacts describe term correspondence; the adjacent
`records.ts` declarations compile to executable MappingIR and SourceSchemaIR.
They are separate evidence levels, not interchangeable claims of law transport.

All records are independently authored synthetic exports. Schema revision
`<system>-three/1`, mapping version `0.1.0`, fixture-byte digest, source-envelope
digest and canonical target dependencies are pinned in the generated artifacts.
There is no live connector or production CDC qualification. Source observations
are proposals, not permission to post stock or ledger changes.

| Meaning | ERPNext fixture | Shopify fixture | Odoo fixture | Canonical target / REA interface |
|---|---|---|---|---|
| Catalog metadata | Item | ProductVariant | product.product | canon.Item / canon.Catalog; never a Resource |
| Owner/location/lot balance | Warehouse Lot Balance | InventoryLevel | stock.quant | canon.Holding / core.Resource |
| Party and role | Party | Customer | res.partner | canon.Party / core.Agent and canon.PartyRole / core.AgentRole |
| Order line | Sales Order Item | OrderLine | sale.order.line | canon.OrderLine / core.Commitment |
| Completed delivery line | Delivery Note Item | FulfillmentLine | stock.move | canon.Delivery / core.EconomicEvent |
| Signed book entry | GL Entry | Not supplied | account.move.line | canon.Posting / core.LedgerEffect |
| Payment/refund report | Payment Entry | Transaction | account.payment | canon.SettlementObservation; cash-event binding remains a gap |

`custom_norn_ref`, `metafield_norn_ref` and `x_norn_ref`, together with the
`*_ref` role fields, are fixture extensions containing explicit, reviewed,
organisation-scoped canonical references. They are not standard vendor fields.
Source `name`/`id` values remain independent aliases in source provenance, with
the system, instance and dataset in the source record reference. No SKU, label,
price, case key or arrival order establishes cross-system identity. Missing,
empty or multiple canonical references refuse mapping with existing
`mapping.runtime.invalid_record` / `mapping.runtime.invalid_key` diagnostics.
This profile uses synthetic ASCII identifiers; it does not qualify opaque ID
normalization or infer identities for real exports.

A catalog Item is shared by two distinct holdings, partitioned by explicit
owner/location references. Owner and custodian have different identities.
Provider, receiver, resource and fulfilment roles survive on one delivery
identity reported by all three systems. Source reports are corroborating
evidence: three reports plus redelivery are not four economic contributions.
The test asserts equal candidate keys; durable promotion, conflict review and
aggregate dedup remain the responsibility of the Observed inbox/cell bridge.

Quantities use grams only; unit code sets refuse unresolved conversions.
ERPNext/Odoo cost observations use the explicit moving-average/USD fixture
profile, an identified stock book and signed inventory/COGS roles. Only posted
rows are representable; draft/unknown posting states refuse. This is neither
FIFO parity nor a general vendor valuation implementation. Custom modules,
other units, account-role interpretations, returns and original-event corrections
are not inferred. Settlement reports retain payer/payee but cannot establish
cash movement, settlement finality or balanced posting without further evidence.

Holding `quantity_basis` preserves the semantic difference: ERPNext/Odoo report
on-hand quantities in this profile; Shopify reports available quantities.
Availability is not an ownership or complete on-hand assertion; reservation and
on-hand reconciliation remain unqualified, and the reports cannot be summed.

Shopify catalog price is deliberately ignored with a reason. Its holdings have
explicit `stock_cost` and `cost_currency` gaps and a null valuation method;
valuation coverage is incomplete. Neither the sales price nor a payment amount
is substituted for stock cost. No Shopify GL entries are emitted.

Each `synthetic.json` declares `synthetic_snapshot_incremental`,
`productionCdc: false`, snapshot watermark 10 and ordered upserts starting at 11.
The first delivery is deliberately redelivered at sequence 11. Sequence 12
updates the same source holding from 200 g to 180 g (ERPNext/Odoo cost
9,000 USD to 8,100 USD); 20 g and the 900 USD signed inventory/COGS pair
remain distinct measures. Replay produces
the same candidate and provenance. Odoo has a snapshot plus delivery, balanced
signed ledger observations and settlement increments. There are no deletes,
production cursor-convergence, transactional-capture or restart guarantees in
this fixture contract. Unsupported real changes require a newly admitted
contract, never a claim that this export is a live CDC feed.

Regenerate artifacts with
`cd tools/norn-ts && NORN_PACKS_UPDATE=1 bun test test/registry-packs.test.ts`.
The Rust conformance test checks the exact source hash, admitted artifact set,
roles, aliases, identity refusals, cost gaps, posting/unit refusals and replay.
