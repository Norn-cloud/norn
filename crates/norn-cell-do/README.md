# norn-cell-do

Ring 1. One tenant cell per Workers Durable Object, over `norn-api`'s shared
authorized read services and `norn-cell`'s portable store on
`norn_cell::workers::WorkersEngine`. The Durable Object binding is `wasm`
(wasm32 only); the Worker shim and smoke test are in `apps/norn-cell-do`.

- `principal`: `PrincipalResolver` is the one seam from transport credential
  to Norn principal. `TokenPrincipals` verifies the Studio Worker's signed host
  token (#817); `FixturePrincipals` is for tests and local smoke builds only.
- `seed`: provisions the public three-systems demo cell idempotently.
- `base`: the trusted base this host compiles in (`registry/core/artifacts`'
  reserved-namespace artifacts).
- `tenant`: one scope's cell, vacant until `POST /v1/cell:provision`
  (HOST-PROVISION-1) creates it from a tenant's submission, then served and
  reopened on restart; see `apps/norn-cell-do/README.md`.
- `server`: fetch dispatch (reads, ad-hoc questions, the retained definition
  routes for dashboards and agents, written only by admins and owners, and the
  governed action route `POST /v1/actions/{actionId}`); native tests run the
  identical code.
- `actions`: governed action calls decided by the scope's own `policy`
  artifacts through the kernel PDP (HOST-ACTION-2).
- `mcp`: `POST /v1/mcp`, the cell's catalog as MCP tools (object types,
  admitted questions, retained definitions and one tool per action the caller
  may call); each call re-enters `server` as the same caller.
- `questions`: ad-hoc questions over the metrics the active revision derives.
- `olap`: how this host reaches the tenant analytics engine. On the
  Durable Object an engine call ends the request and the Worker performs it
  against `NORN_OLAP_URL`; a native host links the engine in process.

## Binaries

- `norn-dev-host` (native): the host behind `norn dev` (#207). One process
  owns the project's SQLite cell, serves the same dispatch on a loopback
  address, and admits and installs each compiled set; a failed compile,
  admission or install keeps the last good revision serving.
- `norn-olapd` (native, `duckdb` feature): the tenant analytics engine as a
  sidecar next to the cell host. It serves `norn_cell::analytics`' typed
  operations over HTTP to the cells. OLAP-CRED-1 (#1493): each call carries
  the calling cell's own credential, an Ed25519-signed `nolap2.<kid>.<tenant>.
  <expires_ms>.<body sha256>.<sig>` the cell mints for its own tenant only,
  living two minutes (`norn_api::olap_credential`). The signature also covers
  the call's operation and the SHA-256 of its exact body, so a credential is
  good for that one request only (OLAP-03). The same request can still be
  replayed byte for byte until it expires, so the transport between the cell
  host and the daemon must be protected: TLS, or a private network nothing
  else can join. The daemon holds public keys only, in the keyring file
  `NORN_OLAP_KEYS_FILE` (one `<kid>:<base64url public key>` per line), read on
  every call. A cell host and a daemon must run the same credential format:
  deploy them together.

  The daemon authorizes each request completely before it routes or parses it
  (OLAP-04). #1747: the credential is verified from the request's head, for
  the operation the path names and the body digest it carries, before any
  body byte is read; the body then read must have that digest, and its
  `tenant` must be the credential's. Every refusal up to there (missing,
  malformed, unknown or rotated-out key, bad signature, expired, longer-lived
  than ten minutes, minted for another request, another tenant's, a body
  naming no tenant, a method other than `POST`, an unknown path) gets one
  identical `401`, before any tenant database is opened. A `400` or `404`
  needs a credential the cell minted for that exact request. Residual timing
  difference: only a well-formed credential naming a key id in the keyring
  pays the signature check, so response time can tell it from garbage; key
  ids are not secret, and no tenant's existence or data changes the time of
  a refusal. Beyond that the daemon decides no access; the cell authorizes
  every request and checks every answer.

  Connections are read and authenticated in a bounded pre-admission stage
  (64 at once, 16 from one peer address), apart from the 8 engine workers;
  only a fully authenticated, digest-checked request is queued for a worker
  (8 may wait). A full stage or queue is answered `503` at once, so idle,
  slow or forged connections never hold an engine worker.

  Each tenant has its own scratch directory, `<NORN_OLAP_DATA>/tmp/<tenant>/`,
  for its load downloads, its exports and its `DuckDB` spill, and its database
  may read and write only there (OLAP-02). Each load attempt writes a file the
  daemon names (`load-<128 random bits>`), creates exclusively and alone
  removes; no request names a path.

  Other settings: `NORN_OLAP_ADDR` (default `0.0.0.0:8790`), `NORN_OLAP_DATA`
  (default `/data`), `NORN_OLAP_MEMORY_LIMIT` (default `512MB`),
  `NORN_OLAP_THREADS`. `norn-olapd --health` exits 0 when the running daemon
  answers `/healthz`; `norn-olapd --new-key <kid>` prints a fresh signing key
  for the cell host and its keyring line. `apps/norn-olapd/Dockerfile` builds
  an image.

  Cutover from the shared token (OLAP-05): the retired `NORN_SECRET_DUCKDB_TOKEN`
  refuses startup whenever it is set at all, even empty or not UTF-8, so unset
  it rather than blank it. (1) Deploy the daemon with `NORN_OLAP_KEYS_FILE`
  and without the token, and the cell host with `NORN_OLAP_SIGNING_KEY`.
  (2) Retire every replica of the old daemon; none may stay in rotation
  behind the route. (3) Verify through the deployed route that the legacy
  token is rejected: `POST <NORN_OLAP_URL>/v1/frontier` with
  `authorization: Bearer <old token>` must answer `401`
  `analytics.unauthorized`, from every replica. Then delete the old token.

  Rotation, with no downtime. Replace the keyring file atomically at both
  steps that change it (write a copy, then rename it over the original), so
  the daemon never reads a partial file: (1) add the new key's keyring line;
  (2) set the cell host's `NORN_OLAP_SIGNING_KEY` to the new signing key;
  (3) ten minutes later, remove the old line, again by atomic replacement.
  The removal applies to every request that opens the keyring after the
  rename; a request that read the keyring before it completes as it began.

```sh
cargo build --locked --release -p norn-cell-do --features duckdb --bin norn-olapd
```
