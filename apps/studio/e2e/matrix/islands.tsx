/**
 * The fixture page's catalog islands, one per tier, typed by the registry `norn codegen --ts` emits
 * for the fixture widget (`packages/norn-react/test/fixtures/matrix-islands.d.ts`): a missing island,
 * an extra one, or one whose props or events differ from its entry does not compile.
 *
 * - `fixture-meter` (Tier 1, presentational): its caption prop and a `pick` event, nothing else.
 * - `fixture-status` (Tier 2, stateful): the page's server states, drawn through `matchReadState`
 *   and `matchActionState`, so a state it left out would not compile.
 */
import { Button } from "@ui-sdk/react/button"
import { matchActionState, matchReadState, type IslandStates } from "../../../../packages/norn-react/src/index.ts"
import type { RequiredCatalogRegistry } from "../../../../packages/norn-react/test/fixtures/matrix-islands.d.ts"

const MUTED = "text-meta text-muted-foreground"

export const matrixIslands = {
  "fixture-meter": ({ props, emit }) => (
    <div className="flex items-center gap-2">
      <span className={MUTED}>{props.caption}</span>
      <Button className="inline-flex h-8 items-center rounded-full border border-border px-3 text-meta" onClick={() => emit("pick", {})}>Pick</Button>
    </div>
  ),
  "fixture-status": ({ states }) => {
    const read = states.reads.records
    const call = states.calls.save
    const reading = read === undefined ? "not read" : matchReadState(read, {
      pending: () => "reading",
      ok: ({ freshness }) => `read (${freshness})`,
      refused: ({ refusal }) => `refused (${refusal.code})`,
      not_served: () => "not served",
      unavailable: () => "unavailable",
      answer_moved: () => "moved",
      timed_out: () => "timed out",
    })
    const calling = call === undefined ? "no call" : matchActionState(call, {
      editing: () => "editing",
      confirming: () => "confirming",
      sending: () => "sending",
      pending_approval: () => "waiting for approval",
      committed: () => "committed",
      refused: ({ refusal }) => `refused (${refusal.code})`,
      not_served: () => "not served",
      unavailable: () => "not applied",
      timed_out: () => "outcome unknown",
      undoing: () => "undoing",
      undo_failed: () => "undo failed",
      undone: () => "undone",
    })
    return <p className={MUTED} data-island-read={read?.kind ?? "none"} data-island-call={call?.kind ?? "none"}>{`Records: ${reading}. Save: ${calling}.`}</p>
  },
} satisfies RequiredCatalogRegistry<"fixture.matrix.widget", IslandStates>
