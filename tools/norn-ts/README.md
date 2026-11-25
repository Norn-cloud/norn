# Typed authoring

Norn definitions are written in Effect's vocabulary (Effect Schema for shapes, `Effect.gen` for flows) and type-checked against the pinned `effect` package, but Norn never runs them: the compiler reads the syntax and lowers it to Norn IR, and the Rust checker decides what is admitted.
This frontend is that compiler; the checker runs through its WASM ABI.

Install the exact dependencies with `bun install --frozen-lockfile`.

## Fast local setup (no Rust build)

Download the verified prebuilt runtime (`norn-checker-wasm`, `norn-dev-host`, and `norn`) once into `~/.cache/norn/runtime/`:

```bash
bun src/cli.ts setup                  # or from repo root: scripts/dev/fetch-local-runtime.sh
```

Once cached, `norn check`, `norn explain`, and `norn dev` auto-discover `~/.cache/norn/runtime/` with zero extra environment variables (respecting `XDG_CACHE_HOME` and `NORN_CACHE_DIR`). Override with `NORN_CHECKER_WASM_PKG` or `NORN_DEV_HOST_BIN` whenever testing a local Rust build (for example from `crates/norn-checker-wasm/scripts/build-pkg.sh`).

## Which `norn`

This package's `bin` is `norn` (`src/cli.ts`). The Rust CLI in `crates/norn-cli` is
also called `norn` and has different commands; its `check` reads compiled JSON IR,
this one's reads typed sources. Renaming one is
[#1451](https://github.com/Norn-cloud/norn/issues/1451). From this directory, run
this one as `bun src/cli.ts`; in a project made by `norn init`, as `bunx norn`.

## Commands

- `setup [--version TAG] [--base-url URL]` fetches and SHA-256 verifies the prebuilt `norn-checker-wasm`, `norn-dev-host`, and `norn` runtime into `~/.cache/norn/runtime/`.
- `init <dir>` creates a project (`norn.json`, `src/`, `AGENTS.md`) and installs it.
- `dev [--port N] [--json]` runs the native dev host (`norn-dev-host`, from
  `norn-cell-do`; auto-discovered from `~/.cache/norn/runtime/bin/norn-dev-host`
  after `norn setup`, or overridden via `NORN_DEV_HOST_BIN`) over the
  project and reinstalls every save that is admitted.
- `fmt [path...] [--check]` formats typed sources.
- `check <pack-dir|file.ts> --org ID --env ID` compiles and admits a set.
- `compile <pack-dir|file.ts> --org ID --env ID --out FILE` writes its IR array.
- `profile <file.ts>` reports whether a file stays inside the supported syntax.
- `question <file.ts>` and `dashboard <file.ts>` emit candidate `QuestionIR` and
  dashboard values.
- `codegen --ts <artifacts.json|artifact-dir|pack-dir|project-dir> [--base <artifacts>]... [--out FILE.d.ts] [--check]`
  emits one deterministic, types-only `.d.ts` for an admitted set: `ActionMap`
  (action id → the input a caller sends, without actor-derived `fromCaller`
  fields, and the declared output; exact decimals as `string`), `ObjectMap` and
  `DefinitionsDigest`, for the `@norn/sdk` client. `--base` resolves shared value
  types and supertypes (`--base registry/core/artifacts` for registry packs)
  without typing them. A compiled set is read as admitted; a pack or project is
  admitted first. Shapes outside the closed dialect fail the run; `--check`
  fails on drift.
- `plan|apply <pack-dir|file.ts> --url URL --org ID --env ID` sends a pack to a
  running hosted cell's `definitions:plan` / `:apply`; the cell decides and installs
  only for an owner or admin. Credentials come from the environment
  (`--password-env VAR` with `--email`, or `--token-env VAR`).
- `explain <action|law|concept> <id> [--json]` explains a declaration of the project: what it reads and writes, the laws that apply, and refusals as code frames.

The [quickstart](../../QUICK_START.md) uses an independently authored definition, and
[the authoring tutorial](../../docs/authoring/tutorial.md) walks through `init`, `dev`
and `explain`. `bun run typecheck` and `bun test` exercise the profile, compiler,
builders and native/WASM checker contracts.

### Standalone typed questions

`bun src/cli.ts question file.ts` emits candidate `QuestionIR` values from
`Norn.question({ name, of, where, measure, by, asOf, population })`. `of` is an
object definition reference or installed qualified name, `measure` is an exact
metric identity, and direct predicates/dimensions retain the existing IR shape.
The initial population is `{ kind: "authorized_objects" }`; the host resolves
scope, visibility, source pins and population identity.
`asOf` declares `{ basis: "effective", from, to, freshness }`. The ordinary Rust
compute checker must admit the candidate and selected metric definitions before
execution. Knowledge-time policy, richer populations, rendering and ambiguity
interpretation remain separate qualification work.

The native scrap-yield correspondence golden is regenerated with
`bun src/cli.ts question fixtures/questions/scrap-yield.ts --out fixtures/questions/scrap-yield.question.json`.
The authoring suite checks reproduction; the exact cell suite strictly decodes
that same golden, compares it with the admitted native plan and evaluates it.
