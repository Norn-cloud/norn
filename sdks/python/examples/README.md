# Explore a cell from Python

`explore_cell.py` is a notebook-style script (`# %%` cells). Against a local
development host it authenticates with the session token `norn dev` writes,
lists the cell's admitted definitions, reads each object type into a pandas
dataframe (following the page cursor to the end), and lists the links touching
one object. `../run-demo.sh` runs it in CI over a two-type project
(`demo-project/order.ts` added to the starter) seeded through the governed
action route.

## What a data person cannot do yet

- **No SQL and no bulk export.** Reads are typed routes only (#733); there is
  no Arrow or Parquet export. A dataframe is built from paged JSON, at most 200
  rows a page, and a sort or filter reads at most 10,000 visible rows.
- **No typed questions on the dev host.** The local development host declares
  no metrics, so `/v1/questions/...` refuses there; the demo does not use them.
- **No lineage without ingested data.** `/v1/objects/{type}/{id}/lineage`
  answers only for objects produced by a mapping from source data; nothing can
  ingest into a dev cell yet (#921), so every object here returns 404 for it.
- **No real-source data.** Sources and sync are not connected to a cell yet.
- **No package index install.** The client is generated from a checkout; it is
  not published.
- **Local only.** A published host's tokens come from its identity service
  and live at most five minutes; this demo uses the dev host's session token.
