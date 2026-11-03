# Jewelry demonstration acceptance

[Owner] Scope is sell → scrap → melt, with REA core → inventory/accounting/sales → jewelry → app. No refining network, chemistry or custody-depth demo. The original five beats remain; [beat 6: data engineering](data-engineering.md) is equally required for the data-scientist/engineer audience. All values below are synthetic target vectors, not reported test results.

## Six audience beats

1. **Working app:** receive, transfer, sell, return, cancel/correct; stock and books agree under the inventory parity table below.
2. **What built it:** inspect admitted types, links, actions, statecharts, rules/folds and laws through the definition catalog; the same declarations drive UI/API/MCP/CLI, without CRUD/posting hooks.
3. **Jewelry live:** install the jewelry specialisation with purity and a fine-metal ledger; execute sell → scrap exchange → melt with explicit loss.
4. **Laws carry through:** try an omitted-loss transaction and weakening overlay; show readable refusal and unchanged stock/books/outbox.
5. **Evidence:** Guarantees lists inherited origins, execution enforcement, generated bounded/model/proof results and ERPNext sampled parity separately. Edit a declaration to provoke a generated counterexample; never edit a solver model by hand.
6. **Data engineering:** change feeds → incremental question/freshness → source-document lineage → quarantine → bitemporal restatement → guarded statistical publication, exactly as specified in the linked acceptance.

## Inventory/accounting oracle profile

[Default] Pin one ERPNext v16 release/image digest and its configuration in each run: one synthetic company, USD, scale 2, explicit midpoint-away rounding, no taxes for stock vectors, FIFO and moving average. ERPNext is a black-box behavior oracle; no GPL code/schema is copied into Norn. Freeze exchange rates, precision, posting dates and background-job completion before comparison. Differences are named and resolved, never hidden by tolerance or skipped scenarios.

All rows below start from a fresh book unless they explicitly continue a previous row. Quantity has exact scale 3. Receipt R1 is 2 units at 10, R2 is 3 units at 20, ordered by effective time then stable event ID. Receipt GL totals: Dr inventory 80 / Cr received-not-billed 80. Availability starts at 5.

| Scenario | When | FIFO expected | Moving-average expected |
|---|---|---|---|
| INV-CP1 | Deliver 4 after R1/R2 | COGS 60; remaining 1/value 20 | COGS 64; remaining 1/value 16 |
| INV-TRANSFER | Transfer 2 from A to B before delivery | A: 3/value 60; B: 2/value 20 | A: 3/value 48; B: 2/value 32 |
| INV-RETURN | After INV-CP1 return one explicitly selected unit from the R2 allocation to A | Returned cost 20; stock 2/value 40; net COGS 40 | Returned cost 16; stock 2/value 32; net COGS 48 |
| INV-CANCEL | Cancel INV-CP1 delivery before dependents | Stock 5/value 80; COGS 0 | Stock 5/value 80; COGS 0 |
| INV-DEPENDENT | Cancel delivery after the return | Refuse until return compensated; then cancel restores 5/80 | Same dependency rule and final 5/80 |
| INV-RECONCILE | From initial 5/80 explicitly reconcile to 4 units/value 68 | Quantity delta -1, value delta -12, Dr variance 12 / Cr inventory 12 | Same explicit measured target, not an inferred valuation rate |
| INV-PARTIAL | From initial layers deliver 0.5 | COGS 5; remaining 4.5/value 75 | COGS 8; remaining 4.5/value 72 |

Every delivery emits Dr COGS / Cr inventory of its cost. A return reverses those accounts at original allocation cost, not today's rate. Transfer moves value between location partitions and does not create sales/COGS; transfer allocations cannot become sale-return references. Cancellation appends linked inverse entries and retains original events. Reconciliation has an explicit external/variance counterpart.

- **INV-DUPLICATE:** Given a committed delivery, when exact intent repeats, then one receipt/consequence; changed payload under its ID refuses.
- **INV-ORDER:** Given equal timestamps, when arrival order changes, then stable declared tie-break preserves valuation; backdating requires a restatement plan.
- **INV-OVERDRAW:** Given balance 5, when delivering 6 or racing two deliveries of 4, then overdraw refuses; stale membership/absence reads also refuse.
- **INV-REOPEN:** Given each accepted/refused step, when reopening and replaying the durable store, then complete canonical dumps and receipt identities agree with pre-close state.

Required differential output per checkpoint: quantities, availability/holds, valuation layers and allocations, stock value, account debits/credits, lifecycle state, causal reversal links and refusal/no-write behavior. Bucket mismatches as definition, mapping, oracle configuration, ordering/precision, engine defect or unsupported scope. Every nonzero bucket blocks that claimed parity scenario pending an explicit ruling; do not call sampled parity proof of equivalence.

## Jewelry fixture and exact cycle

