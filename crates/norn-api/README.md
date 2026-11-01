# Core wire contracts

Projects the Rust IR and checker schemas into OpenAPI components for the TypeScript SDK.
Run `norn openapi` to regenerate the contract. Cell execution uses `norn-cell` and
`norn-kernel` directly; the fixture example supplies a local definitions server.

## Host export contract (slice 1a)

Exports scope and refusal infrastructure without a server or store.
Transport adapters map `Refusal.status` to their response.

- HOST-SCOPE: missing/blank dimensions refuse with `scope.missing` (400);
  exact dimension mismatches refuse with `scope.denied` (403).
  Keys percent-escape `%` and `/` once per component.
- HOST-WIRE: camelCase scopes, snake_case categories, status outside JSON.
- HOST-REFUSAL: conceal opaque diagnostics while retaining code/category/status;
  explicit categories override status defaults. Commit projection follows later.
- HOST-IDENTITY: visibility uses canonical `norn_ir::ScopeRef`.

Correspondence: `tests/host_perimeter.rs`. Denial-before-read and default-deny
belong to host wiring; the perimeter performs no IO.

## Host admission epoch (slice 1b)

HOST-ADMISSION: `DevHost::new` checks a full source array before initializing a
fresh tenant store: unrelated residual schema/state refuses. An identical
initial provisioning request reopens without writing.
Only a successful transactional definition install publishes
the host. Trusted installs check before taking the exclusive definition epoch;
failed admission and stale-version installs preserve both revision and ledger.
Readers hold the shared epoch through authorization, store load and projection.
HOST-EXACT: retained source uses the existing RuntimeValue path, and the checker
issues its receipt unchanged. HOST-RECEIPT is **not claimed**: #290/#273 track the
checker identity gap. No additional host canonicalizer is introduced.
HOST-IDENTITY: artifacts, scope and logical time remain the canonical IR/kernel
types. The API now owns a ring-1 host over ring-1 cells; pure contracts stay here.
Correspondence: `host::tests`. HTTP reads and action parity follow in later slices.

HOST-SCOPE/HOST-AUTHORITY: scope and `ReadDefinitions` precede store queries (SQL spy); default deny.
HOST-ADMISSION: one definition epoch covers load and projection; retained versions remain readable.
HOST-WIRE/HOST-EXACT: raw source/receipt JSON, camelCase, empty collections and exact numeric vectors.
HOST-REFUSAL: absent version `defs.version_not_found` (404); malformed query `request.invalid` (400).
HTTP requires an authenticated routing host to supply x-norn-actor; host errors conceal diagnostics.

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


HOST-FIXTURE-BOOTSTRAP: the fixture executable combines exact artifact arrays,
checks and atomically installs the complete SalesOrder/stock set into a new
in-memory tenant on every start. It binds loopback port 4174. Bearer authentication
removes caller actor headers and supplies a fixed read principal only after a
credential match; anonymous, incorrect-token and spoofed-actor requests deny.
Correspondence: the example's authentication matrix and a real TCP Boat smoke
that compiles stock, starts the command and checks both namespaces via `/v1/defs`.

From the repository root (fixture sources require #147 and #227):

```sh
(cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile) && mkdir -p ~/.cache/agent-scratch/norn-demo-host && bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/engine-alpha --org org_demo --env dev --out ~/.cache/agent-scratch/norn-demo-host/alpha.json && bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/stock --org org_demo --env dev --out ~/.cache/agent-scratch/norn-demo-host/stock.json && NORN_DEMO_TOKEN=local-demo cargo run --locked -p norn-api --example fixture-server -- ~/.cache/agent-scratch/norn-demo-host/alpha.json ~/.cache/agent-scratch/norn-demo-host/stock.json
```

Read with `Authorization: Bearer local-demo` at
`http://127.0.0.1:4174/v1/defs?orgId=org_demo&environmentId=dev`.
The environment credential is local fixture configuration. This host exposes
definition reads; actions and subscriptions remain the adapter lane's scope.
HOST-MODEL-DEFAULT-SNAPSHOT remains unclaimed: numeric JSON object defaults use
the legacy floating-point IR and need a shared representation migration.
HOST-VERSION-ABSENT: every syntactically valid but unrepresentable revision is
absent (404), including u64::MAX and the first integer above the SQL seam limit.
HOST-HTTP-WORKER: synchronous store access and complete retained-set admission
run on the blocking pool, outside the Tokio request worker; actor/scope admission
still precedes store reads. HTTP correspondence covers allowed/denied actors.

HOST-CACHE-PRIVATE: all definition HTTP responses, including refusals, declare
`Cache-Control: no-store`; scope/actor-sensitive results never authorize shared
cache reuse. The HTTP success/refusal matrices assert the header.

HOST-FIXTURE-SCOPE: both authored sets compile with the same explicit scope;
the original org_alpha golden is never adopted by the org_demo host.
