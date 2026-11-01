# Core wire contracts

Projects the Rust IR and checker schemas into OpenAPI components for the TypeScript SDK.
Run `norn openapi` to regenerate the contract. Cell execution uses `norn-cell` and
`norn-kernel` directly; this crate supplies no server.

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
revision 1 without writes; a changed request or advanced definition revision
refuses. Residual unrelated schema is never adopted. The transaction owns the
freshness check, so competing requests cannot both initialize the cell.
Correspondence: `definitions_install::host_provision_atomic_at_every_storage_boundary`
injects errors and abrupt subprocess exit after every SQL operation, before
commit and after commit, then reopens, retries and checks conflicting retries.
