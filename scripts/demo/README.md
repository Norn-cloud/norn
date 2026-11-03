# Jewelry demo

Run the demo through the product: `norn capabilities` discovers the installed
operations, `norn mcp` serves them over MCP, and the [demo UI](../../packages/norn-demo-ui/README.md)
renders the server's governed documents. Discover actual operations before
invoking them; a checked definition is not a completed business transaction.

The six-beat status and blockers live in the [single gap matrix](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6559837435)
and [DAG visualizer tracking issue](https://github.com/Norn-cloud/norn/issues/566).
[Jewelry acceptance](../../docs/spec/acceptance/jewelry-demo.md),
[data acceptance](../../docs/spec/acceptance/data-engineering.md) and
[master-ontology acceptance](../../docs/spec/acceptance/master-ontology-mvp.md)
define the required behavior. Test profiles do not replace these product surfaces.

## Reproduce a drop

[GitHub Releases](https://github.com/Norn-cloud/norn/releases) retain each
published drop's main SHA, Boat result and honest boundaries. Backfilled releases
preserve historical evidence; they add no delivered capability. Detach at the
release's recorded main SHA for historical reproduction. The profiles below
arrive with this conversion and are not retroactively present in older tags.

Use Rust 1.98, cargo-nextest 0.9.150 and Bun with frozen authoring dependencies:

```sh
(cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile)
cargo nextest run --profile drop-02
```

| Profile | Evidence selected |
| --- | --- |
| `drop-01` | Exact values, codecs, units and three bounded Kani harnesses; installs the exact Kani 0.68.0 tool through Cargo/native setup |
| `drop-02` | Freshly authored law, admission/refusals, generated obligations and EN/AR rendering |
| `drop-03` | Freshly authored catalog, bounded reads, kernel PDP and write refusals |
| `drop-04` | Fresh allocation goldens and seven exact fold scenarios |
| `drop-05` | Twelve quarantine/recovery scenarios; sampled properties use 16 cases |
| `drop-06` | Fresh pricing declarations and both native SQLite scenarios |
| `drop-07` | Three incremental/recompute scenarios |
| `drop-08` | Ten cutoff/restatement scenarios |
| `drop-09` | Fresh pack goldens and eight synthetic three-system mapping scenarios |
| `drop-constitution` | Loss mutation/refusal/restoration; generated goals remain unknown (#565) |

Nextest owns selection, setup commands, process execution and failures. Profile
configuration pins sampled seeds/algorithms and disables golden regeneration.
Missing tests or tools fail rather than producing a successful empty rehearsal.
With `CI` set, prepare `NORN_BIN` and the checker WASM package as documented in the
[authoring SDK](../../tools/norn-ts/README.md); the existing import guard stays strict.
The `ci` profile, its complete test selection and required gates are unchanged.

`jewelry.source.ts` is consumed by the native authored-law and catalog tests;
`constitution.source.ts` is consumed by the authoring mutation/golden test.
The jewelry fragment pins `core.units`; its scope is admission and display,
not the full sell → scrap → melt app. Its reviewed Arabic uses **مُرحَّلة** for
posted. Structured bilingual authoring remains TARGET (#58); the Rust wording
adapter uses the real renderer. The constitution result is bounded runtime
correspondence (77 commits / 66 mutant refusals / 77 restored commits), not a
solver or inductive proof. See each release's actual bounds and omissions.
