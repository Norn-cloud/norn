# Master ontology MVP acceptance

[Owner] MVP is the REA core plus inventory, accounting, sales and shallow jewelry, with ERPNext, Shopify and Odoo system packs mapped in. This is a bounded semantic subset, not import of every vendor table. [Default] All fixtures are synthetic; vendor behavior is observed through black-box contracts.

## Required concepts, behavior and laws

| Layer | Required subset | Required contracts |
|---|---|---|
| REA core | Agent, Resource, EconomicEvent, Commitment, Claim, Hold, LedgerEffect; Process, Conversion, Exchange | Quantity/value conservation by dimension/currency; holds ≤ funded balance; immutable posted events; terminal commitments; owner distinct from custodian. |
| Inventory | Item metadata, Holding/lot, location partition, Receipt, Delivery, Transfer, Return, Reconciliation | Explicit external/variance counterparts; FIFO/moving-average folds; original-cost return links; no negative availability. |
| Accounting | Book, Account, currency-bound ledger and posting effects | Per-currency debit=credit; typed account roles; stock/subledger equality with explicit valuation/opening bindings. |
| Sales | Customer/party role, order/line commitments, holds, delivery, invoice/receivable/payment linkage | Fulfillment/cancellation/breach terminality; independent resource and settlement sides; no synthetic revenue from transfer. |
| Jewelry | Piece/Scrap/Metal classification, assay observation, scrap exchange, melt conversion, fine-metal ledger | Purity bounds; measured input=output+loss; repeated assay evidence; explicit approval; append-only reconciliation. |

[Owner] These are authored Norn definitions/laws/rules, not trusted kernel business code. [Default] Additional ontologies may map through the same generic interpretation mechanism; materials/chemistry, deep custody, global tax catalogs and refining networks are outside this MVP.

## System-pack mapping acceptance

Each pack pins its source ontology/schema revision, connector capability, mappings, role/identity/unit interpretations and declared gaps. Source-record transform, term correspondence and law interpretation are labelled separately. Source-system fields are not automatically native mutation authority.

| System | Minimal mapping | Required gap/qualification boundary |
|---|---|---|
| ERPNext | Item → Item; warehouse/lot balances → Holdings; stock movement/voucher → EconomicEvent + intent; GL entries → LedgerEffect; parties → Agent; orders → Commitment | Item never substitutes for Holding; valuation/account-role precision pinned; live black-box inventory/accounting parity only for named demo profile. |
| Shopify | Product/variant → Item; location/inventory observations → scoped Holdings; order/line → sales commitments; fulfillment → delivery observation; refund/payment → settlement observation | Do not infer stock valuation or GL books from prices; missing cost/book data is a declared gap. Join canonical identities through explicit aliases/mapping evidence. |
| Odoo | Product → Item; lot/location/stock move → Holding/event; partner → Agent; sale lines → Commitment; accounting moves/lines → ledger effects | Unit conversions, valuation method, account roles and posting state must resolve; unsupported custom fields/modules remain gaps. |

ERPNext/Shopify change feeds are live demo priority under [data engineering](data-engineering.md). Odoo MVP requires a synthetic snapshot+incremental contract fixture and mapping conformance, not an unqualified claim of production CDC support. System packs expose capability/coverage labels so a declared mapping never looks like a live connector.

## Named scenarios

- **MVP-CORE:** Given core laws plus a concrete inventory implementor, when admitted, then required members and effective laws resolve; a missing quantity member refuses even with no live rows.
- **MVP-LAYERS:** Given core → inventory/accounting/sales → jewelry → app, when composed, then retain law IDs/origins and typed instance bindings; weakening/domain narrowing refuses.
- **MVP-ITEM-HOLDING:** Given one Item and holdings in two locations, when mapping system rows, then keep one catalog identity and two holding identities with owner/location partitions.
- **MVP-MAP-THREE:** Given synthetic ERPNext/Shopify/Odoo documents with equivalent intended entities, when mapped, then preserve source aliases, role links and declared semantic differences; ambiguous identity is a finding or mapping refusal, never a guessed merge.
- **MVP-MAP-GAP:** Given a Shopify price with no cost/book information, when asked for canonical valuation, then report incomplete coverage; do not label sales price as stock cost.
- **MVP-CONSTITUTION:** Given an invalid core or jewelry law writer, when generated obligations and live commits run, then produce the named counterexample/refusal; no handwritten theorem substitutes.

The bounded constitution driver is `cargo nextest run --profile drop-constitution` on
clean main. Its typed `core.Conversion` specialisation records input, output and
loss in pinned UCUM grams at scale zero. The approved LawIR stays unchanged while
the compiler removes the writer's loss derivation, then restores it.

- **MVP-CONSTITUTION-PASS:** From an empty SQLite cell, execute one create-only
  action for every input 2–12 g and output 1–input g: all 77 cases commit, including
  66 reachable positive-loss cases. Seed zero denotes deterministic enumeration.
- **MVP-CONSTITUTION-LOSS-REMOVED:** Recompile the same writer with the loss
  derivation removed. Its declared zero default exposes 66 conservation refusals;
  generate the first witness (2 g in, 1 g out, missing 1 g loss), and compare the
  entire SQLite dump before/after each refusal, including the action audit table.
- **MVP-CONSTITUTION-RESTORED:** Restore the declaration and reproduce all 77
  successes and the positive-loss witness. A previous result used with the mutated
  Challenge is PROOF-STALE/unknown.

Results consume existing checker-generated initialization, preservation and
non-vacuity goals and the checker's evidence identity binding. Identity matching
grants no verdict.
The executable driver supplies bounded runtime conformance, not solver proof:
generated action-to-backend relations, induction over arbitrary states/writers,
inherited-law instantiation and full REA stock/value effects remain unknown.
All generated proof goals remain open; linked observations are bounded runtime
evidence only. This qualified subset does not by itself complete
MVP-CONSTITUTION or thesis T4-C2.
- **MVP-ROUNDTRIP:** Given one admitted action, when invoked through UI/API/MCP/CLI, then equivalent authorization, exact inputs, consequences and refusals hold; discovery catalog matches the admitted definitions.
- **MVP-DATA:** Given the ERPNext/Shopify feed fixture, when analytical outputs are produced, then freshness/lineage/quarantine/as-of/statistical guards pass all beat-6 cases.

## What exists and what done means

At the [core baseline](../core/README.md), `registry/core/src/{physics,commerce}.ts` and generated interfaces/value types exist; `registry/packs/{canon,erpnext,shopify,odoo}/` contains declared pack/link artifacts. This does not prove retained law transport, live connectors, core-law runtime enforcement or application parity. #227 is open inventory alignment; #69/#71/#97/#103 are open analytical prerequisites.

Done requires every row above admitted with named laws and complete writer closure; every mapping required-member/unit/role case tested; all [core completeness](completeness-gate.md) and [six demo beats](jewelry-demo.md) passing without skips; explicit declared gaps; exact release/parameter/evidence pins; no new app opcode; and the repo's required CI/review gates on the accepted heads. Runtime/model/proof/parity statuses stay independent. Unknown unsupported safety, unresolved mandatory mappings or incomplete authoritative reads block the corresponding capability.

Delivery order: core definitions/readable physics → inventory/accounting folds and transactions → sales exchange/settlement → jewelry → system mappings/observed-data qualification → same-binary acceptance. Data-engineering contracts and ERPNext/Shopify feed fixtures proceed alongside inventory, not after demo polish. Each implementation cut is one coherent unit (no line cap) and remains fail-closed until its dependent semantics land.
