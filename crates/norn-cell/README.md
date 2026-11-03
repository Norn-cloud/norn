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
