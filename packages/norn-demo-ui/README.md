# Demo UI

The first independent slice of #218 is a read-only definitions inspector.
`renderDefinitionsDocument({ entries })` returns a self-contained HTML document.
Each entry contains metadata typed from the generated SDK and the exact JSON
source returned by an authorized server read. An optional `definition` carries
the admitted kind, payload and dependencies from that read; it enables the
structured view without parsing or rewriting exact source. The caller supplies these from
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

Presentation contract: D4 preserves server order within kind groups. D5 retains
property types, units, links and invariants. D6 displays only declared states and
transitions, joining action labels by entity identity, version, lifecycle and
transition; missing action declarations stay unresolved. D7 displays inputs,
preconditions, bound lifecycle guards, integrity invariants and referenced
definitions without evaluating availability. D8 shows rule targets, expressions
and outputs without inventing business meaning. D1–D3 exact source, escaping and
empty-document guarantees still apply. Source remains a secondary disclosure.
