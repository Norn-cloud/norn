# norn

`norn check --artifacts artifacts.json --context context.json` runs the same
checker ABI used by TypeScript authoring and returns its admission report.
`norn openapi` emits the generated core wire contract. Embed `norn-cell` for
transactional cell execution; the public CLI has no product server or adapters.

`norn eval --defs DIR --org ID --env ID --rule ARTIFACT_ID` admits a standalone
rule set and evaluates JSON-lines `{slice,event}` inputs through `norn-decisions`.
It emits candidate effects as data under a bounded operation budget.
