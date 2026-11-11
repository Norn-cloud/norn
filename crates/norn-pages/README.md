# norn-pages

A content-addressed copy-on-write page store under SQLite, so forking a cell
costs metadata instead of a copy (Core 08, "Fork lifecycle and isolation").
Ring one, native only: Durable Object and wasm hosts get copy-on-write from
object-store journal lineage instead of a VFS.

- Pages are stored once under a keyed per-tenant address (keyed BLAKE3 over
  the content, with the key derived from a tenant secret held by the host's
  secret store). Addresses never compare across tenants, so dedupe is within
  one tenant only. Rotating the secret changes every address and restarts
  dedupe for that tenant.
- Each file is a root of a Merkle radix page map. Writing a page path-copies
  the nodes above it; forking a file copies its root.
- Garbage collection marks what live roots reach and sweeps the rest.
- `CowEngine` runs `norn-cell`'s native engine over a page-store file and
  implements `ForkEngine`. Its forks are checked against the copying
  reference engine (laws L1–L4).

The tier is in memory: durability (a pack plus a root log with group commit)
is a later unit. The SQLite FFI lives in `sqlite-plugin`; this crate has no
`unsafe`. Its build runs bindgen, which needs libclang.
