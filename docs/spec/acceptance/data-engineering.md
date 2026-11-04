# Demo beat 6: Scrap-yield acceptance

[Owner] Data scientists and engineers are a primary audience. The ontology is the semantic layer; declarations derive pipelines. This required [sixth beat](jewelry-demo.md) follows the [approved Lane D design](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6558867021) and [coordinator ruling](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6558874617). The weekly scrap-yield fixture governs CP2/CP3. Inventory-by-region is post-demo; its original contract is preserved verbatim in the appendix.

All numbers are **synthetic qualification targets**, not reported runtime results. Replay signed ERPNext/Shopify-shaped envelopes through real adapters; label this **synthetic feed replay**, not production CDC. No SQL, authored pipeline DAG, domain kernel opcode or standalone publication authority.

## Pinned scrap-yield fixture

One fictional organization, USD only, UTC, exact fine/gross grams at scale 3 and purity as an exact fraction in `[0,1]`. The transaction demo uses per-mille purity; the source mapping explicitly converts the fraction, never guesses the unit. Keep money/mass accounting exact; statistical calculations are explicitly approximate binary64.

Freeze 16 ISO weeks, July 6–October 25, 2026: four gold melts plus one silver melt per week (80 melts, 64 gold). Each Conversion links input/output/loss lots, accountable agents, source documents/child rows and event/recorded times. Catalog Item/Variant is distinct from Holding/Lot. Declare synthetic ERPNext measurement fields and gross/purity/fine consistency; they are not standard vendor fields.

The showcase week ends October 25 at effective cutoff **T = October 26 00:00 UTC, exclusive**:

| Gold batch | Input fine g | Recovered fine g | Recorded loss fine g |
| --- | --- | --- | --- |
| G1 | 60.000 | 58.800 | 1.200 |
| G2 | 75.000 | 73.500 | 1.500 |
| G3 | 80.000 | 78.400 | 1.600 |
| G4 | 85.000 | 83.300 | 1.700 |
| Total | 300.000 | 294.000 | 6.000 |

The declared question is **Scrap yield by week — gold**: `sum(recovered fine g) / sum(input fine g)`, at Conversion ID grain and ISO-week boundaries, partitioned by metal. The showcase result is **98%**, not an average of batch percentages. [Default] The canonical dimensionless ratio has decimal scale 6 and explicit midpoint-away-from-zero rounding; retain the exact numerator/denominator beside it. Zero input is unavailable, never infinity or an invented zero. Other weeks use the exact mass recipe below, with nonzero predictor variance and nonzero residuals. No favorable RMSE or slope is promised.

### Reproducible forecast population (TARGET fixture, not a runtime result)

