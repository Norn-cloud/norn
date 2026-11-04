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

All rights writers and both trusted admission jobs share the
`rights-authority-v4` GitHub Actions mutex, with queued work retained and active
work never cancelled by another rights event. Test jobs remain outside this
mutex. The source run's server creation time sets a two-hour end-to-end deadline: an
expired authority run publishes failure and cannot publish success. The same
deadline is checked immediately before publication.

A reconciled cursor bounds history to new runs. Its append-only Git journal
retains each target's saved event identities; a bot receipt binds the immutable
checkpoint commit. The `ci/rights-ledger-v1` ref retains those tool-managed
metadata objects and only advances without force. It is separate from shipped
source. A failed write cannot advance the cursor; an unpublished journal commit
is reconstructed from the last published checkpoint. No fixed date or page
count cuts off history. Each target's live rights ledger, including pending
events and failure generation, has its own SHA/tree-bound checkpoint digest.

Ordinary comments without rights markers never enter the authority queue. Their
saved-payload classification is available before a runner starts; possible
commands get a normalized identity stamp in the capture job before any writes.
Both collectors deduplicate offset pages, wait up to thirty seconds for capture
and five seconds for an in-flight ledger, and fail closed at their deadline.
Known pending events remain scoped to their target. Re-run the whole original
event as an admin to reconcile it: retries conservatively record a revocation,
then require a fresh review after the successful receipt. Same-second review
fails closed. Missing source runs and changed saved identities refuse admission.

Publication re-collects the ledger, compares its digest with the decision, and
re-reads the target and publication checkpoints immediately before writing.
Success records the digest and latest failure generation. The evaluator rejects
a late success carrying a generation older than the latest failure. The global
authority mutex covers this entire validation/publication transaction, including
all constituents of a merge group. Per-target concurrency is deferred to Phase 2.
The PR records the measured duration and resulting single-authority throughput
bound; API and runner delays can reduce it. All reads and receipts remain
base-owned; no candidate code executes in this lane.
The base-owned lane checks identity and current evidence; CI cannot infer all
third-party origins. Merge groups require every constituent head's acknowledgment
and exactly the independently recomputed clean merge, including combined edits
to the same file. Hand resolutions require renewed review. Cross-side payload and rights-record
changes also require renewed review, including metadata already on the queue
base; a clean textual merge alone does not review their combined meaning.

For `git archive`, add `--distribution --root <expanded> --source-root <git>
--base-root <trusted> --head <full lowercase source commit SHA> --repository Norn-cloud/norn`.
Member sets and bytes, including control files, must match that independent
source. Inventories generated from the distribution itself are never accepted.
Generated release members need a separately verified export binding (unsupported
exports fail closed). `--legacy-distribution --distribution --root <expanded>`
retains strict stored-hash checking. Stale or hashless records cannot use it.
Only top-level legacy hashes are optional in bound mode; all nested source,
rights, notice, API, install-validation and package-member hashes remain enforced.
No historical field deletion belongs in Phase 1. The enabling PR supplies correct
legacy hashes for every changed file so the old base-owned checker still passes.
