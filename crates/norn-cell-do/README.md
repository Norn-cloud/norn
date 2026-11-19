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
  operations over HTTP to one caller, the cell, which authenticates with a
  shared bearer token (`NORN_SECRET_DUCKDB_TOKEN`, at least 32 bytes). It
  decides no access; the cell authorizes every request and checks every
  answer. Other settings: `NORN_OLAP_ADDR` (default `0.0.0.0:8790`),
  `NORN_OLAP_DATA` (default `/data`), `NORN_OLAP_MEMORY_LIMIT` (default
  `512MB`), `NORN_OLAP_THREADS`. `norn-olapd --health` exits 0 when the running
  daemon answers `/healthz`. `apps/norn-olapd/Dockerfile` builds an image:

```sh
cargo build --locked --release -p norn-cell-do --features duckdb --bin norn-olapd
```