[Default] Reuse the mass recipe in [D1a's mapping conformance](https://github.com/Norn-cloud/norn/blob/d77c4f55048b6309b71238cc5704ff038471ead2/crates/norn-checker/tests/demo_measurements.rs), not a second random generator. For week `w=0..15` and batch `b=0..4`, integer milligrams are:

```text
input = [60000,75000,80000,85000,50000][b]                     if w=15
        50000 + 7000*b + (193*w + 71*b) mod 997                otherwise
loss  = floor(input/50)                                      if w=15
        floor(input/50) + (17*w + 13*b) mod 103                otherwise
recovered = input - loss
metal = silver if b=4, gold otherwise
conversion_id = SYN-MELT-{w as two decimal digits}-{b}
```

All gross/fine measurements in this numerical fixture have purity `1.000000`; grams are milligrams divided by 1000 exactly. The bad purity and K2 correction are separate appended variants, excluded from this K1 population. Week Monday is July 6 00:00 UTC plus `7*w` days. Per batch, feature knowledge is Monday 09:00 plus `b` minutes, effective conversion is Friday 12:00 plus `b` minutes, and outcome knowledge is Friday 12:01 plus `b` minutes. Historical receipts in the declared synthetic execution world must authenticate those knowledge times; declarations cannot backdate real knowledge. In particular D1a's bulk mapping test records everything on October 26 and **cannot qualify training knowledge**; H #346 must supply the eligible temporal-export fixture or reject it.

The committed Rust-readable table is [`fixtures/demo/scrap-yield-k1.csv`](../../../fixtures/demo/scrap-yield-k1.csv); `include_str!` can consume it without a generator dependency. Its mass columns are canonical integer-milligram interchange; qualified direct gram columns must retain exact correspondence to the mapped measurements, not become computed columns passed to the temporal admission profile.

Canonical table bytes: UTF-8, LF, final LF, no quoting/spaces; header `week,batch,conversion_id,metal,input_mg,recovered_mg,loss_mg,feature_known_at,effective_at,outcome_known_at`; 80 rows ordered by `(w,b)`; integers use unpadded decimal except the two-digit week in the ID; timestamps use `YYYY-MM-DDTHH:MM:SSZ`. SHA-256: **`22cee20298906d6f1ecb8bd3eabc3d405409d7c3091c275cbfe0a8932694c28d`**. This pins the proposed table values, not authenticated receipt evidence. The qualified forecast identity must additionally bind its actual table/source versions, K1 and receipts; unresolved or later-learned incarnations still refuse. Filter `metal=gold` after validating the full table; training is `w<12`, holdout `w>=12`.

ERPNext owns mass/cost observations; Shopify supplies linked catalog/order/fulfillment evidence, with missing cost/book authority declared. An explicit shipment cross-reference attaches both systems' evidence to one economic event; never double-deplete stock or fuzzy-match identities. Each result binds question/definitions/mappings, authorized population/epoch, world, ledger head, effective/knowledge coordinates and a per-source consumed frontier vector. Source heartbeat, consumed frontier, coverage and completed materialization are distinct.

Knowledge **K1** precedes a correction recorded October 27. At **K2**, accepted linked restatement changes G2 recovered mass **73.500 → 72.000 g** and loss **1.500 → 3.000 g**, input unchanged. The later result is **292.500 / 300.000 = 97.5%**. K1 is immutable. An authenticated bad candidate with purity **1.20** is retained as observation/finding and excluded, rather than becoming a valid holding or refusing capture.

## CP2/CP3 scenarios (given / when / then)

1. **DATA-INGEST:** Given the pinned gold/silver records and Shopify references, when signed capture/mapping commits, then ordinary REA objects retain source documents/cursors and the showcase accepted population has four gold Conversions, 300.000 input g and 294.000 recovered g (98%). Replay, duplicate or reversed arrivals converge without extra rows/value; shared shipment evidence has one consequence. CP2 needs one mapped ERPNext feed; both feeds are required at CP3. Missing/ambiguous identities and mapping coverage remain explicit.
2. **DATA-FRESH:** Given complete accepted contributions and required-source heartbeats/frontiers, when replay advances, then show consumed cursors, coverage, materialization, per-source observed lag and quarantine count with the declared **live ≤5 s** policy. Pause a dependency beyond that bound: show stale/catching-up; exact governed-law use refuses. Resume and catch up. Shopify pause does not stale an ERPNext-only yield question. Fresh heartbeat plus quarantine/gaps is still incomplete; render time is not freshness.
3. **DATA-LINEAGE:** Given 98%, when drilling through, then show numerator 294.000 g and denominator 300.000 g, four Conversion/lot versions, transforms, ERPNext document/child row, observation, mapping version/receipt and relevant Shopify cross-reference. Silver and catalog enrichment add no numeric contribution. Membership pages retain the result pins or restart; masking/revocation applies to every object/document/subscription emission.
4. **DATA-INCREMENTAL:** Given G1–G3 at 210.700/215.000 g, when valid G4 arrives, then measured IVM adds 83.300/85.000 g to reach 294.000/300.000. At every accepted frontier, compare exact in-cell and maintained results with full recomputation from the identical authorized snapshot: numerator/denominator, ratio/rounding, membership, exclusions/findings and temporal pins. Include insert, update, duplicate, reversed order, tombstone and restart/replay; each adds no unintended delta. A batch refresh is not incremental. DuckDB/WellDB OLAP correspondence is a labelled CP3 stretch, not a CP2 prerequisite or a shipped engine claim.
5. **DATA-QUARANTINE:** Given the authenticated purity-1.20 candidate against `[0,1]`, when capture/law evaluation runs, then advance the source cursor and persist raw observation plus `core.boundary.import_finding`, exact offending path/value, law/version, mapping, source document and evidence digest. Quarantine/exclude it; accepted result stays 294.000/300.000 with one exclusion and **“caught up; 1 quarantined; incomplete”**. Replay produces no duplicate active finding. A bad newer version cannot leave an older record labelled fresh/complete. Valid correction appends evidence and resolves under the declared lifecycle. Native invalid writes separately refuse atomically; ingress authentication failures still reject.
6. **DATA-ASOF:** Given T/K1 and the G2 correction learned at K2, when a governed approved restatement activates, then T/K1 remains 294.000/300.000 = 98%; T/K2 is 292.500/300.000 = 97.5%. Display both time coordinates and the correcting source version/reason/approval. Append reconciling movements with counterparts; never overwrite posted history. A ledger cursor alone is not a bitemporal API; an analytical restatement is not automatically reconciled books.
7. **DATA-RESTATEMENT-DELTA:** Given the K1 generation, when the linked correction activates at K2, then recovered numerator retracts 1.500 g, loss adds 1.500 g and input denominator stays 300.000 g. IVM retraction/replacement equals full recomputation at each bitemporal slice, including late correction, quarantine/recovery and restart. Retain old results and source versions. Interrupted rebuild leaves the old complete generation visible with truthful stale status; switch all outputs atomically at the declared frontier.
8. **DATA-FORECAST:** Given the same 64 gold melts, when the pinned direct numeric temporal table is admitted, then train on the first 12 weeks (**48 batches**, before September 28 00:00 UTC) and hold out the last four (**16 batches**). [Default] CP3 uses the immutable **K1** population/table version, with that knowledge coordinate, source versions and table digest bound to the forecast identity; K2 can only produce a new analysis version. Input fine g is known before each melt/prediction; recovered fine g is its later outcome. Freeze the split, code/method/population/parameters/assumptions and receipts before looking at holdout outcomes. [Default] Fit unweighted one-predictor OLS **with an intercept on the 48 training batches only**; compute its residual/QQ/influence diagnostics on that same training population. Score the 16 fixed holdout outcomes without refitting or diagnostic tuning. Show `forecast()` held-out predictions, RMSE in grams and the training-mean baseline without promising a win; show OLS slope in recovered-g/input-g plus residual/QQ/influence plots and diagnostic identities. Independence/confounding remain visible attestations. No money/computed columns are smuggled into a direct numeric-only profile; no causal/future-confidence claim.
9. **DATA-PUBLISH-GUARD:** Given registered method/population/split/assumptions/remedy and a typed Function result/receipt, when an authorized reviewer invokes the declared publication happening, then server checks the exact identities, temporal knowledge, completeness/freshness and diagnostic policy. A failed diagnostic withholds inference or uses a preregistered descriptive-only remedy; a training label learned after the split rejects the analysis population (`core.projection.publication_guard` with temporal detail). Raw imported evidence remains available. Changed/stale/tampered populations or receipts cannot publish under an old identity. Reanalysis is a new pinned version retaining the old report; statistical output cannot post stock/GL.

## Checkpoints and evidence

**CP2, November 2 stop/go:** one mapped feed replay; replay-stable quarantine; one real delta updating an admitted question with exact in-cell/IVM/recompute equality; authenticated temporal export rejecting a late training label. Definition admission or pure statistical arithmetic alone does not satisfy these links.

**CP3, November 3 rehearsal:** both feeds, authorized result-to-document traversal, 98%/97.5% as-of answers, 48/16 forecast/OLS and actual server-governed publication denial in the same app. OLAP correspondence is a separately labelled stretch. Missing links remain explicit; reduced replay/read-only segments are labelled accordingly. November 4 is fixes/polish, not new scope.

Use Core [03](../core/03-query-compute.md), [05](https://github.com/Norn-cloud/norn/blob/6ac89f34c9040241c1ff381d1c9c204df8a4b344/docs/spec/core/05-laws-authority.md), [07](https://github.com/Norn-cloud/norn/blob/7076bfcf8d985a21fe989bfe3434bf1ac4a8b18e/docs/spec/core/07-world-boundary.md) and [08](https://github.com/Norn-cloud/norn/blob/7076bfcf8d985a21fe989bfe3434bf1ac4a8b18e/docs/spec/core/08-worlds-projections.md). The latter three links pin the published specification PRs until their main paths land. The nine identities above remain stable through this fixture change. Every case needs real evaluator/cell/surface correspondence and exact-head repository CI/Boat gates. Separate admission/runtime, bounded correspondence, sampled ERPNext parity and proof evidence; no skip passes and no universal IVM or production-ML claim.

## Native incremental correspondence acceptance

The native acceptance is `crates/norn-cell/tests/scrap_yield_incremental.rs`:
`cargo test --locked -p norn-cell --test scrap_yield_incremental`.
`data_incremental_k1_k2_has_exact_signed_deltas_and_reproducible_ids` checks
G4's 83.300/85.000 g insertion, K1 294.000/300.000 = 0.98, K2
292.500/300.000 = 0.975, recovered delta -1.500 g and loss delta +1.500 g.
Input stays 300.000 g; only the corrected G2 contribution version changes.
K1 is reread at its original knowledge cutoff after K2 and a SQLite reopen.

`generated_changes_match_recompute_through_reorder_duplicate_and_restart`
compares live and knowledge-cutoff IVM with full recomputation at every
receipt, including the complete result envelope and versioned contribution
IDs. Each generated sequence contains insert, update, delete, duplicate and
restart, with possible generated changes of amount, metal and effective week.
Independent identities arrive in forward/reverse batch order; updates of the
same identity retain their order. Different physical ledger hashes are not
claimed equal across these runs. Restart is explicitly a complete rebuild;
receipt advances must report incremental maintenance.

`restatement_activation.rs` checks the same signed correction deltas and
same-slice full recomputation after governed atomic activation. These are
synthetic native-cell acceptance scenarios and sampled correspondence, with
source cursor pins supplied by the harness. Mapped-feed capture, source
authority, training knowledge and the app surface need their own evidence.

## Post-demo fixture

The following inventory-by-region contract is retained **verbatim** from the earlier acceptance baseline. Its “required”/three-engine language applies to post-demo qualification, not CP2/CP3. It adds no current demo commitment and does not replace the scrap-yield cases above.

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
