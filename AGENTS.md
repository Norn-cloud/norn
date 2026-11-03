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
`.github/ci/provenance/artifacts/<path>.json`, with its standing origin,
category and licence declaration. Independently authored edits retain that
metadata. New files add records; deletions remove records; renames move them.
Origin, category, licence, sources, parents, notices, rights evidence and supported
connector terms changes require corresponding metadata changes. Every changed
payload and metadata path needs an admin `/ack-rights <exact head SHA>` comment,
collected from the API by base-owned policy. This is separate from `/approve-gates`;
new heads invalidate it, edits/deletions revoke it, and `/revoke-rights <SHA>`
explicitly revokes it. Base-owned append-only commit status history retains the
revocation boundary across comment edits/deletions; a fresh acknowledgment must
postdate the recorded boundary. Merge groups also refuse unreviewed cross-side
payload/rights-record combinations, including rights changes already on base.
Human source review remains necessary. The gate binds all
inspected bytes and records to the immutable candidate tree. Expanded archives
must match that source commit exactly; generated exports require an independently
verified external binding and are refused without one. Nested evidence and
package-member hashes remain enforced. Historical top-level hashes stay readable
and shape-checked but are authoritative only in explicit legacy distributions.
Until the enabling validator lands on base, changed files still need correct
legacy hashes for the old base-owned checker. The path comes from the record's file
name, so it is not a field. `.github/ci/provenance/manifest.json` holds the
version and software sources; nothing else lives in that directory.
`cargo xtask ci provenance` enforces this, and
the trusted policy lane checks pull requests against the base branch's rules.
Symlinks, customer data, credentials, vendor schemas and copied GPL-family code
are refused.

## Verification coverage

Coverage lives in `verification/coverage-v1/manifest/`: `index.json` holds the
status vocabulary and evidence roots, and `subsystems/<id>/index.json` holds metadata. Each array member lives in
`<field>/<identity-hash>.json` with an `order` and `value`; the loader computes
identities with `entry_id`. A new executable source adds one entry under its
subsystem's `sourceFiles` directory and its own provenance records. Do not
rewrite other entries. The subsystem id is its directory name.
`scripts/coverage_manifest.py` is the only reader and writer.

## Changes and merging

- **Coherent PRs.** One coherent feature or fix per PR, with no line cap,
  open for hours rather than days. Don't split work into stacked PRs just to
  keep them small; stack only when a layer has to merge first.
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

## Norn authoring

Before writing or reviewing Norn definitions or engine features, every agent must read:

- [The authoring reference](docs/authoring/README.md), including CURRENT versus TARGET syntax.
- The relevant `docs/spec/core/01–08` file selected through the [core spec index](docs/spec/core/README.md), and its named acceptance scenarios.
- [The 2026-11-01 decision record](docs/architecture/decisions/2026-11-01.md), which governs older designs.

Use the [norn-authoring skill](.claude/skills/norn-authoring/SKILL.md) for definitions, packs, laws, mappings and authoring SDK work. Implement specified invariants, refusals and scenarios; report ambiguous contracts rather than inventing domain kernel behavior.
