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
Rights state belongs to the immutable head SHA and tree SHA, never a PR number.
An acknowledgment approves the standing declarations of that whole target tree,
including every changed payload and metadata path. Different PRs with the same
target read the same ledger. The latest recorded review remains the frontier
even after its source comment is edited or removed; an older review cannot
reappear. Gate-change approvals remain separate and PR-scoped.

A pure capture job reads the original saved run payload before any API write.
It records plain-issue versus pull-request attribution and derives the exact
target heads from rights commands. The journal job's saved identity contains
those heads and the source comment ID. A failed write therefore remains pending
for every admission of that target, including admission through another PR.
Unknown uncaptured events fail closed for all targets. Plain-issue captures are
ignored even when their journal job is skipped under an unexpanded name.

Both collectors read one repository-wide event history from protocol activation,
deduplicate repeated run IDs before checking continuity, and read the append-only
ledger for each exact head/tree. Real gaps, missing receipts and failed jobs
refuse admission. Re-run the whole original event as an admin to reconcile it:
retries conservatively record a revocation, then require a fresh review after
the successful receipt. Same-second review fails closed. All reads and receipts
remain base-owned; no candidate code executes in this lane.
The base-owned lane checks identity and current evidence; CI cannot infer all
third-party origins. Merge groups require every constituent head's acknowledgment
and exactly the independently recomputed clean merge, including combined edits
to the same file. Hand resolutions require renewed review. Cross-side payload and rights-record
changes also require renewed review, including metadata already on the queue
base; a clean textual merge alone does not review their combined meaning.

For `git archive`, add `--distribution --root <expanded> --source-root <git>
--base-root <trusted> --head 69ffae65225a6784ebcb96696fc88c65d810fa4f --repository Norn-cloud/norn`.
Member sets and bytes, including control files, must match that independent
source. Inventories generated from the distribution itself are never accepted.
Generated release members need a separately verified export binding (unsupported
exports fail closed). `--legacy-distribution --distribution --root <expanded>`
retains strict stored-hash checking. Stale or hashless records cannot use it.
Only top-level legacy hashes are optional in bound mode; all nested source,
rights, notice, API, install-validation and package-member hashes remain enforced.
No historical field deletion belongs in Phase 1. The enabling PR supplies correct
legacy hashes for every changed file so the old base-owned checker still passes.
