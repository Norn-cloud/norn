# Demo UI

The first independent slice of #218 is a read-only definitions inspector.
`renderDefinitionsDocument({ entries })` returns a self-contained HTML document.
Each entry contains metadata typed from the generated SDK and the exact JSON
source returned by an authorized server read. The caller must supply both from
the same read. The inspector preserves source bytes as escaped text, including
decimal and integer literals outside JavaScript's numeric precision.

It does not fetch definitions, admit artifacts, infer permissions, execute
actions or claim that supplied definitions have been installed. Do not pass an
unfiltered registry dump in place of the server's authorized document.

Run `bun install --frozen-lockfile --ignore-scripts --backend copyfile`,
`bun run typecheck` and `bun test` from this package on Boat.

The operational shell remains dependent on the public host/binding export and
the admitted alpha fixture. It will use the existing WidgetIR renderer for
lists, forms and actions. The five-beat fresh-cell demo also needs executable
stock, jewelry and evidence adapters; this inspector is not that demo.
