# Demo beat 6: Data engineering acceptance

[Owner] Data scientists and engineers are a primary audience. Norn's ontology is the semantic layer; declarations derive pipelines. This beat is required alongside the [five original beats](jewelry-demo.md), using one question across exact in-cell, IVM and DuckDB/Iceberg (WellDB) execution. The numbers below are **[Default] synthetic qualification fixtures**, not measured results.

## Pinned fixture

Use a separate synthetic analytical fixture from the jewelry transaction book. Currency USD, money scale 2, fine/gross mass scale 3 g, UTC, no tax/FX. Two regions North/South. All query totals below are at **Holding ID grain**, never Shopify order-line/tag grain. ERPNext owns quantity/cost/location observations; Shopify supplies catalog/order/customer-region observations. Region attribution is an explicit warehouse→region mapping, not inferred from customer address.

| Feed event / source document | Effective time (2026 UTC) | Payload / mapping |
|---|---|---|
| E1 / ERP-R1 | Nov 1 09:00 | H1: synthetic jewelry material, North, 2 g at 10 USD/g, purity 1000‰, value 20. |
| E2 / ERP-R2 | Nov 1 10:00 | H2: same catalog Item, North, 3 g at 20 USD/g, purity 1000‰, value 60. |
| S1 / SHOP-P1 | Nov 1 08:00 | Product/variant aliases to that Item; two tags; no cost authority. |
| S2 / SHOP-O1 | Nov 1 11:00 | Order with two lines, linked customer and Item; no stock consequence until ERP fulfillment evidence. |
| E3 / ERP-D1 | Nov 2 09:00 | Delivery consumes H1's 2 g/value 20 and 2 g/value 40 of H2 (FIFO); residual H2=1 g/value 20. |
| E4 / ERP-BAD | Nov 2 10:00 | Proposed observed Hbad: 1 gross g, purity 1100‰, cost 10; invalid purity. |
| E5 / ERP-C1 | Nov 3 09:00 recorded; effective Nov 1 10:00 | Correction to R2: unit cost was 22, not 20; append correction evidence, do not overwrite E2. |

E1/E2/S1/S2 are captured Nov 1 12:00; E3 Nov 2 09:01; E4 Nov 2 10:01; E5 Nov 3 09:01. Cursor vectors: initial `(ERP=2, Shopify=2)`, delivery `(3,2)`, bad row `(4,2)`, restatement `(5,2)`. Cursor values are synthetic sequence positions within one partition per source; production cursors stay opaque. Source heartbeats are separately recorded; bounded freshness is **60 seconds** from the query's observed clock. Harness supplies fresh heartbeats except in the stale case.

Question **inventory value by region**: eligible Holding residual quantities × recorded valuation, grouped by warehouse region and currency; quarantined observations excluded and counted separately. Its definition declares units, grain, exact rounding, required sources, effective/knowledge times, 60-second freshness, lineage and output sinks. The pipeline DAG is generated, never handwritten by the demo.

## Required named scenarios (given / when / then)

