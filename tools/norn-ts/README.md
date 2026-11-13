# Typed authoring

Norn definitions are written in Effect's vocabulary (Effect Schema for shapes, `Effect.gen` for flows) and type-checked against the pinned `effect` package, but Norn never runs them: the compiler reads the syntax and lowers it to Norn IR, and the Rust checker decides what is admitted.
This frontend is that compiler; the checker runs through its WASM ABI.

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

The native scrap-yield correspondence golden is regenerated with
`bun src/cli.ts question fixtures/questions/scrap-yield.ts --out fixtures/questions/scrap-yield.question.json`.
The authoring suite checks reproduction; the exact cell suite strictly decodes
that same golden, compares it with the admitted native plan and evaluates it.
