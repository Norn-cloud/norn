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

The `workers::WorkersEngine` adapter (wasm32 only) uses DO SQLite on celld
and workerd. Its storage handle is trusted host configuration, never request
input. TXN-ATOMIC-1 rolls back typed Rust errors by throwing through the platform
transaction callback. Transactions reject nesting; the platform rejects SQL
transaction control. Cursors are drained before returning. Large exact values
use the shared store's text representation. See the `workers_sql` example for
real-runtime checks; it is an adapter probe, not a governed action endpoint.

Build the example with `cargo rustc --locked -p norn-cell --no-default-features
--example workers_sql --target wasm32-unknown-unknown --profile wasm-release`,
then run matching `wasm-bindgen --target web --out-name workers_sql --out-dir
crates/norn-cell/examples/workers-sql/pkg` on the resulting example Wasm.
With esbuild and the pinned celld on PATH, run `celld dev --port 9876` from
`examples/workers-sql`. `/` exercises the adapter; `/state` returns semantic
SQLite/alarm state; `/alarm-refusal` must return 422 with identical state.
Restart without `--clean` and compare `/state` before rerunning `/`.
