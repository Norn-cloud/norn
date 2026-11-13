# norn-cell-do

The Workers/Durable Objects host: one Norn tenant cell per Durable Object
(#728, #819). It runs unchanged on self-hosted celld, workerd and Cloudflare.

- `worker.mjs`: the router picks the cell's object by its `CellScope` key
  (each scope in a declared environment, and each seeded demo scope, has its
  own object); the `Cell` object holds one
  Rust `NornCell` over its own DO SQLite storage.
- The Rust side is `crates/norn-cell-do`: provisioning, principal resolution,
  authorization and every route live there and run natively in its tests.

## Tenants

A scope has no cell until its organization's owner provisions it
(HOST-PROVISION-1, `norn_api::provision`; the contract is host-neutral, this
host mounts it):

```
POST /v1/cell:provision?orgId=<org>&environmentId=<environment>
{ "baseSha256": "<the host's trusted base digest>", "artifacts": [ <artifact envelope>, ... ] }
```

- **Who:** a verified host token whose organization is the scope's and whose
  role is a provisioner in the declared grant table (`norn-cell-do-grants/3`:
  `owner`). A cell that does not exist has no definitions, so this one
  authority is the host's; everything after it is the cell's own policy.
  Anyone else gets the opaque 403, and nothing is written.
- **Where:** only in a declared environment. `NORN_TENANT_ENVIRONMENTS`
  (Worker configuration, comma-separated, default `dev,prod`) names them; a
  scope naming any other environment never reaches a cell of its own (the demo
  cell's opaque denial answers it). An organization has at most one cell per
  declared environment.
- **What is submitted:** the tenant's own artifacts, at most 1 MiB. None may be
  in a reserved namespace (`core.`, `std.`), and none may be scoped outside
  the cell (another environment or organization): the host prepends the trusted base
  it was built with (`registry/core/artifacts`) and admits the tenant's
  artifacts as candidates against it with the shared checker. `baseSha256`
  names the base the set was authored against; another digest is refused with
  `cell.base_mismatch`, which states the host's.
- **What is written, atomically** (HOST-PROVISION-ATOMIC): the store schema,
  the scope binding, a fresh cell identity, the definitions as version 1, their
  admission receipt and the ledger commit, in one storage transaction. A
  failure or a crash leaves the scope with no cell.
- **Retry:** the same submission replays (`"outcome": "replayed"`) without
  writing, also after a restart; a different one is refused with
  `cell.provision_conflict`. Definitions change afterwards through the
  definition routes.
- **A scope with no cell** answers a caller nobody verified exactly as a
  provisioned cell would (the opaque denials), so its existence is not
  disclosed. A verified member of its organization gets 404
  `cell.not_provisioned`.

The isolation boundary: a host token names an organization and a role, not
an environment. Organizations are isolated by the token (another
organization's principal is nobody here). The environments of one organization
are separate cells (own storage, identity and definitions) reached by routing
and checked by scope admission, and a principal holds its role in all of them:
there are no per-environment roles. A tenant that wants its environments to
differ says so in its own policy.

Refusal codes this adds: `cell.not_provisioned` (404), `cell.provision_conflict`
(409), `cell.base_mismatch` (409), `cell.reserved_artifact` (400),
`cell.provision_refused` (422, with the checker's findings, or
`cell.scope_conflict` findings at each artifact scoped outside the cell).

Known limits:
- Any caller can make the router instantiate the object of a scope in a
  declared environment. A vacant object writes nothing and holds no store;
  opening it costs one JWKS document, and cold opens in one isolate share a
  document for a minute. Rate limiting them belongs at the edge.
- An organization's owner can create one stored cell per declared environment
  (up to 1 MiB of admitted definitions plus data); stored cells are bounded by
  organizations × declared environments. If every sign-up creates an
  organization, every user is an owner: invite-only sign-up bounds that.

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
`norn-cell-do-grants/3` (viewer: Population; member: + Values; admin and
owner: + Sources, and they alone write retained definitions: dashboards and
agents through `/v1/definitions:admit`, `:install` and `/{id}:retire`; an
owner alone provisions the cell).
Two Worker settings are pinned configuration, never taken
from a token:

- `NORN_HOST_ISSUER`: the token issuer (Studio's base URL);
- `NORN_HOST_JWKS_URL`: its JWKS document (Studio's `/api/auth/jwks`).

The cell takes the JWKS when it opens (a document this isolate fetched within
the last minute is reused, with its real fetch time) and refetches it every five minutes
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
