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
`norn-cell-do-grants/1` (viewer: Population; member: + Values; admin and
owner: + Sources). Two Worker settings are pinned configuration, never taken
from a token:

- `NORN_HOST_ISSUER`: the token issuer (Studio's base URL);
- `NORN_HOST_JWKS_URL`: its JWKS document (Studio's `/api/auth/jwks`).

The cell fetches the JWKS when it opens and refetches it every five minutes
whatever tokens arrive, so a key the issuer removes stops verifying within that
time. A token naming an unknown key id also triggers a refetch, then the
request is retried once. Refetch attempts are at most one per minute, and so is a cold open: a failed
open (issuer unreachable, missing lineage key) is cached for a minute and
refused at once with the same error, without a JWKS fetch. If
refetches keep failing, keys older than fifteen minutes are no longer honoured
and every token is refused until a refetch succeeds (the Worker logs the
failure). Set `ALLOWED_ORIGINS` (comma-separated) to let a browser origin read
it.

The analytics cell keys its lineage handles (D53) with a third Worker secret,
`NORN_LINEAGE_KEY`: at least 32 bytes, one per stage, set with
`wrangler secret put NORN_LINEAGE_KEY`. Without it (or with a shorter one)
the cell refuses to open and the Worker logs `cell open refused`; answers and
traces are never served unkeyed.

## Guarantees and their mechanism here

| Guarantee | Native | Durable Object |
|---|---|---|
| Single writer | `Mutex` around one cell | one DO instance per cell; synchronous Wasm turns |
| Atomic commit (TXN-ATOMIC-1) | `BEGIN IMMEDIATE` | `storage.transactionSync` |
| Durability | `synchronous=FULL` fsync | DO storage; output gates hold replies until durable |
| Ledger, receipts, transparency | shared `norn-cell` store | the same store over `WorkersEngine` |
