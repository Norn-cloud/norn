# Agent and contributor notes

These rules apply to every change in this repository, whether a person or a
coding agent writes it. `CONTRIBUTING.md` holds the legal terms; this file
holds the engineering rules.

## What lives here

Norn's embedded core: canonical IR (`norn-ir`), checker (`norn-checker`,
`norn-checker-wasm`), kernel (`norn-kernel`), cells (`norn-cell`), CLI
(`norn-cli`), the typed TypeScript authoring toolchain (`tools/norn-ts`) and the
generated SDK (`packages/norn-sdk`, `sdks/openapi`). `vendor/zen` is MIT
third-party code; keep its notices and provenance.

## Architecture rules

- **Typed authoring only.** Definitions are written in typed TypeScript
  (`tools/norn-ts`) or Rust and compile to canonical JSON IR. Never hand-write
  IR, decision tables, policies, JSON or TOML that a generator owns; change the
  source and regenerate.
- **Definitions are declarative.** Object, relation, action, decision,
  lifecycle, connector and widget definitions serialize to IR and perform no
  IO. Authoring callbacks such as `Norn.expression(Schema, input => ...)` are
  compiled to declarative IR; no callback survives extraction or runs at
  runtime. Extraction rejects non-serializable values and restricted imports.
- **Rust is the engine.** Use typed `Result<Output, DomainError>` errors,
  `tracing`, and explicit dependencies. No `unsafe` in kernel or evaluator
  crates.
- **Cells own durability.** One cell owns commit order, time and live
  connections through the four-verb `CellHost` boundary (`commit`, `perform`,
  `observe`, `clock`). Long work is facts, commands and timers, not processes.
- **Scope and authorization fail closed.** Org, tenant, user, role and
  environment scope are explicit at every boundary and checked before data
  access or mutation.
- **SDKs come from contracts.** `norn openapi` emits the OpenAPI document; the
  SDK is generated from it and CI fails on drift. Never hand-edit generated
  files.
- **Reuse first.** Use the standard library, existing workspace dependencies or
  a maintained, permissively licensed crate before writing new code. The
  checker, kernel, ledger and authorization decisions stay Norn-owned.
- **Delete instead of preserving.** Superseded code is removed, not kept behind
  compatibility shims.

## Verification

- Behaviour changes in the kernel, evaluator and checker start with the law or
  invariant, then the code.
- Label evidence honestly: structural, exhaustive-finite, bounded, checked
  proof, runtime-enforced, sampled, refuted or unknown. Never claim a bare
  "verified".
- Laws are protected. Weakening a law, adding a precondition or excluding
  inputs needs separate maintainer approval; a passing proof never authorizes
  it.

## Provenance

Every tracked file `<path>` outside `.github/ci/provenance/` has one record,
`.github/ci/provenance/artifacts/<path>.json`, with its SHA-256, origin,
category and licence. A change updates that record's hash, a new file adds a
record, and a deleted file deletes it. The path comes from the record's file
name, so it is not a field. `.github/ci/provenance/manifest.json` holds the
version and software sources; nothing else lives in that directory.
`cargo xtask ci provenance` enforces this, and
the trusted policy lane checks pull requests against the base branch's rules.
Symlinks, customer data, credentials, vendor schemas and copied GPL-family code
are refused.

## Verification coverage

Coverage lives in `verification/coverage-v1/manifest/`: `index.json` holds the
status vocabulary and evidence roots, and `subsystems/<id>.json` holds one
subsystem's obligations, claims and `sourceFiles`. The id is the file name, so
it is not a field. A new executable source joins exactly one subsystem's
`sourceFiles`; edit only that subsystem's file and its provenance record.
`scripts/coverage_manifest.py` is the only reader and writer.

## Changes and merging

- **Small PRs.** One reviewable unit per PR, about 400 changed lines of
  non-generated code at most, open for hours rather than days. Split larger
  work into units that each merge on their own.
- **Merge bar.** Every required check is green on the exact head being merged,
  review is approved and no findings are open. Never bypass branch protection,
  skip or weaken a gate, or grow an allowlist to get green.
- **No new red.** A red `main` is fixed first by one owner. A PR may merge
  while `main` is red only if its own checks are green, every failing check
  fails identically on `main`, each failure has an open issue, and the PR does
  not touch the failing area. Never introduce a new failure.
- **Requalify before merging.** Merge the latest `main` into the branch and
  rerun the affected checks. Never force-push shared branches.
- **Compile wide, test narrow.** Compile and lint the whole workspace
  (`cargo clippy --locked --workspace --all-targets -- -D warnings`), then run
  the tests for the crates you touched and their dependents. `bash
  scripts/ci/core.sh cheap` is the fast gate; `bash scripts/ci/core.sh` is the
  full one.
- **Heavy work runs off shared machines.** Full builds, test suites, fuzzing,
  security scans and formal verification run in CI or disposable Boat
  sandboxes, never on a shared development host.

## Working in the tree

- Add a Rust dependency to the crate that needs it (`cargo add <crate> -p
  <package>`); it must pass `deny.toml`.
- TypeScript packages install into their own directory only (`bun install
  --frozen-lockfile` inside `tools/norn-ts` or `packages/norn-sdk`).
- Do not commit `.env` files, credentials, logs or scratch output.
- Never revert changes you did not make.
