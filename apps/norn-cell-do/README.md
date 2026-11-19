# norn-cell-do

The Workers/Durable Objects host: one Norn tenant cell per Durable Object
(#728, #819). It runs unchanged on self-hosted celld, workerd and Cloudflare.

- `subscribe.mjs`: live reads (`norn.live.v1`). A hibernating WebSocket on
  `/v1/subscribe` names GET routes; after every commit the cell answers each
  one again as its subscriber and sends only changed answers. The cell's own
  handler and authorization decide every answer; an expired token closes the
  socket (4401) and the client reconnects with a fresh one.
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
cp apps/norn-cell-do/{worker.mjs,subscribe.mjs,wrangler.json} "$NORN_CELL_DO_PROJECT/"
cargo build --locked -p norn-cell-do --features fixtures --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm" \
  --target web --out-name norn_cell_do --out-dir "$NORN_CELL_DO_PROJECT/pkg"
python3 apps/norn-cell-do/smoke.py celld   # or: wrangler
```

The smoke build enables `fixtures` (`Bearer fixture:admin@demo`, `guest@demo`);
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

## Host conformance (D122)

`conformance/` checks that a Workers host runs this cell the way native Rust
does. Customers' cells run on OpenOrange (each Durable Object a Rivet actor on
a Rivet Engine); celld and Wrangler are the other hosts.

- `crates/norn-cell-do/tests/conformance_reference.rs` owns the request
  script and serves it natively through the same tenant dispatch as
  `src/wasm.rs`, recording each status, body and verified ledger head. The
  record must be reproducible, including with the cell reopened before every
  step.
- `conformance/worker.mjs` is a test entry around the unchanged `worker.mjs`:
  a fixed request clock, a hold at a storage boundary (the n-th SQL statement
  of a commit, the end of its synchronous transaction, or the next timer turn)
  where the runner kills the host, the verified ledger head (the exact string
  the Rust cell serializes), and `Probe` objects for the Durable Object API the
  cell relies on. It serves nothing unless it is opted in: a `fixtures` build
  AND `NORN_STAGE=test` AND `NORN_CONFORMANCE=synthetic` (its
  `wrangler.json` sets both). Opted in, it serves only the synthetic scope
  `org_demo/analytics` and its probe objects, and refuses everything else.
  The production deploy (`apps/studio/scripts/celld-deploy-cell.ts`) writes its
  own configuration with neither variable, so this entry refuses every request
  there (`apps/studio/test/norn-cell-conformance-entry.test.ts`).
- `conformance/conformance.py` runs the suite. Every row is **pass**,
  **fail**, **not tested** (the run could not show it) or **measured** (an
  observation, not a claim):
  - the 12-step script compared with native: statuses, raw body bytes and the
    Rust-serialized ledger head after each step, byte for byte;
  - transactionSync rollback and nesting;
  - kills at **sampled** SQL boundaries of a commit (statements 1, 2, 3, n/4,
    n/2, 3n/4, n−1 and n, the end of the transaction and the next turn), not
    every statement. Each boundary is proven reached before the kill: the
    entry prints the request's random marker to the host's log while it spins
    there, and the runner kills only once it reads it; a boundary never shown
    reached makes the row **not tested**. A kill must leave the commit wholly
    absent or wholly present, and a retry must match native;
  - an acknowledged commit surviving a kill. The suite does **not** show that
    the platform withholds replies until a write is durable: the post-turn
    hold delays the reply in the entry itself;
  - no interleaving inside a synchronous turn; one live instance per object
    (two runners on Rivet, and a runner stalled or frozen mid-commit while its
    cell moves, each proven held by the same marker);
  - alarms as **platform probes only** (fire, re-fire after a failing handler
    with evidence that a failed delivery preceded it, rolled back with their
    transaction, across a kill). The cell's own timers are not in the suite
    yet;
  - SQLite affinity, bindings, and a 1 MiB value stored exactly (larger values
    are measured); the clock inside a synchronous call (measured);
  - waitUntil past idle eviction;
  - a live WebSocket across 15 s idle, and, as its own row, whether the object
    hibernated meanwhile (a new instance answered the commit the socket saw);
    when the same instance answered, hibernation is **not tested**;
  - backup and restore in place; then cold start, commit latency, memory per
    cell and 100 cells on one host (measured).

  Durability evidence is process kills (SIGKILL) against **one** Rivet Engine
  with its log and objects on local disk, or one `celld dev` process: no
  power loss, torn writes, multi-engine clusters or S3 object storage.

Build the project outside the checkout, then run it on a host:

```sh
P="${CARGO_TARGET_DIR:?}/norn-cell-conformance"; mkdir -p "$P"
cargo build --locked -p norn-cell-do --features fixtures --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm" --target web --out-name norn_cell_do --out-dir "$P/pkg"
cargo rustc --locked -p norn-cell --no-default-features --example workers_sql --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/examples/workers_sql.wasm" --target web --out-name workers_sql --out-dir "$P/probe-pkg"
cp apps/norn-cell-do/{worker.mjs,subscribe.mjs} "$P/"
cp apps/norn-cell-do/conformance/worker.mjs "$P/conformance.mjs"
cp apps/norn-cell-do/conformance/wrangler.json "$P/"
NORN_CELL_DO_CONFORMANCE_OUT="$P/reference.json" cargo test --locked -p norn-cell-do --test conformance_reference
(cd apps/norn-cell-do/conformance && bun install --frozen-lockfile)   # open-orange 1.0.3, test-only; runs on Node 22+
python3 apps/norn-cell-do/conformance/conformance.py --reference "$P/reference.json" --host rivet --project "$P"
```

`--host celld` runs `celld dev --no-watch --logs` (0.6.3). Host state and logs go beside the project (`--state`), never inside it. `--host rivet` runs the Rivet Engine
(`rivetdev/engine` 3.1.2-rc.1, pinned by digest, in Docker) with two
`orange serve` runners from the locked install (`conformance/package.json`
and `bun.lock`, with integrity hashes); `ORANGE` may name another binary, and
the runner refuses any that is not open-orange 1.0.3. Run it on a Boat VM,
never a shared host.

`--host url --url <base>` runs the tests that need no host control (kills,
restarts and backups report **not tested**). Point it only at a fresh,
isolated deployment made for the run, never at a stage that serves anyone:

1. Build the project as above (a `fixtures` build; the production deploy
   script refuses one).
2. Deploy `$P` with its own `wrangler.json` (main `conformance.mjs`, the
   `CELLS` and `PROBES` bindings, `NORN_STAGE=test`,
   `NORN_CONFORMANCE=synthetic`) under a new, throwaway name, and set
   `NORN_LINEAGE_KEY` to the reference's public lineage key
   (`jq -r .lineageKey "$P/reference.json"`); the ledger heads only match
   native with that key.
3. Run `conformance.py --reference "$P/reference.json" --host url --url
   https://<that deployment>` (`https` uses TLS with hostname verification,
   port 443 unless the URL names one).
4. Delete the deployment afterwards.
