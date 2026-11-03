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
- `renderDefinitionsDocument({ entries })`: the source inspector. Each entry is
  metadata typed from the generated SDK plus the exact JSON source returned by
  the same read; source bytes are preserved as escaped text, including numeric
  literals outside JavaScript's precision.

Run `bun install --frozen-lockfile --ignore-scripts --backend copyfile`,
`bun run typecheck` and `bun test` from this package on Boat. The tests render
the compiled registry and law fixtures, not hand-written definitions.
