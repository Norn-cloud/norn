/**
 * Studio's record page as a server-held Widget UX document (#1762 milestone C2): the first page
 * Studio draws with `<NornAppRenderer>` from a definition the cell admitted and serves.
 *
 * One page serves every object type: its context names the record (`objectType`, `id`, from the
 * route), and its one read is a context read of that record (UI-CONTEXT-READ-1), its links and its
 * lineage, which the cell decides. The page machine owns the read's every outcome:
 *
 * - `loading` and `refreshing` wait for the read, each with a deadline, and handle every outcome a
 *   read can have (L2): the record (`showing`), a record the cell does not serve to this reader
 *   (`absent`: not served, or refused as not visible), and everything else (`failed`).
 * - `failed` offers a person's Retry (L4); `showing` reads again on a person's Read again, keeping
 *   the record on screen while it does (`refreshing`); `absent` reads again after a correction.
 *
 * What the page shows in each state is a Tier 2 catalog island (UI-CATALOG-1), held in the states
 * it is drawn in: it receives the read's closed state and draws it through `matchReadState`. The
 * islands are Studio's (`apps/studio/src/record/islands.tsx`, typed by the generated
 * `RequiredCatalogRegistry`); nothing in them decides a read, a refusal or a permission.
 *
 * Every text is a source message (`msg`): Studio draws it through its translations.
 *
 * `bun test registry/studio` (tools/norn-ts `studio-pages.test.ts`) lowers this file and holds the
 * committed artifact to it; `NORN_PACKS_UPDATE=1` writes it.
 */
import * as W from "../../tools/norn-ts/src/builder/widget.ts"

const { msg } = W

export const RECORD_PAGE_ID = "norn.studio.record.widget"
export const RECORD_PAGE_VERSION = "0.1.0"

/** The record read's outcomes, each to the state it leaves the page in. */
const outcomes = [
  ["accepted", "showing"],
  ["hidden", "absent"],
  ["not-served", "absent"],
  ["refused", "failed"],
  ["network-failure", "failed"],
  ["timeout", "failed"],
  ["answer-moved", "failed"],
] as const

const read = { kind: "start", call: { kind: "read", bindingId: "record" } } as const

/** A Tier 2 island held in `states`: what the page shows there. */
const island = (nodeId: string, heldIn: ReadonlyArray<string>): W.WidgetNode => ({
  nodeId,
  component: { kind: "catalog", entry: { entryId: nodeId }, heldIn },
})

/** A catalog entry for a Studio island that draws the page's server states. */
const entry = (entryId: string, label: W.Message, events: ReadonlyArray<string> = []) => ({
  entryId,
  label,
  trust: "contract_checked",
  framework: "react",
  ...(events.length === 0 ? {} : { events: events.map((eventId) => ({ eventId })) }),
})

const user = (eventId: string, nodeId: string, emits: string): W.MachineEvent => ({ eventId, source: { kind: "user", nodeId, emits } })

export const recordPage: W.WidgetDef = W.widget({
  id: RECORD_PAGE_ID,
  title: "Record",
  version: RECORD_PAGE_VERSION,
  pages: [{
    pageId: "record",
    name: "Record",
    contextParams: [
      { paramId: "objectType", type: "text", required: true },
      { paramId: "id", type: "text", required: true },
    ],
    root: {
      nodeId: "record",
      component: { kind: "section", section: { sectionId: "record", kind: "section", label: msg("Record"), fields: [], order: 0 } },
      children: [
        island("record-reading", ["loading"]),
        island("record-failed", ["failed"]),
        island("record-absent", ["absent"]),
        island("record-header", ["showing", "refreshing"]),
        island("record-body", ["showing", "refreshing"]),
      ],
    },
    machine: {
      initialStateId: "opening",
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        user("retry", "record-failed", "retry"),
        user("read-again", "record-header", "readAgain"),
        user("corrected", "record-body", "readAgain"),
        user("released", "record-absent", "readAgain"),
        W.outcome("accepted", "accepted"),
        W.outcome("hidden", "refused", { category: "visibility" }),
        W.outcome("not-served", "not_served"),
        W.outcome("refused", "refused"),
        W.outcome("network-failure", "network_failure"),
        W.outcome("timeout", "timeout"),
        W.outcome("answer-moved", "answer_moved"),
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "record" }, deadlineMs: 15000 },
        { stateId: "refreshing", role: "pending", awaits: { kind: "read", bindingId: "record" }, deadlineMs: 15000 },
        { stateId: "showing", role: "idle" },
        { stateId: "absent", role: "idle" },
        { stateId: "failed", role: "error" },
      ],
      transitions: [
        { transitionId: "open", from: "opening", on: "opened", target: "loading", effects: [read] },
        ...(["loading", "refreshing"] as const).flatMap((from) =>
          outcomes.map(([on, target]) => ({ transitionId: `${from}-${on}`, from, on, target }))),
        { transitionId: "retry", from: "failed", on: "retry", target: "loading", effects: [read] },
        { transitionId: "read-again", from: "showing", on: "read-again", target: "refreshing", effects: [read] },
        { transitionId: "corrected", from: "showing", on: "corrected", target: "refreshing", effects: [read] },
        { transitionId: "released", from: "absent", on: "released", target: "loading", effects: [read] },
      ],
    },
  }],
  contextReads: [{ bindingId: "record", pageId: "record", typeParameterId: "objectType", keyParameterId: "id" }],
  contract: {
    catalog: [
      entry("record-reading", msg("Reading the record")),
      entry("record-failed", msg("The record could not be read"), ["retry"]),
      entry("record-absent", msg("Record not available"), ["readAgain"]),
      entry("record-header", msg("The record's name, state and actions"), ["readAgain"]),
      entry("record-body", msg("The record's facts, story, lineage and rules"), ["readAgain"]),
    ],
  },
})

/** The page's admitted artifact, as the demo workspace's definitions carry it. */
export const recordPageArtifact = () => W.lowerWidget(recordPage, { scope: {}, sourcePath: "registry/studio/record-page.ts", registryPackagePath: "registry/studio" })
