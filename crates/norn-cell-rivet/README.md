# norn-cell-rivet

Ring 1. One tenant cell per Rivet actor (D131, #1130): the third host behind
the `SqlEngine` + `CellHost` seam, next to the native host and the Durable
Object host (`norn-cell-do`). It uses Rivet's native Rust SDK, RivetKit 3.1.1
with `sqlite-local`. The cell's SQLite runs in this process on Rivet's VFS. A
commit returns once it is fsynced on a majority of the control plane's
instances.

The request layer is `norn-cell-do`'s `Tenant` and `server` dispatch,
unchanged, so this host serves the same routes as the Durable Object host. Its
answers are byte-identical to native Rust: the host conformance suite's
differential row checks statuses, bodies and the verified head.

## Modules

| Module | Role |
|---|---|
| `engine` | `RivetEngine: SqlEngine`. Each statement blocks on the actor's `SqliteDb` from the cell thread. A transaction is one RivetKit manual transaction. Norn refuses nesting and SQL transaction control before the platform sees them. |
| `thread` | The actor's cell thread. Every cell call runs there in order, so it is the cell's writer lock: RivetKit runs actions and HTTP callbacks concurrently. A panic is contained: the transaction rolls back and the cell reopens from storage. |
| `core` | One opened cell: the native counterpart of `wasm.rs`'s `NornCell`. |
| `actor` | The cell actor: `worker.mjs`'s `Cell` and `wake.mjs`. It writes nothing in `create` or `on_start`, because RivetKit holds startup writes in memory until startup completes. Provisioning is therefore acknowledged only from a request. Writer admission, a write's deadline update and the wake's final deadline decision share one lock, so a wake never erases a newer writer's recovery wake. Every start reads the cell's storage version first, and a cell this engine refuses fails the start: RivetKit deletes each due schedule row before its handler runs, at the end of a start that succeeds and whenever a trigger comes due, so only an actor that never starts writes nothing to a refused cell. The engine retries the start; a release that reads the cell serves it. |
| `alarm` | The durable alarm. It is a row in the actor's own database, so it commits or rolls back with its transaction. A RivetKit schedule event is only its trigger, and every start re-arms that trigger from the row. Delivery is at least once, with retry and backoff. |
| `live` | Live subscriptions (`subscribe.mjs`, `norn.live.v1`). Sockets do not hibernate. |
| `router` | The public router (the Worker's default export). Requests reach the actor through the engine's gateway, so any runner routes to any cell. |
| `conformance` (feature) | The host conformance seams and the probe actor. They serve only when `NORN_STAGE=test` and `NORN_CONFORMANCE=synthetic`, and only for the synthetic scope. |

## Building

This crate is its own Cargo workspace, excluded from the root. Rivet's
`sqlite-embedded` links `libsqlite3-sys` 0.30, and `norn-cell`'s native engine
links 0.39. Cargo allows one `links = "sqlite3"` per lockfile, so `norn-cell`,
`norn-api` and `norn-cell-do` build here without native SQLite, as on the
Durable Object host. Enabling native SQLite here fails to resolve.
`.cargo/config.toml` sets the SQLite compile options Rivet's VFS requires.

```sh
cd crates/norn-cell-rivet
cargo build --locked --release                         # the runner
cargo build --locked --release --features conformance  # the test build the suite runs
```

## Running

A runner joins the engine named by `RIVET_ENDPOINT`
(`https://default:<admin token>@<engine>:6420`; it refuses plaintext `http`
to anything but loopback unless `NORN_RIVET_PLAINTEXT_ENGINE=single-host`
says the engine is on the same machine's bridge network) and serves the router on
`NORN_LISTEN` (default `127.0.0.1:8787`). The other variables are the Durable
Object Worker's:

- `NORN_HOST_ISSUER`, `NORN_HOST_JWKS_URL`
- `NORN_LINEAGE_KEY`
- `NORN_TENANT_ENVIRONMENTS`
- `NORN_SOURCES`, `NORN_SECRET_*`, `NORN_S3_*`
- `NORN_OLAP_URL`, `NORN_OLAP_SIGNING_KEY`
- `ALLOWED_ORIGINS`
- `NORN_SYNC_STEP_MS`

The three-instance deployment (compose and systemd) is in `apps/norn-cell-rivet`.

## Conformance

```sh
NORN_CELL_DO_CONFORMANCE_OUT=$P/reference.json cargo test --locked -p norn-cell-do --test conformance_reference
NORN_CELL_RIVET=crates/norn-cell-rivet/target/release/norn-cell-rivet \
  python3 apps/norn-cell-do/conformance/conformance.py --reference $P/reference.json --host rivet-native
```

`--host rivet-native` runs Rivet Engine 3.1.1 in Docker, pinned by digest,
with two runner processes.

## Not covered yet

- WebSocket hibernation: an open socket keeps the actor awake.
- Streaming request bodies, which the router never sends.
- Streaming engine exports: an export is read whole, up to 64 MiB.
- Telemetry export (`OTEL-OFF-1`).
