# norn-cell-do

The Workers/Durable Objects host: one Norn tenant cell per Durable Object
(#728, #819). It runs unchanged on workerd and Cloudflare.

- `subscribe.mjs`: live reads (`norn.live.v1`). A hibernating WebSocket on
  `/v1/subscribe` names GET routes; after every commit the cell answers each
  one again as its subscriber and sends only changed answers. The cell's own
  handler and authorization decide every answer; an expired token closes the
  socket (4401) and the client reconnects with a fresh one.
- `wake.mjs`: the object's alarm (TIMER-ALARM-1). A request that may write
  is guarded by a durable wake before it runs; afterwards, and after every
  alarm run, the alarm follows the cell's next deadline; an alarm sets a
  recovery wake first and re-arms before any fetch. `bun test ./wake.test.mjs`.
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
cp apps/norn-cell-do/{worker.mjs,subscribe.mjs,wake.mjs,connect_vars.mjs,telemetry.mjs,otlp.mjs,wrangler.json} "$NORN_CELL_DO_PROJECT/"
cargo build --locked -p norn-cell-do --features fixtures --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm" \
  --target web --out-name norn_cell_do --out-dir "$NORN_CELL_DO_PROJECT/pkg"
python3 apps/norn-cell-do/smoke.py   # on `wrangler dev --local`
```

The smoke build enables `fixtures` (`Bearer fixture:admin@demo`, `guest@demo`);
a deployable build omits it and then requires a verified host token. A
`fixtures` build opens no cell unless `NORN_STAGE` is `smoke` or `test` (the
smoke sets it), and `apps/studio/scripts/celld-deploy-cell.ts` refuses to
deploy one.

Pin: Wrangler 4.157.0, as in `crates/norn-cell/README.md`.

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

With `NORN_OLAP_URL` set, the cells sign their calls to the analytics engine
(`norn-olapd`) with a fourth Worker secret, `NORN_OLAP_SIGNING_KEY`
(`<kid>:<base64url Ed25519 seed>`, from `norn-olapd --new-key <kid>`), set with
`wrangler secret put NORN_OLAP_SIGNING_KEY`. Each call carries a
two-minute credential for the calling cell's own tenant (OLAP-CRED-1, #1493);
there is no shared engine token. Without a valid key the Worker logs
`analytics not enabled` and the cells serve without analytics. Rotation is
described under `norn-olapd` in `crates/norn-cell-do/README.md`.

Deployment-wide secrets stay out of the connection secret namespace
(OLAP-01). A connector's auth resolves `NORN_SECRET_<REF>` by any name it
declares and sends the value to its source, so only per-connection secrets
belong there. The analytics signing key is `NORN_OLAP_SIGNING_KEY`, passed to
the analytics path only, and the object store keys are `NORN_S3_ACCESS_KEY_ID`,
`NORN_S3_SECRET_ACCESS_KEY`, `NORN_S3_DOCUMENTS_ACCESS_KEY_ID` and
`NORN_S3_DOCUMENTS_SECRET_ACCESS_KEY`, read by the upload path only. A Worker
that still has `NORN_SECRET_S3_*`, `NORN_SECRET_OLAP_*` or
`NORN_SECRET_DUCKDB_*` set refuses to configure its connections (it logs
`source connections not configured`) until they are renamed or deleted.

The Worker reaches `norn-olapd` over a protected transport only: TLS, or a
private network that nothing else can join. A credential is bound to one
call's operation and exact body, but whoever sees a request in transit can
replay that same request until the credential expires (at most two minutes).

## Guarantees and their mechanism here

| Guarantee | Native | Durable Object |
|---|---|---|
| Single writer | `Mutex` around one cell | one DO instance per cell; synchronous Wasm turns |
| Atomic commit (TXN-ATOMIC-1) | `BEGIN IMMEDIATE` | `storage.transactionSync` |
| Durability | `synchronous=FULL` fsync | DO storage; output gates hold replies until durable |
| Ledger, receipts, transparency | shared `norn-cell` store | the same store over `WorkersEngine` |

## Host conformance (D122)

`conformance/` checks that a cell host runs this cell the way native Rust
does. The self-hosted host is the native Rivet host (`crates/norn-cell-rivet`,
D131); a deployed Workers host runs through `--host url`.

- `crates/norn-cell-do/tests/conformance_reference.rs` owns the request
  script and serves it natively through the same tenant dispatch as
  `src/wasm.rs`, recording each status, body and verified ledger head. The
  record must be reproducible, including with the cell reopened before every
  step.
- `conformance/worker.mjs` is a test entry around the unchanged `worker.mjs`:
  a fixed request clock, a hold inside a commit's write transaction (right
  after its n-th SQL statement, or at the end of its callback before it
  commits) where the runner kills the host, the verified ledger head (the
  exact string the Rust cell serializes), and `Probe` objects for the Durable
  Object API the cell relies on. It serves nothing unless it is opted in: a
  `fixtures` build AND `NORN_STAGE=test` AND `NORN_CONFORMANCE=synthetic`.
  The checked-in `conformance/wrangler.json` sets neither and has no
  `workers.dev`, preview or route URL, so a plain `wrangler deploy` of it
  serves nothing. A hosted run supplies them in a separate, untracked config (see
  below). Opted in, it serves only the synthetic scope
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
  - kills at **sampled** SQL boundaries inside a commit's transaction (after
    statements 1, 2, 3, n/4, n/2, 3n/4, n−1 and n, and at the end of the
    callback before it commits), not every statement. Each boundary is proven
    reached before the kill: the entry prints the request's random marker to
    the host's log from inside the write transaction, after a statement ran,
    while it spins there (never for a refusal that wrote nothing or a callback
    that threw), and the runner kills only once it reads it. Without a marker
    the runner neither kills nor restarts the host, even when the connection
    was reset: the boundary is **not tested**, and if the cell then stops
    answering the sweep ends there, every remaining boundary **not tested**.
    A kill must leave the commit wholly absent, and a retry must match native;
  - an acknowledged commit surviving a kill. The suite does **not** show that
    the platform withholds replies until a write is durable: no boundary sits
    between a commit and its reply;
  - no interleaving inside a synchronous turn; one live instance per object
    (two runners on Rivet, and a runner stalled or frozen mid-commit while its
    cell moves, each proven held by the same marker);
  - alarms as **platform probes only** (fire, re-fire after a failing handler
    with evidence that a failed delivery preceded it, rolled back with their
    transaction, and a firing recorded after the host restarted from a kill).
    The cell's own timers are not in the suite yet;
  - SQLite affinity, bindings, and a 1 MiB value stored exactly (larger values
    are measured); the clock inside a synchronous call (measured);
  - waitUntil background work completing after the reply, with no traffic
    meanwhile. It passes only with evidence that the work was still pending
    when the reply arrived (the reply counts fewer writes than scheduled and
    came back before the first was due) and that all of it ran afterwards.
    Without that timing evidence the row is not tested; a qualifying reply
    followed by missing writes is a fail (idle eviction is not claimed; whether
    the object was replaced meanwhile is only reported);
  - a live WebSocket across 15 s idle, and, as its own row, whether the object
    hibernated meanwhile (a new instance answered the commit the socket saw);
    when the same instance answered, hibernation is **not tested**;
  - backup and restore in place; then cold start, commit latency, memory per
    cell and 100 cells on one host (measured).

  Durability evidence is process kills (SIGKILL) against **one** Rivet Engine
  with its log and objects on local disk: no
  power loss, torn writes, multi-engine clusters or S3 object storage.

`--host rivet-native` runs the native Rivet host (`crates/norn-cell-rivet`,
D131): Rivet Engine 3.1.1 (`rivetdev/engine`, pinned by digest, in Docker)
with two `norn-cell-rivet` runners, a `conformance` build named by
`NORN_CELL_RIVET`. Host state and logs go to a fresh temporary directory (or
an empty, real `--state` directory). Run it on a Boat VM, never a shared host
(`scripts/ci/core.sh rivet-conformance` runs exactly this):

```sh
P="${CARGO_TARGET_DIR:?}/norn-cell-conformance"; mkdir -p "$P"
NORN_CELL_DO_CONFORMANCE_OUT="$P/reference.json" cargo test --locked -p norn-cell-do --test suites conformance_reference::
(cd crates/norn-cell-rivet && cargo build --locked --release --features conformance)
NORN_CELL_RIVET=crates/norn-cell-rivet/target/release/norn-cell-rivet \
  python3 apps/norn-cell-do/conformance/conformance.py --reference "$P/reference.json" --host rivet-native
```

`python3 -m unittest apps/norn-cell-do/conformance/test_conformance.py`
checks the runner's own rules with a stub host: host state is a fresh, real
directory, a reset without a marker kills nothing, and waitUntil does not
pass when the reply came after the work.

`--host url --url <base>` runs the tests that need no host control (kills,
restarts and backups report **not tested**). Point it only at a fresh,
isolated deployment made for the run, never at a stage that serves anyone:

1. Build the project as above (a `fixtures` build; the production deploy
   script refuses one).
2. Write a separate, untracked opt-in config for a new, throwaway name and
   deploy it; the checked-in `wrangler.json` stays disabled. `NORN_LINEAGE_KEY`
   is the reference's public lineage key (the ledger heads only match native
   with it):

   ```sh
   jq --arg name "norn-conformance-$(openssl rand -hex 4)" --arg key "$(jq -r .lineageKey "$P/reference.json")" \
     '.name = $name | .workers_dev = true | .vars = {NORN_STAGE: "test", NORN_CONFORMANCE: "synthetic", NORN_LINEAGE_KEY: $key}' \
     "$P/wrangler.json" > "$P/wrangler.optin.json"
   (cd "$P" && wrangler deploy --config wrangler.optin.json)
   ```
3. Run `conformance.py --reference "$P/reference.json" --host url --url
   https://<that deployment>` (`https` uses TLS with hostname verification,
   port 443 unless the URL names one). The runner identifies itself as
   `User-Agent: norn-conformance/1` (`workers.dev` refuses urllib's default).
4. Delete the deployment (`wrangler delete --name <that name>`) and
   `$P/wrangler.optin.json` afterwards.

## Telemetry

`telemetry.mjs` exports what the host does over OpenTelemetry (OTLP/HTTP, JSON
encoding) through `otlp.mjs`, a dependency-free exporter Studio's Worker shares.
It is off unless the Worker has `OTEL_EXPORTER_OTLP_ENDPOINT` (OTEL-OFF-1):
without it every wrapped method calls straight through and nothing is sent. Set:

| var | value |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | the collector's base URL; OpenObserve: `https://<host>/api/<org>` |
| `OTEL_EXPORTER_OTLP_HEADERS` | `Authorization=Basic%20<base64 user:password>` (a secret) |
| `OTEL_RESOURCE_ATTRIBUTES` | `deployment.environment=<stage>` |
| `OTEL_TRACES_SAMPLER_ARG` | fraction of spans kept (default 1) |
| `NORN_OTEL_INTERVAL_MS` | export interval (default 10000) |

Misconfiguration turns export off rather than failing a request: an endpoint
that is not an http(s) URL or carries userinfo, a header that is not a valid
HTTP field, or credentials sent over plain HTTP to anything but loopback
(OTEL-TLS-1). Telemetry never delays or replaces a handler's result
(OTEL-NEVER-FAILS-1): the wrappers read only the status and a refusal's body
text the host already holds (`noteRefusal`), and recording and export run in a
background task handed to `waitUntil`. The first observation in a quiet isolate
schedules one flush within the interval (at most 20 s).

Metrics are delta counters and histograms:

| metric | attributes |
|---|---|
| `norn_cell_requests_total`, `norn_cell_request_duration_ms` | route template, method, kind (action, decision, provision, plan, apply, admission, install, sync, load, question, read, subscribe, write, other), outcome, status class, environment |
| `norn_cell_definitions_stage_ms` | stage: provision, admission, plan, apply, install |
| `norn_cell_refusals_total` | refusal code, route, kind |
| `norn_cell_actions_total`, `norn_cell_decisions_total` | outcome: ok, pending (parked for approval), refused, error |
| `norn_cell_sync_runs_total`, `norn_cell_sync_duration_ms` (the sync lag: start to rows committed), `norn_cell_sync_rows_{read,landed,quarantined}_total` | outcome (ok, partial, failed), failure code |
| `norn_cell_alarm_lag_ms`, `norn_cell_alarms_total` | retry; lag is fired minus armed deadline (TIMER-ALARM-1, once `alarm()` exists) |
| `norn_cell_live_publishes_total`, `norn_cell_live_fanout` (sockets per publish), `norn_cell_live_snapshots_total` (changed snapshots sent) | none |

Each request, sync and alarm is also a span (`cell <kind> <route template>`).
Every attribute comes from a fixed table (OTEL-NO-TENANT-1): routes are the
templates in `otlp.mjs` `CELL_ROUTES` (`/v1/objects/{type}/{id}/history`; any
other path is `other`), methods, environments and refusal codes are
allowlisted (anything else is `other`), and nothing from a path segment, the
scope, a principal or a body is kept. The per-commit work counters (#1421,
`counters` feature) stay test-only.

`probe/golden-path.mjs` is the synthetic probe (`probe/dev.sh` runs it against a
fresh `norn dev`). It prints fixed diagnostic codes only, sends tokens only over
HTTPS or to loopback, checks every credential before its first write, and
writes into fixed slots (PROBE-BOUNDED-1). A hosted probe needs a cell dedicated
to it: the probe refuses to write into a cell holding non-probe definitions.
There is no cell deprovision or object delete API, so a hosted probe cell keeps
each run's ledger entries, receipts and parked decision. Dashboards and alerts
live in Norn-cloud/norn-ops `observability/`.