[Default] Use a separate book from inventory vectors. Currency USD, fine/gross mass scale 3 g, purity scale 0 per mille, money scale 2. Fine mass = gross mass × purity / 1000. Inputs contain 100.000 g of pure metal with value 1000.00; all external source/sink movements are explicit. USD and fine-gram ledgers are distinct; the cross-module law binds company, book, valuation basis and account roles.

| Scenario | Given / when | Required then |
|---|---|---|
| JEWEL-SELL | From 100 g/value 1000 sell 20 g for USD 300 | Stock 80 g/value 800; Dr COGS 200 / Cr stock 200; Dr cash/receivable 300 / Cr sales 300. Link delivery and exchange sides. |
| JEWEL-SCRAP | Buy back 25 gross g at 800‰ for USD 180; two distinct accountable assayers each record 25 g/800‰ | Fine receipt 20 g; stock fine 100 g/value 980; Dr stock 180 / Cr payable 180; received resource AND payable commitment recorded. This is a new exchange, not an original-cost sales return. |
| JEWEL-ASSAY | Repeat assay before acceptance; try duplicate assessor evidence | Distinct evidence IDs/assessors retained; duplicate person does not satisfy two-person policy. After posting, remeasurement requires new observation and reconciling movement, never edit prior purity. |
| JEWEL-FUND | Allocate all 100 fine g/value 980 to melt | Explicit reservation/transfer into melting; no unfunded consumption; concurrent consumption loses by full read-set validation. |
| JEWEL-MELT | Output 99.200 gross/fine g at 1000‰; record 0.800 fine-g loss | 100 = 99.2 + 0.8 fine g; output value 972.16 and loss expense 7.84 at blended 9.80/fine g. Gross input 105 g also records 5.000 g non-fine byproduct plus 0.800 g process loss. |
| JEWEL-APPROVE | Loss ratio 0.8/100 = 0.008 exceeds configured 0.005 | Real APPROVE action/evidence by authorized manager required before posting; changed batch/output invalidates approval. Exactly 0.005 does not trigger the `>` policy. |
| JEWEL-REFUSE | Omit loss, use purity 1100‰, or remove inherited conservation | Refuse admission or live commit at the appropriate layer, with no partial holding/GL/lifecycle/outbox effects. Imported invalid purity instead follows beat-6 quarantine. |

[Default] Approval policy for this synthetic demo requires two assays for every scrap exchange, an intentionally stricter fixture than a threshold-only policy. Assay and approval states/actions must be reachable; a policy naming a nonexistent action fails acceptance. The metal ledger keeps measured loss provenance; balanced accounting alone does not prove fine-metal conservation.

- **JEWEL-PRICE:** The sell and scrap pricing step declares USD/g at scale 6, fine g at scale 3, and USD at scale 2 with default Refuse: 20 g × 15 USD/g = USD 300 and 20 g × 9 USD/g = USD 180. The checked action derives the money value, commits once, and preserves it through retry/reopen; EUR input, an mg denominator, zero mass and a nonzero cent remainder refuse without partial writes. `bash scripts/demo/drop-06-exact-pricing.sh` exercises this step through typed source, native admission and SQLite. Pricing evidence alone does not establish stock valuation, balanced GL, assay or the full sell → scrap → melt cycle.

## Completion and lane integration

[Astra] Lane C #227 supplies core-aligned definitions, then declared receipt/delivery/returns and jewelry. A #323/#324 supplies folds/read fencing and atomic effects; B #316/#322 and B1–B8 supplies readable/inherited laws and generated evidence. Host #270/#308–#312/#317 supplies equivalent governed surfaces. Pending branches do not count as main capability.

Pass when all six beats run without skips on the same binary, all named scenarios have executable correspondence, inherited laws remain visible, generated counterexamples come from edited declarations, and the parity table has no unexplained divergence. Use the [completeness gate](completeness-gate.md) and [MVP](master-ontology-mvp.md); never substitute a hand-coded stock path or handwritten application theorem.

## Independent stock consumer

**INV-ONE-LOT:** the stock-only authored app admits without Conversion or approval definitions. From the only receipt 10@2, delivery3 leaves quantity7/value14, records one original-cost allocation and COGS6, and retains no synthetic second layer. FIFO 2@10+3@20 → delivery4 costs60/residual20; the separate moving-average book costs64/residual16. Overdraw and changed-intent refusals preserve the complete SQLite state; accepted retry and cold reopen retain identity.

**INV-TRACE-FIFO:** the two frozen integer/cent-exact receipt/delivery traces produce seven observed checkpoints, including initial states. Read actual Event roles, linked GL legs, vault membership and complete Holdings; requested/expected values never supply observations. Preserve native keys, full database dumps and ledger heads with exact reopen equality. Changed or unsupported input refuses this observation profile before opening a book. The synthetic one Unit = one gram binding does not qualify rounding, effective-time, general UOM or actual-main ERPNext parity; rate and FIFO-queue fields remain unavailable.
