# norn-projections

`norn-projections` is an implemented ring 0 crate in the pure, portable core.
It projects admitted IR to SQLite DDL, Cedar schemas, lifecycle diagrams,
lineage views, interop exports, and MCP forms. Projections are one-way.
Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.
