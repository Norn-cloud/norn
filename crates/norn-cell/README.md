# norn-cell

`norn-cell` is the implemented ring 1 cell store over host-provided `SqlEngine`.
The default `native` feature selects rusqlite and native export/restore support;
the Rust Durable Object host supplies its own SQL adapter. Hosts own durability.
Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.

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

The wasm32 `workers::WorkersEngine` uses DO SQLite on celld and workerd.
The storage handle is trusted host configuration. TXN-ATOMIC-1 throws typed
Rust refusals through the platform callback to roll back. Nested/SQL transaction
control is refused, cursors are drained, and exact wide values use text.
The `workers_sql` example probes the adapter rather than governed actions.

Build and run with generated files and durable state outside the checkout:

```sh
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:?set a build directory outside the checkout}"
export NORN_WORKERS_SQL_PROJECT="${CARGO_TARGET_DIR:?set a build directory outside the checkout}/workers-sql"
mkdir -p "$NORN_WORKERS_SQL_PROJECT"
cp crates/norn-cell/examples/workers-sql/{worker.mjs,wrangler.json} "$NORN_WORKERS_SQL_PROJECT/"
cargo rustc --locked -p norn-cell --no-default-features --example workers_sql \
  --target wasm32-unknown-unknown --profile wasm-release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/examples/workers_sql.wasm" \
  --target web --out-name workers_sql --out-dir "$NORN_WORKERS_SQL_PROJECT/pkg"
python3 crates/norn-cell/examples/workers-sql/check.py
```

Use matching wasm-bindgen, esbuild on PATH, and celld v0.6.3 (pin/checksum in
[#728](https://github.com/Norn-cloud/norn/issues/728#issuecomment-6604974784)).
The probe compares semantic SQLite/alarm state after refusal and restart.
Pass `wrangler` to the probe for parity with Wrangler 4.157.0 on PATH.
