# Core boundaries and provenance

This test and CI crate checks the retained Cargo boundaries, deterministic core,
canonical-byte ownership, exact source enrollment and shipped-file provenance.
It is never a runtime dependency.

`cargo test --locked -p norn-skeleton-guard` runs the boundary and negative
provenance regressions. `cargo xtask ci provenance --head <SHA> --repository Norn-cloud/norn` checks the candidate;
`cargo xtask ci provenance --root PATH --distribution --base-root BASE
--source-root SOURCE --head SHA --repository Norn-cloud/norn` checks an expanded
archive against the immutable source commit and trusted source facts. The source binding is independent of the distribution being checked.

The hosted CI runs `scripts/ci/core.sh`. Fork code runs only on GitHub-hosted
runners without repository secrets. The trusted lane builds the base's checker,
reads candidate content as data, preserves core obligations and validates exact
source enrollment using the base's Python validator. Workflow and gate contract
changes require a reviewed base-policy change before updating those contracts.

Repository admission uses `cargo xtask ci provenance --head <full commit SHA>
--repository Norn-cloud/norn`. The gate compares working bytes and the complete
inventory (including rights records) to immutable Git blobs; staging a mutation
cannot renew the binding. Gate reports identify repository, base, candidate,
policy commit and inventory digest outside tracked records.

Records are standing path-scoped rights declarations. Ordinary authored edits
retain metadata. Imports, derivations, origin/category/licence, sources/parents,
notices, evidence and supported connector terms require metadata reconsideration.
Every changed payload and metadata path is covered by the admin's unedited
`/ack-rights <head>` API comment; `/revoke-rights <head>` or removal/editing revokes
it. Gate-change `/approve-gates` is separate. A new head needs a new acknowledgment.
Repeated acknowledgments fail closed; revoke before renewing the acknowledgment.
The base-owned lane checks identity and current evidence; CI cannot infer all
third-party origins. Merge groups require every constituent head's acknowledgment
and exactly the independently recomputed clean merge, including combined edits
to the same file. Hand resolutions require renewed review.

For `git archive`, add `--distribution --root <expanded> --source-root <git>
--base-root <trusted> --head <source commit> --repository Norn-cloud/norn`.
Member sets and bytes, including control files, must match that independent
source. Inventories generated from the distribution itself are never accepted.
Generated release members need a separately verified export binding (unsupported
exports fail closed). `--legacy-distribution --distribution --root <expanded>`
retains strict stored-hash checking. Stale or hashless records cannot use it.
Only top-level legacy hashes are optional in bound mode; all nested source,
rights, notice, API, install-validation and package-member hashes remain enforced.
No historical field deletion belongs in Phase 1. The enabling PR supplies correct
legacy hashes for every changed file so the old base-owned checker still passes.
