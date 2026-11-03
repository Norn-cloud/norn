# Drop 2: Authored laws, checked and rendered

Law-drop candidate; publication requires a passing main Boat rehearsal.

Run `bash scripts/demo/drop-02-authored-laws.sh` from a clean checkout contained
in freshly fetched `origin/main`, with Rust and Bun installed and a frozen
`bun install --frozen-lockfile --backend copyfile` in `tools/norn-ts`.

Shows one `Norn.law` in `jewelry.source.ts`, actual checker admission, generated
source-bound obligations, protected-law/invalid-member refusals, and real EN/AR
views of that same law. Arabic posted is **مُرحَّلة**. The structured bilingual
authoring adapter remains Rust TARGET/#58 until the TS profile supports it.

Requires #359, #366, #322 and the renderer bridge. Missing capabilities print
`not yet` and exit 1; unexpected failures also stop. This is admission and
rendering, not a sell/scrap/melt transaction, aggregate enforcement, solver
counterexample or checked proof. Locale is display data, not a second law.
