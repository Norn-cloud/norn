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
- The R2 keys live in Infisical, never in GitHub secrets (the credential policy, `norn-skeleton-guard` `ci::credentials`), and only the reviewed helper `scripts/ci/infisical-oidc.sh` reads them. On Boat, the step "Read the shared cache keys from Infisical over OIDC" runs it as one of two identities, each in **its own Infisical project**: a `push` to `refs/heads/main` (only inside `ci-cache-writer-boat`) as the writer, which the `NORN_SCCACHE_WRITER_INFISICAL_*` variables of that environment name; every other run as the reader, which the repository's `NORN_SCCACHE_READER_INFISICAL_*` variables name. Each reads `NORN_SCCACHE_{BUCKET,ENDPOINT,KEY_ID,SECRET}` from `prod` `/sccache` of its own project. The reader project holds only the read-only R2 key and the writer project only the read-write one, and neither identity is a member of the other's project, so the reader cannot reach the writer key even with a valid login (Infisical has no folder-level access control on this plan, so a shared project would not do). The writer identity's OIDC binding is this repository's immutable environment subject plus the exact workflow, `main` and `push`, so no other run can log in as it. A fork's run gets no OIDC token at all.
- The start step also sets `SCCACHE_S3_RW_MODE=READ_ONLY` for every event except a `push` to `refs/heads/main`.
- The keys are in the environment of the "Start sccache or compile without it" step and of the server it starts. They are never written to `GITHUB_ENV`, but that server runs as the same user as every later step, so same-repository pull-request code on Boat can read the read-only key. What stops a pull request or a merge group from writing is the read-only token's scope and the environment's branch rule, not step scoping.

`verification/coverage-v1/validate.py` pins all of this. `negative_canaries.py --workflow-contract` runs the start step for every event and rejects each way of widening it: the writer environment for pull requests or for every event, read-write mode for pull requests, keys in the job environment or in another step, keys exported to later steps, and the shared cache on hosted runners.

## Who writes today

Nobody yet. Pushes to `main` run on GitHub-hosted runners, where the shared cache is never used, so the bucket stays empty and Boat pull-request builds only add lookups that miss. That is safe: an unwritten cache is only a cache that never hits. Routing main pushes to Boat changes the runner boundary, which the skeleton guard owns and which has to change guard-first; the sequence is tracked in #1369.

The bucket has no object expiry configured, so an object stays until it is deleted by hand.

A cache outage never fails a build: the wrapper falls back to plain `rustc` when sccache itself errors.

## Credentials

No GitHub secrets: `core` and `test` have `id-token: write` for the key step and nothing else reads a token. GitHub issues this repository **immutable OIDC subjects** (`use_immutable_subject: true`): the subject prefix is `repo:Norn-cloud@251878045/norn@1438876916` (owner id 251878045, repository id 1438876916), not `repo:Norn-cloud/norn`, so a binding on the legacy subject never matches.

| Variable | Where | Names |
|---|---|---|
| `NORN_SCCACHE_READER_INFISICAL_IDENTITY_ID` | repository | the reader identity |
| `NORN_SCCACHE_READER_INFISICAL_PROJECT_ID` | repository | the reader project (holds the read-only R2 key) |
| `NORN_SCCACHE_WRITER_INFISICAL_IDENTITY_ID` | `ci-cache-writer-boat` environment only | the writer identity |
| `NORN_SCCACHE_WRITER_INFISICAL_PROJECT_ID` | `ci-cache-writer-boat` environment only | the writer project (holds the read-write R2 key) |

Each project has `NORN_SCCACHE_BUCKET`, `NORN_SCCACHE_ENDPOINT`, `NORN_SCCACHE_KEY_ID` and `NORN_SCCACHE_SECRET` in `prod` at `/sccache`. The writer variables are never set at repository level: the step asks for the writer only on a push to main, and the binding below refuses every other run anyway.

Infisical OIDC auth (issuer and discovery URL `https://token.actions.githubusercontent.com`, audience `https://github.com/Norn-cloud`), one identity per project, each a member (Viewer) of its own project only:

| Identity | Bound subject | Bound claims |
|---|---|---|
| reader | `repo:Norn-cloud@251878045/norn@1438876916:*` | `repository_id=1438876916`, `job_workflow_ref=Norn-cloud/norn/.github/workflows/ci-policy.yml@*` |
| writer | `repo:Norn-cloud@251878045/norn@1438876916:environment:ci-cache-writer-boat` | `repository_id=1438876916`, `environment=ci-cache-writer-boat`, `job_workflow_ref=Norn-cloud/norn/.github/workflows/ci-policy.yml@refs/heads/main`, `ref=refs/heads/main`, `event_name=push` |

The `ci-cache-writer-boat` environment exists with one deployment branch rule, `main` (`gh api repos/Norn-cloud/norn/environments/ci-cache-writer-boat/deployment-branch-policies` lists exactly `main`).

The check that the reader is refused on the writer project: `GET /api/v2/workspace/<writer project id>/identity-memberships` (Infisical admin) lists the writer identity and no other, and the reader identity's memberships (`GET /api/v1/identities/<reader id>`, its projects) list only the reader project. Run both after any change to either identity; the writer key is reachable only through the writer project.

Without the variables, or when any read fails, Boat keeps its local cache, as before. The old `NORN_SCCACHE_*` repository and environment secrets are drift: the daily credential audit in `Norn-cloud/norn-ops` fails until they are deleted.
