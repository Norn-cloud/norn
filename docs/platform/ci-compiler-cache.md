# CI compiler cache

Rust compiles in the `core` and `bundle` jobs go through sccache. Where the cache lives depends on the runner:

| Runner | Cache | Who writes |
|---|---|---|
| GitHub-hosted | GitHub Actions cache (`SCCACHE_GHA_ENABLED=true`) | every run, in its own ref's scope |
| Boat, bucket keys present | local disk (`SCCACHE_CACHE_SIZE`), then the shared R2 bucket (`SCCACHE_MULTILEVEL_CHAIN=disk,s3`) | pushes to main only; none run on Boat yet (see below) |
| Boat, no bucket keys | local disk only | the VM itself |

Boat never uses the GitHub Actions cache: its egress limit is account-wide, and a Boat bundle once failed on "Egress is over the account limit".

## Why only a push to main writes the shared cache

A compiler cache maps an input hash to an output. Anyone who can write it can make a later build pick up an output they chose, so only code that has landed on `main` writes. A merge group does not: it runs the queued pull request's own build scripts, proc macros and tests before that code lands, and a pull request enters the queue on green checks alone.

- The `core` and `test` jobs enter the `ci-cache-writer-boat` environment only for a `push` to `main` that runs on Boat: made and started by `github-merge-queue[bot]`, with `NORN_MAIN_RUNNER=boat` (#1369). A human's push or re-run of main runs hosted and never receives the key. The environment's only deployment branch is `main`. The retired `ci-cache-writer` environment holds no secrets, so a re-run of a workflow from before #1369 gets no writable key.
- The R2 keys live in Infisical, never in GitHub secrets (the credential policy, `norn-skeleton-guard` `ci::credentials`). On Boat, the step "Read the shared cache keys from Infisical over OIDC" logs in as the Infisical identity `NORN_SCCACHE_INFISICAL_IDENTITY_ID` names and reads `NORN_SCCACHE_{BUCKET,ENDPOINT,KEY_ID,SECRET}` from `NORN_SCCACHE_INFISICAL_PATH`. The repository variables name the **read-only** identity and folder; the `ci-cache-writer-boat` environment's own variables name the read-write ones, and the writer identity's OIDC binding is that environment's subject, so no other run can log in as it. Every other run, merge groups and pull requests included, gets only the read-only token. A fork's run gets no OIDC token at all.
- The start step also sets `SCCACHE_S3_RW_MODE=READ_ONLY` for every event except a `push` to `refs/heads/main`.
- The keys are in the environment of the "Start sccache or compile without it" step and of the server it starts. They are never written to `GITHUB_ENV`, but that server runs as the same user as every later step, so same-repository pull-request code on Boat can read the read-only key. What stops a pull request or a merge group from writing is the read-only token's scope and the environment's branch rule, not step scoping.

`verification/coverage-v1/validate.py` pins all of this. `negative_canaries.py --workflow-contract` runs the start step for every event and rejects each way of widening it: the writer environment for pull requests or for every event, read-write mode for pull requests, keys in the job environment or in another step, keys exported to later steps, and the shared cache on hosted runners.

## Who writes today

Nobody yet. Pushes to `main` run on GitHub-hosted runners, where the shared cache is never used, so the bucket stays empty and Boat pull-request builds only add lookups that miss. That is safe: an unwritten cache is only a cache that never hits. Routing main pushes to Boat changes the runner boundary, which the skeleton guard owns and which has to change guard-first; the sequence is tracked in #1369.

The bucket has no object expiry configured, so an object stays until it is deleted by hand.

A cache outage never fails a build: the wrapper falls back to plain `rustc` when sccache itself errors.

## Credentials

No GitHub secrets: `core` and `test` have `id-token: write` for the key step and nothing else reads a token.

| Variable | Where | Names |
|---|---|---|
| `NORN_SCCACHE_INFISICAL_IDENTITY_ID` | repository | the read-only identity (OIDC subject `repo:Norn-cloud/norn:*`, audience `https://github.com/Norn-cloud`) |
| `NORN_SCCACHE_INFISICAL_IDENTITY_ID` | `ci-cache-writer-boat` environment | the read-write identity (OIDC subject `repo:Norn-cloud/norn:environment:ci-cache-writer-boat`) |
| `NORN_SCCACHE_INFISICAL_PROJECT_ID` | repository | the Infisical project holding the keys |
| `NORN_SCCACHE_INFISICAL_PATH` | repository / environment | the folder (prod) with `NORN_SCCACHE_BUCKET`, `NORN_SCCACHE_ENDPOINT`, `NORN_SCCACHE_KEY_ID`, `NORN_SCCACHE_SECRET` |

Without the variables, or when any read fails, Boat keeps its local cache, as before. The old `NORN_SCCACHE_*` repository and environment secrets are drift: the daily credential audit in `Norn-cloud/norn-ops` fails until they are deleted.
