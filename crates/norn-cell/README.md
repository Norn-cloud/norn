# norn-cell

`norn-cell` is the implemented ring 1 cell store over host-provided `SqlEngine`.
The default `native` feature selects rusqlite and native export/restore support;
the Rust Durable Object host supplies its own SQL adapter. Hosts own durability.
Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.
