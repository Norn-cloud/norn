# Norn

Norn is an embedded Rust core for typed definitions, checked decisions and transactional cells.
The checker issues content-addressed admission receipts; the kernel admits effects;
SQLite cells record ordered commits. TypeScript definitions compile to the same IR.

Start with [the quickstart](QUICK_START.md). Embed `norn-cell` and `norn-kernel` for
execution; use `norn check` for native admission and `tools/norn-ts` for typed authoring.
`packages/norn-sdk` contains generated core wire types. Crate READMEs describe the APIs.

Norn-owned code is Apache-2.0. Third-party licences and notices remain intact.
