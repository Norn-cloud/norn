# ERPNext BI golden definitions (#3301 slice 1)

`ontology.json` is a self-contained BI projection of the checked-in
`registry/packs/erpnext-norn-core` SalesInvoiceItem, SalesInvoice and Customer
contracts. The invoice `items` cardinality, customer role, posting date, document
status and currency registry reference come from that pack. The item projection
adds a required forward `parent_sales_invoice` role; it does not derive minimum
cardinality from the source inverse. Invoice `items` retains its source maximum
cardinality without declaring a conflicting inverse role. It deliberately omits unrelated source members and dependencies.

The small source pack does not yet discover net_amount, conversion_rate, store
or region. This fixture explicitly adds those BI projection fields/relations;
it does not claim that the production pack currently imports them. customer_key
is the projected Customer document identity on an invoice, not its display name.
Conversion rates are supplied source-to-SAR facts, not looked up by this slice.

The goldens check net sales by store/day at invoice-item grain and distinct
customer identities by region/week at invoice grain. They contain no data,
expected evaluated results, running queries or runtime/API scaffolding.

The monetary `net_amount` and `rate` decimal types explicitly declare their
parent invoice `currencySource`. The fixtures do not infer monetary semantics
from descriptions or claim the source importer already emits this association.

## OSI reshape (owner 2026-10-14)

`.definition.json` files are typed MetricFlow-style authoring/checker inputs;
`.metric.json` files are generated OSI 0.1.1 documents with standard DBT and
`x-norn` extensions. Question files now explicitly use objectSet/filter/
searchAround/groupBy/aggregate/window/compare/freshness.

The pinned schema and Apache-2.0 attribution are under `osi/`. Regenerate full
metric documents on Boat with `NORN_UPDATE_METRIC_GOLDENS=1 cargo test --locked
-p norn-checker --test metric_interchange`. The test validates stripped exports
against the upstream schema and checks near-identity import with ontology
binding. No semantic datasets or relationships are independently authored.
