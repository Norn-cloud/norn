# Drop: three systems, one ontology

Run `bash scripts/demo/drop-three-systems.sh` from clean, fetched main.
Install the frozen `tools/norn-ts` dependencies with Bun's `--backend copyfile`;
Rust 1.98 and Python 3 are required.

The script first regenerates every registry pack from its typed sources and
compares the result with the committed artifacts, so the ERPNext, Shopify and
Odoo source schemas and mappings are exactly what `records.ts` declares. It
then builds one native checker test binary, prints its SHA-256 and runs the
eight named `MVP-MAP-THREE`, `MVP-MAP-GAP` and `MVP-ITEM-HOLDING` scenarios.
Test output is live; no stored output is replayed. The declarations and the
correspondence table are in
[`registry/packs/THREE-SYSTEMS.md`](../../registry/packs/THREE-SYSTEMS.md).

What the scenarios show: the three synthetic exports are admitted as one
artifact set and map onto the shared canonical REA types. Source aliases and
provider, receiver, resource and fulfilment roles survive. One delivery
reported by all three systems, plus a redelivery, yields one candidate
economic identity, not four contributions. A catalog Item is never a Holding;
two holdings of one Item stay partitioned by owner and location. Missing,
empty or multiple canonical references refuse mapping; no SKU, label, price or
arrival order is used to guess a merge. Shopify availability is not on-hand
quantity, and a Shopify price never becomes stock cost or a GL entry: its
holdings report explicit cost gaps. Unresolved units and draft postings
refuse. Odoo's synthetic snapshot plus ordered incremental contract replays to
the same candidates and provenance.

Honest boundary: the mappings produce observation proposals, not posting
authority. Durable promotion, conflict review and aggregate dedup through the
Observed inbox and cell bridge are not exercised here. There is no live
connector, production CDC, cursor convergence or delete handling; all data is
synthetic and organisation-scoped canonical references are fixture
extensions, not vendor fields. No FIFO parity, general vendor valuation,
generated solver counterexample or checked application proof is claimed. The
script prints these as `not yet` lines.

Publish only after these commands pass against a recorded main source.
If a committed wrapper is rehearsed before its PR lands, pin both wrapper
and main source explicitly in the drop note; PR-head engine runs count zero.
