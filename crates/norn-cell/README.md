# norn-cell

`norn-cell` is the implemented ring 1 cell store over host-provided `SqlEngine`.
The default `native` feature selects rusqlite and native export/restore support;
the Rust Durable Object host supplies its own SQL adapter. Hosts own durability.
Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.

## Modules beyond the store

- `compute`: exact question and transform evaluation over the cell's own rows,
  with lineage, materialized transform results (VIEW-MAT-1) and receipt-driven
  incremental maintenance of question answers (`compute::ivm`,
  QUERY-ENGINE-1). With the `duckdb` feature an admitted transform can also run
  on embedded DuckDB over the same authorized snapshot (VIEW-ENGINE-1); a plan
  outside its subset refuses `core.compute.unsupported_engine`.
- `analytics`: the typed contract of the tenant analytics engine. The
  cell authorizes, lowers an admitted plan and verifies every answer; the
  engine only runs typed operations (no SQL, table or column expression) over
  one tenant's database. `analytics::engine`, the DuckDB implementation, needs
  the `duckdb` and `native` features; the daemon that serves it is
  `norn-olapd` in `norn-cell-do`.
- `reactive`: subscriptions over declared, profile-bounded read slices.
- `export` exports the committed journal; `restore` (`native`) rebuilds a
  prefix of it through the typed commit path.

## Restoring a cell that will not open

`norn-cell-restore` (bin, `native` feature) is the operator path for a cell
whose open refuses (`cell.unopenable`):

```sh
cargo run -p norn-cell --bin norn-cell-restore -- diagnose <cell.sqlite>
cargo run -p norn-cell --bin norn-cell-restore -- restore <cell.sqlite> <target.sqlite> <staging-dir> [--through N]
```

`diagnose` prints how far the history is proven and the first ledger entry the
store's value proofs refuse. `restore` rebuilds the proven prefix of the cell's
export journal into an absent target; a prefix shorter than the journal needs
`--through N` with N equal to the diagnosed `provenThrough`. The source opens
read-only. Run it on a copy taken while the cell is not serving, with its
`-wal` and `-shm` files when present.

## Tenant provisioning invariant

HOST-PROVISION-ATOMIC: initialization, scope binding, initial definitions, model
snapshot, receipt and ledger publish in one engine transaction. Failure or abrupt
process loss before commit leaves no application schema; after commit a lost
reply leaves the complete initial revision. An identical provisioning request
(scope, projected model and full install input, including logical time) reopens
the current revision without writes, including after later trusted installs; a
changed provisioning request refuses. Residual unrelated schema is never adopted. The transaction owns the
freshness check, so competing requests cannot both initialize the cell.
Correspondence: `definitions_install::host_provision_atomic_at_every_storage_boundary`
injects errors and abrupt subprocess exit after every SQL operation, before
commit and after commit, then reopens, retries and checks conflicting retries.

The wasm32 `workers::WorkersEngine` uses DO SQLite on workerd.
The storage handle is trusted host configuration. TXN-ATOMIC-1 throws typed
Rust refusals through the platform callback to roll back. Nested/SQL transaction
control is refused, cursors are drained, and exact wide values use text.
The `workers_sql` example probes the adapter rather than governed actions.

Build and run with generated files and durable state outside the checkout:

```sh
export NORN_WORKERS_SQL_PROJECT="${CARGO_TARGET_DIR:?set a build directory outside the checkout}/workers-sql"
mkdir -p "$NORN_WORKERS_SQL_PROJECT"
cp crates/norn-cell/examples/workers-sql/{worker.mjs,wrangler.json} "$NORN_WORKERS_SQL_PROJECT/"
cargo rustc --locked -p norn-cell --no-default-features --example workers_sql \
  --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/examples/workers_sql.wasm" \
  --target web --out-name workers_sql --out-dir "$NORN_WORKERS_SQL_PROJECT/pkg"
python3 crates/norn-cell/examples/workers-sql/check.py
```

Use matching wasm-bindgen, esbuild on PATH, and Wrangler 4.157.0 on PATH.
The probe compares semantic SQLite/alarm state after refusal and restart.
