# CI compiler cache

Rust compiles in the `core` and `bundle` jobs go through sccache. Where the cache lives depends on the runner:

| Runner | Cache | Who writes |
|---|---|---|
| GitHub-hosted | GitHub Actions cache (`SCCACHE_GHA_ENABLED=true`) | every run, in its own ref's scope |
| Boat, bucket keys present | local disk (`SCCACHE_CACHE_SIZE`), then the shared R2 bucket (`SCCACHE_MULTILEVEL_CHAIN=disk,s3`) | main pushes and merge groups only |
| Boat, no bucket keys | local disk only | the VM itself |

Boat never uses the GitHub Actions cache: its egress limit is account-wide, and a Boat bundle once failed on "Egress is over the account limit".

## Why pull requests can't write the shared cache

A compiler cache maps an input hash to an output. Anyone who can write it can make a later build pick up an output they chose, so only reviewed code writes:

- The `core` and `test` jobs enter the `ci-cache-writer` environment only for `push` to `main` and for `merge_group`. The environment's deployment branches are `main` and `gh-readonly-queue/main/*`.
- `NORN_SCCACHE_KEY_ID` and `NORN_SCCACHE_SECRET` are repository secrets holding a **read-only** R2 token. The environment overrides them with a read-write token. Every other run, pull requests included, gets only the read-only token.
- The start step also sets `SCCACHE_S3_RW_MODE=READ_ONLY` for every event except a `push` to `refs/heads/main` and a `merge_group` on `refs/heads/gh-readonly-queue/main/*`.
- The keys reach only the "Start sccache or compile without it" step and the server it starts. They are never written to `GITHUB_ENV`.

`verification/coverage-v1/validate.py` pins all of this. `negative_canaries.py --workflow-contract` runs the start step for every event and rejects each way of widening it: the writer environment for pull requests or for every event, read-write mode for pull requests, keys in the job environment or in another step, keys exported to later steps, and the shared cache on hosted runners.

A cache outage never fails a build: the wrapper falls back to plain `rustc` when sccache itself errors.

## Secrets

| Name | Where | Holds |
|---|---|---|
| `NORN_SCCACHE_BUCKET` | repository | bucket name |
| `NORN_SCCACHE_ENDPOINT` | repository | `https://<account>.r2.cloudflarestorage.com` |
| `NORN_SCCACHE_KEY_ID`, `NORN_SCCACHE_SECRET` | repository | read-only R2 token |
| `NORN_SCCACHE_KEY_ID`, `NORN_SCCACHE_SECRET` | `ci-cache-writer` environment | read-write R2 token |

Without the bucket secrets, Boat keeps its local cache, as before.
