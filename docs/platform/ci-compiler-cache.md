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

- The `core` and `test` jobs enter the `ci-cache-writer` environment only for `push` to `main`. The environment's only deployment branch is `main`.
- `NORN_SCCACHE_KEY_ID` and `NORN_SCCACHE_SECRET` are repository secrets holding a **read-only** R2 token. The environment overrides them with a read-write token. Every other run, merge groups and pull requests included, gets only the read-only token.
- The start step also sets `SCCACHE_S3_RW_MODE=READ_ONLY` for every event except a `push` to `refs/heads/main`.
- The keys are in the environment of the "Start sccache or compile without it" step and of the server it starts. They are never written to `GITHUB_ENV`, but that server runs as the same user as every later step, so same-repository pull-request code on Boat can read the read-only key. What stops a pull request or a merge group from writing is the read-only token's scope and the environment's branch rule, not step scoping.

`verification/coverage-v1/validate.py` pins all of this. `negative_canaries.py --workflow-contract` runs the start step for every event and rejects each way of widening it: the writer environment for pull requests or for every event, read-write mode for pull requests, keys in the job environment or in another step, keys exported to later steps, and the shared cache on hosted runners.

## Who writes today

Nobody yet. Pushes to `main` run on GitHub-hosted runners, where the shared cache is never used, so the bucket stays empty and Boat pull-request builds only add lookups that miss. That is safe: an unwritten cache is only a cache that never hits. Routing main pushes to Boat changes the runner boundary, which the skeleton guard owns and which has to change guard-first; the sequence is tracked in #1369.

The bucket has no object expiry configured, so an object stays until it is deleted by hand.

A cache outage never fails a build: the wrapper falls back to plain `rustc` when sccache itself errors.

## Secrets

| Name | Where | Holds |
|---|---|---|
| `NORN_SCCACHE_BUCKET` | repository | bucket name |
| `NORN_SCCACHE_ENDPOINT` | repository | `https://<account>.r2.cloudflarestorage.com` |
| `NORN_SCCACHE_KEY_ID`, `NORN_SCCACHE_SECRET` | repository | read-only R2 token |
| `NORN_SCCACHE_KEY_ID`, `NORN_SCCACHE_SECRET` | `ci-cache-writer` environment | read-write R2 token |

Without the bucket secrets, Boat keeps its local cache, as before.
