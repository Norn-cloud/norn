# Typed authoring

This frontend reads typed TypeScript syntax without executing authored modules.
Its Effect profile and compiler lower definitions and bounded flows to Norn IR;
the Rust checker remains the admission authority through the WASM ABI.

Install the exact dependencies with `bun install --frozen-lockfile`.
`bun src/cli.ts profile <file.ts>` recognizes the supported syntax.
`check <pack-dir|file.ts> --org ID --env ID` compiles and admits a set;
`compile <pack-dir|file.ts> --org ID --env ID --out FILE` writes its IR array.
Set `NORN_CHECKER_WASM_PKG` to the package built by
`crates/norn-checker-wasm/scripts/build-pkg.sh` for admission.

The [quickstart](../../QUICK_START.md) uses an independently authored definition.
`bun run typecheck` and `bun test` exercise the profile, compiler, builders and
native/WASM checker contracts. The public frontend has no remote deployment or
product-server commands.

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