1. **DATA-INGEST:** Given the four initial ERPNext/Shopify feed events, when capture/mapping commits, then REA Item/Holding/Agent/Commitment/event links retain each source document and cursor; totals North=80.00, South=0.00, quantity=5.000 g, two holdings. Shopify's two tags/two order lines do not multiply inventory. Replayed E2/S2 add zero rows/value. Missing Shopify cost is a declared gap, satisfied for valuation by authoritative ERP cost, not invented from price.
2. **DATA-FRESH:** Given `(2,2)` and both heartbeats at 12:00:00, when querying at 12:00:30, then show fresh with both cursors and max observed lag 30 s. At 12:01:01 without a Shopify heartbeat, show stale (61 s) for a label-permitted analytical read; an exact governed-law use refuses. Unknown/gapped coverage displays incomplete, even when the heartbeat is fresh.
3. **DATA-LINEAGE:** Given North=80.00, when drilling into it, then show contributions H1=20 and H2=60, their allocation/valuation transforms, ERP-R1/E1 and ERP-R2/E2 documents, source digests, mappings and cursor frontier. SHOP-P1 is catalog enrichment lineage, not another numeric contribution. Restricted document content is redacted with the restriction visible.
4. **DATA-INCREMENTAL:** Given the initial complete generation, when E3 arrives, then the IVM delta is -60.00 North and -4.000 g; new North=20.00, South=0.00, quantity=1.000 g. Full recompute, exact cell and OLAP at `(3,2)` return the same canonical totals and contribution IDs. Restart/resume and redelivery add no extra delta. Iceberg/export/subscription/view outputs carry identical generation/frontier identity.
5. **DATA-QUARANTINE:** Given E4, when source capture and law evaluation run, then cursor reaches `(4,2)` and raw E4 + `core.boundary.import_finding` linked to the purity law are durable; Hbad is quarantined and excluded. North remains 20.00 with quarantine count 1. The capture is **not refused**. A native action proposing purity 1100‰ separately refuses and writes no canonical business state. Reprocessing E4 creates no duplicate active finding.
6. **DATA-ASOF:** Given E5 and an approved analytical restatement R, when asking effective Nov 1 23:59 known Nov 2 23:59, then North=80.00; same effective time known Nov 3 10:00 under R returns 86.00 (=20+66). Asking effective Nov 2 23:59 with old/new knowledge returns 20.00/22.00 respectively. Restated FIFO COGS is 64.00 (=20+44), not 60.00. Original posted GL remains unchanged until a separate governed adjustment posts; the view is explicitly labelled restated, not reconciled books.
7. **DATA-RESTATEMENT-DELTA:** Given the current `(4,2)` generation, when R activates at `(5,2)`, then current North changes +2.00 to 22.00 and the historical day-one projection changes +6.00 to 86.00; IVM retraction/replacement matches full recompute for each bitemporal slice. Original E2/E3 and old query results remain reproducible. Interrupted rebuild leaves the old complete generation visible and stale; no partial output switch.
8. **DATA-FORECAST:** Given synthetic daily sales values `[10,12,14,16,18]` USD at day indices `[1,2,3,4,5]`, all recorded before their declared training/holdout cutoffs, when a pinned regression Function fits OLS with intercept on days 1–4, then slope=2, intercept=8; day-5 forecast=18, absolute error=0, and day-6 forecast=20. Record input table/version, chronological split, model/code digest, parameters and output receipt. This is a deterministic fixture, not evidence of business forecasting quality; no confidence interval is fabricated from zero residual variance.
9. **DATA-PUBLISH-GUARD:** Given that forecast receipt and declared publication policy (complete non-quarantined inputs, fresh frontier, valid chronological split, holdout absolute error ≤1 USD, authorized reviewer), when publishing, then produce a versioned analytical view with lineage. Change the holdout target to 21: error=3 and publication refuses while retaining the unpublished result. A feature recorded after its prediction cutoff also refuses (`core.projection.publication_guard`/temporal-leakage detail). No statistical result may directly post stock/GL or acquire authority.

## Demo script and evidence

Show ingestion source cursors, the question declaration and live 80→20 update; drill into the 20+60 source contributions on the initial snapshot; show E4's finding/quarantine; compare the two as-of answers 80/86 and 20/22; run and publish the guarded 20 USD day-six forecast. Show false/stale/unknown labels as part of the demo, not hidden setup.

Use [03](../core/03-query-compute.md) for query/grain/engines, [07](../core/07-world-boundary.md) for CDC/findings/Functions, [08](../core/08-worlds-projections.md) for restatement/lineage/publication, and [05](../core/05-laws-authority.md) for authority. OLS execution is a Function or qualified analysis implementation behind the controlled boundary, never a new kernel regression opcode.

Main has metric IR/checking, source mappings, reactive substrate and history. Open #69 introduces `norn-metrics` point-in-time admission; #71 adds cursor-based historical reads; #97/#103 are WellDB prerequisites. None establishes full statistical publication or three-engine execution on the inspected baseline. Track DATA-FORECAST/publication as required additional work, not an existing crate capability.

Pass when all nine cases run on pinned synthetic inputs with exact expected totals, no skips, source/correction lineage, three-engine correspondence, non-refused observation capture and guarded publication. Record separate runtime, sampled differential and statistical evidence. Run the same required CI/Boat gates as the core specs; correctness of this fixture is not a universal IVM proof or a production ML claim.
