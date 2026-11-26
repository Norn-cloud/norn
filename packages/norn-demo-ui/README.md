# Demo UI

Framework-free UI pieces that Studio (`apps/studio`) imports by path. They run
a page's declared UI machine with `@norn/react`'s runtime
(`packages/norn-react/src/machine/`). None of
them fetches on its own, decides permission or computes data: each one renders
or drives what the server's admitted documents declare.

- `src/elements/decision-sheet.ts`: `<norn-decision-sheet>`, one decision
  rendered from its admitted page document and driven only by its machine.
- `src/elements/lifecycle-view.ts`: `<norn-lifecycle-view>` (UI-LIFECYCLE-2),
  an object type's lifecycle with the cell's live count per state, plus the
  checker's counterexamples.
- `src/catalog-host/`: the sandboxed host for a catalog component
  (UI-CATALOG-1), and the compiler from a Foldkit component's machine to
  Norn's machine IR.

Run `bun install --frozen-lockfile --ignore-scripts --backend copyfile`,
`bun run typecheck` and `bun test` from this package on Boat.
