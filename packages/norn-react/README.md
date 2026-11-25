# @norn/react

The React renderer of admitted Widget UX documents (#1762 milestone C). Studio
(`apps/studio`) imports it by path.

- `src/renderer.tsx`: `<NornAppRenderer>` renders one page of an admitted app in
  the workspace the server decided, or refuses the page whole, saying which
  renderer capability, binding form or catalog island it lacks (L6).
- `src/runtime.ts`: `useWidgetMachine` runs the page's `WidgetMachine`.
  Guard-free transitions run in TypeScript at once; ZEN guards and assignments
  run in the checker's engine (`norn-checker-wasm`), loaded lazily after the
  first paint.
- `src/machine/`: the framework-free machine runtime (UI-MACHINE-1): `update`,
  the driver, outcome classification (`not_served` for a route the host does
  not serve), and `walks` for model-based tests.
- `src/state.ts`: the closed UI states and their exhaustive `match*`.
- `src/nodes.tsx`: the component tree, with UI SDK layers, controls as UI SDK
  buttons, and Tier 1 / Tier 2 catalog islands.
- `src/agent.ts`: the page-agent marks (`data-agent-secret`, `-hidden`,
  `-confirm`); server-side redaction stays the only secrecy authority.

Nothing here decides permission: every read and call is the server's.

Run `bun install --frozen-lockfile --ignore-scripts --backend copyfile`,
`bun run typecheck` and `bun test` from this package on Boat. Set
`NORN_CHECKER_WASM_PKG` to the checker's WASM package to run the parity rows.
