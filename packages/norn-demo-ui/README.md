# Demo UI

The one renderer for the #218 demo shell: pure functions from authorized
server documents to self-contained HTML. They don't fetch, admit, authorize,
execute or infer. Whatever supplies a document (today a caller of the
server's generic routes; later a client-cell runtime) must pass the exact
authorized read, never an unfiltered registry dump. Every page states what the
read says about its frontier and freshness, and nothing more.

- `renderShell` (`src/shell.ts`): the shared frame. No script and no remote
  resources; light and dark themes follow the platform preference; responsive.
- `renderCatalogDocument({ artifacts })` (`@norn/demo-ui/catalog`): concepts,
  their declared `extends`/`implements` graph, relations, laws and value types
  over `NornArtifactEnvelope[]`. It shows only what each artifact declares, in
  server order. Inheritance, effective laws, the inherited origin chain and the
  reviewed EN/AR wording are server projections; until the document carries
  them the page says "not exposed by the server" rather than computing them.
- `renderRefusalDocument({ refusal, source })` (`@norn/demo-ui/refusal`): "why
  was this refused?" for one `norn/refusal@2` action response. An action's
  status is a commitment: submitted, then accepted with its receipt or refused
  with its typed reason and no effects (TXN-ATOMIC-1). This page renders the
  refused state; the pending and accepted states arrive with the action forms
  and the server's action route types. Operand values come from the exact
  response text, so decimals keep their scale. A reason the server does not
  disclose (DIAG-REDACT-1) and the repair hint stay absent; categories are shown
  as the server sends them. A source that is not the same response refuses to
  render.
- `renderQuestionBoard({ questions })` (`@norn/demo-ui/questions`): the data
  app's question board. Each card pairs a typed question (`NornIrQuestion`)
  with the cell's exact answer (`NornQueryResult`, QUERY-ENGINE-1). It shows
  every bucket's value beside its numerator and denominator, the answer's
  freshness, its knowledge revision and source frontier, and any quarantined
  inputs that make it incomplete. Each value drills through to the object
  versions it sums. With an answer at an earlier knowledge cutoff
  (`NornHistoricalQuestionResult`, ASOF-KNOWLEDGE-1), the card lists the
  buckets whose answer was restated. Decimal text comes from the read's own
  bytes. Source rows behind an object version are not exposed by the server
  yet (#776).
- `renderDefinitionsDocument({ entries })`: the source inspector. Each entry is
  metadata typed from the generated SDK plus the exact JSON source returned by
  the same read; source bytes are preserved as escaped text, including numeric
  literals outside JavaScript's precision.

Run `bun install --frozen-lockfile --ignore-scripts --backend copyfile`,
`bun run typecheck` and `bun test` from this package on Boat. The tests render
the compiled registry and law fixtures and the cell evaluator's retained
answers (`fixtures/demo/answers`), not hand-written definitions.
