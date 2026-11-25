/**
 * #1762 milestone C: a renderer's islands are the registry `norn codegen --ts` emits for the widget
 * (`fixtures/matrix-islands.d.ts`), or the page does not compile. `bun run typecheck` checks every
 * `@ts-expect-error` row: if the registry ever accepted a missing, extra or mismatched island, the
 * directive would be unused and the typecheck would fail.
 */
import type { IslandStates } from "../src/islands.ts"
import { matchReadState } from "../src/state.ts"
import type { RequiredCatalogRegistry } from "./fixtures/matrix-islands.d.ts"

type Registry = RequiredCatalogRegistry<"fixture.matrix.widget", IslandStates>

const meter = ({ props, emit }: { readonly props: { readonly caption: string }; readonly emit: (event: "pick", payload: Readonly<Record<string, never>>) => void }) => {
  emit("pick", {})
  return props.caption
}
const status = ({ states }: { readonly states: IslandStates }) => {
  const read = states.reads.records
  return read === undefined ? "none" : matchReadState(read, {
    pending: () => "pending", ok: () => "ok", refused: () => "refused", not_served: () => "not served",
    unavailable: () => "unavailable", answer_moved: () => "moved", timed_out: () => "timed out",
  })
}

export const complete: Registry = { "fixture-meter": meter, "fixture-status": status }

// @ts-expect-error a declared island is missing
export const missing: Registry = { "fixture-meter": meter }

// @ts-expect-error an island the widget does not declare
export const extra: Registry = { "fixture-meter": meter, "fixture-status": status, "fixture-other": meter }

export const mismatchedProps: Registry = {
  // @ts-expect-error the meter's caption is text, not a number
  "fixture-meter": ({ props }: { readonly props: { readonly caption: number } }) => props.caption,
  "fixture-status": status,
}

export const undeclaredEvent: Registry = {
  // @ts-expect-error the meter declares `pick`, never `drop`
  "fixture-meter": ({ emit }: { readonly emit: (event: "drop", payload: Readonly<Record<string, never>>) => void }) => emit("drop", {}),
  "fixture-status": status,
}

export const statesInPresentational: Registry = {
  // @ts-expect-error a presentational island never receives the page's server states
  "fixture-meter": ({ states }: { readonly states: IslandStates }) => states,
  "fixture-status": status,
}
