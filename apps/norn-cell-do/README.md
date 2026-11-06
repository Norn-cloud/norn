# norn-cell-do

The Workers/Durable Objects host: one Norn tenant cell per Durable Object
(#728, #819). It runs unchanged on self-hosted celld, workerd and Cloudflare.

- `body.mjs`: reads a POST body under a byte bound, cancelling the stream past it (413 `request.too_large`); `node --test apps/norn-cell-do/body.test.mjs`.
- `worker.mjs`: the router picks the cell's object by its `CellScope` key; the
  `Cell` object holds one Rust `NornCell` over its own DO SQLite storage.
- The Rust side is `crates/norn-cell-do`: provisioning, principal resolution,
  authorization and every route live there and run natively in its tests.

Build into a directory outside the checkout, then smoke it:

```sh
export NORN_CELL_DO_PROJECT="${CARGO_TARGET_DIR:?}/norn-cell-do"
mkdir -p "$NORN_CELL_DO_PROJECT"
cp apps/norn-cell-do/{worker.mjs,body.mjs,wrangler.json} "$NORN_CELL_DO_PROJECT/"
cargo build --locked -p norn-cell-do --features fixtures --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm" \
  --target web --out-name norn_cell_do --out-dir "$NORN_CELL_DO_PROJECT/pkg"
python3 apps/norn-cell-do/smoke.py celld   # or: wrangler
```

The smoke build enables `fixtures` (`Bearer fixture:viewer@demo`, `guest@demo`);
a deployable build omits it and then requires a verified host token.

Pins: celld v0.6.3 and Wrangler 4.157.0, as in `crates/norn-cell/README.md`.

## Callers

A deployed cell accepts only the Studio Worker's signed host token (#817):
`Authorization: Bearer <EdDSA JWT>`, audience `norn-host`, at most five
minutes, its `norn.org` equal to the cell's organization, its role mapped
through `studio-roles/1` and then the declared grant table
`norn-cell-do-grants/2` (viewer: Population; member: + Values; admin and
owner: + Sources, and they alone write retained definitions: dashboards and
agents through `/v1/definitions:admit`, `:install` and `/{id}:retire`).
Two Worker settings are pinned configuration, never taken
from a token:

- `NORN_HOST_ISSUER`: the token issuer (Studio's base URL);
- `NORN_HOST_JWKS_URL`: its JWKS document (Studio's `/api/auth/jwks`).

A secret, one per stage and never shared between stages:

- `NORN_LINEAGE_KEY`: base64url, at least 32 bytes. It keys the public
  `lineageRef` handles (D53) together with each cell's scope. Without it the
  analytics cell refuses every answer and trace.

The cell fetches the JWKS when it opens; a token naming an unknown key id
triggers at most one refetch per minute, then the request is retried once.
Set `ALLOWED_ORIGINS` (comma-separated) to let a browser origin read it.

## Guarantees and their mechanism here

| Guarantee | Native | Durable Object |
|---|---|---|
| Single writer | `Mutex` around one cell | one DO instance per cell; synchronous Wasm turns |
| Atomic commit (TXN-ATOMIC-1) | `BEGIN IMMEDIATE` | `storage.transactionSync` |
| Durability | `synchronous=FULL` fsync | DO storage; output gates hold replies until durable |
| Ledger, receipts, transparency | shared `norn-cell` store | the same store over `WorkersEngine` |
