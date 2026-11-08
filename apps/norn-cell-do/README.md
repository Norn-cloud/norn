# norn-cell-do

The Workers/Durable Objects host: one Norn tenant cell per Durable Object
(#728, #819). It runs unchanged on self-hosted celld, workerd and Cloudflare.

- `worker.mjs`: the router picks the cell's object by its `CellScope` key; the
  `Cell` object holds one Rust `NornCell` over its own DO SQLite storage.
- The Rust side is `crates/norn-cell-do`: provisioning, principal resolution,
  authorization and every route live there and run natively in its tests.

Build into a directory outside the checkout, then smoke it:

```sh
export NORN_CELL_DO_PROJECT="${CARGO_TARGET_DIR:?}/norn-cell-do"
mkdir -p "$NORN_CELL_DO_PROJECT"
cp apps/norn-cell-do/{worker.mjs,wrangler.json} "$NORN_CELL_DO_PROJECT/"
cargo build --locked -p norn-cell-do --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm" \
  --target web --out-name norn_cell_do --out-dir "$NORN_CELL_DO_PROJECT/pkg"
python3 apps/norn-cell-do/smoke.py celld   # or: wrangler
```

Pins: celld v0.6.3 and Wrangler 4.157.0, as in `crates/norn-cell/README.md`.
Set `ALLOWED_ORIGINS` (comma-separated) to let a browser origin read it.

## Guarantees and their mechanism here

| Guarantee | Native | Durable Object |
|---|---|---|
| Single writer | `Mutex` around one cell | one DO instance per cell; synchronous Wasm turns |
| Atomic commit (TXN-ATOMIC-1) | `BEGIN IMMEDIATE` | `storage.transactionSync` |
| Durability | `synchronous=FULL` fsync | DO storage; output gates hold replies until durable |
| Ledger, receipts, transparency | shared `norn-cell` store | the same store over `WorkersEngine` |
