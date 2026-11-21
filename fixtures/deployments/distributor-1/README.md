# distributor-1: a realistic deployment test

A mid-size distributor runs self-hosted ERPNext (books, Cairo/Alexandria
stock), Odoo (Giza/Delta branch), Saleor (B2B web shop, GraphQL and
webhooks), a legacy Postgres warehouse database, and files: MT940 and
camt.053 bank statements, EDI X12 850/856, an xlsx price list with merged
cells, and a fixed-width Windows-1256 Arabic aging file. Its records overlap
and conflict: the same customers, items and stock appear in several systems,
under Arabic and English names, with money as text and times without zones.

This directory is the reusable harness. It deploys those systems in Docker,
seeds them from one synthetic world, extracts through a hostile network,
maps everything into the canon, loads it through `norn dev`, runs laws,
governed actions, questions and an MCP agent, stresses the path with BPI
Challenge 2019, and measures how much of the work came from existing packs.
All data is synthetic; no real company or person appears.

## Layout

| Path | What it is |
|---|---|
| `gen/world.py` | The deterministic world (seeded): customers, items, warehouses, orders, deliveries, invoices, payments, receipts, plus seeded defects. `--scale N` |
| `gen/files.py` | The file sources and `legacy.sql`, rendered from the world |
| `compose.yaml` | ERPNext + MariaDB, Odoo + Postgres, Saleor + Postgres + Redis + worker, the legacy Postgres, Toxiproxy |
| `proxy/hostile.py`, `proxy/chaos.json` | Mutating HTTP proxy (page skips and repeats, 429 storms, dropped fields, schema drift mid-sync, duplicate and out-of-order webhooks) in front of Toxiproxy (latency, resets, timeouts) |
| `proxy/sink.py` | The webhook receiver |
| `seed/seed.py` | Seeds each system through its own API (ERPNext REST, Odoo JSON-RPC, Saleor GraphQL) |
| `extract/extract.py`, `extract/files.py` | Pull through the proxy, `careful` or `naive`, and write recorded exports for `POST /v1/observations:load` |
| `project/src/distributor.ts` | The deployment ontology: what the canon could not express, each type naming its canon gap |
| `project/src/p2p.ts`, `project/src/approvals.ts` | BPIC 2019 types; the credit approval (needs `Norn.approval`, #1469) |
| `project/gen_mappings.py` | Generates every source schema and mapping. Pack track: the ERPNext, Odoo and Shopify (for Saleor) pack maps copied verbatim, only the source block re-bound. New track: one spec entry per dataset; a field that is neither mapped nor ignored with a reason fails generation (MAP-TOTAL) |
| `project/assemble.ts` | Compiles, pins each mapping to its source schema's hash, admits the set with the checker, drops and reports what does not admit |
| `measure/` | `load.py` (paged load, rates, memory), `probe.py` (objects, laws, #1402 holdings, questions, actions, MCP agent), `bpic.py`, `shoot.ts` (Studio), `reuse.py` (the cost metric), `report.py` (the gap report) |
| `run.sh` | Runs the phases in order on one VM |

## Running it

Run it on a disposable VM, never on a shared host. It starts about 12
containers, a release Rust build and a browser. 8 GB of RAM is enough if the
phases run in order; `bpic` stops the systems first.

```sh
git clone https://github.com/Norn-cloud/norn && cd norn
bash fixtures/deployments/distributor-1/run.sh all          # or one phase at a time:
#   setup    docker, image tags, cargo build of norn-dev-host, checker wasm, bun install
#   systems  world + files, compose up, ERPNext setup wizard, Saleor admin, proxy + sink
#   seed     seed the three systems (direct, not through the proxy)
#   extract  careful extract on a clean network, then naive extract with every toxic on
#   norn     generate + assemble mappings, init a three-systems project, norn dev, load, probe, replay
#   chaos    load the naive (hostile-network) extract into its own cell and probe it
#   bpic     download BPIC 2019 through the 4TU/figshare API, convert, load (BPIC_BUDGET_S), probe at scale
#   studio   build Studio, serve it on the loaded cell, screenshots + recording into $RUN/proof
#   pack     tar the results
python3 fixtures/deployments/distributor-1/measure/reuse.py /tmp/reuse.json
python3 fixtures/deployments/distributor-1/measure/report.py "$RUN/results" /tmp/reuse.json > gap-report.md
```

`reuse.py` runs anywhere `bun` does, with no VM. It needs only the TypeScript
compiler.

## What a second deployment reuses

`reuse.py` counts origin four ways: artifacts, object maps, source fields and
authored lines. It then projects deployment #2: every new artifact is marked
either system-generic or deployment-specific. System-generic means any
ERPNext, Odoo, Saleor, bank-file or SAP customer needs it, so it is a pack
candidate. Deployment-specific means it belongs to this distributor: its
credit process and its legacy WMS. The projected reuse is what #2 starts
from once the generic artifacts are promoted into packs.
