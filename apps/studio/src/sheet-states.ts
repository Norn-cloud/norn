/**
 * The canvas contact sheet's states: every state of every canvas component,
 * each with the decision it records. `canvas.graph.mdx` (atlas) and
 * `sheet.html` (the real components, rendered by Studio's own build) both
 * read this list, so the plan and the specimens cannot drift apart.
 */
export interface SheetState {
  readonly component: string
  readonly id: string
  readonly note: string
}

export const SHEET: readonly SheetState[] = [
  { component: "StateBox", id: "state/default", note: "Baseline: a light card floating on the dot grid (contact plus ambient shadow, hairline border, top highlight); kind chip, kind line, name." },
  { component: "StateBox", id: "state/flags", note: "Flags replace the kind line (initial · terminal), muted; no badges, so geometry holds." },
  { component: "StateBox", id: "state/selected", note: "Selection is a crisp accent ring offset from the card, plus the hover depth; never a fill or border change." },
  { component: "StateBox", id: "state/problem", note: "A finding is a destructive ring and a counted pill; the message is on hover and in Problems." },
  { component: "StateBox", id: "nasty/long-name", note: "A long state name wraps inside the fixed width rather than widening the node." },
  { component: "NodeCard", id: "kinds/all", note: "One card per kind with its tinted chip (Tool, Ask a person, Route, Wait, Outcome, Agent, Law): tints differ in hue only, at the same weight." },
  { component: "CanvasFrame", id: "frame/default", note: "A floating toolbar grouped add · edit · save on the canvas, the dot grid, and the Inspector/Problems rail; Save to server disabled with its reason." },
  { component: "CanvasFrame", id: "frame/findings", note: "New findings badge in the header; Problems lists each with where it is, click jumps to it." },
  { component: "CanvasFrame", id: "frame/empty", note: "An empty definition says what to do in the canvas itself; the toolbar still adds." },
  { component: "CanvasFrame", id: "frame/checker-loading", note: "Before the checker loads, the header says so; nothing is shown as clean." },
  { component: "CanvasFrame", id: "frame/checker-error", note: "Without the checker the header says diagnostics are off, in destructive text." },
  { component: "CanvasFrame", id: "frame/dirty", note: "Unsaved changes badge, Discard draft, and the restored-draft notice after a reload." },
  { component: "CanvasFrame", id: "frame/refusal", note: "A refused edit (typed port mismatch, duplicate name) is an alert line under the toolbar." },
  { component: "CanvasFrame", id: "frame/multi-select", note: "Several selected: the Inspector counts them and offers Delete selected." },
  { component: "StepBox", id: "step/tool", note: "Tinted Tool chip, kind line, name, tool id as the muted subtitle; one pill exit on the right." },
  { component: "StepBox", id: "step/route", note: "A route has one exit per branch, then otherwise, then next, spaced evenly down the right edge." },
  { component: "StepBox", id: "step/start", note: "The start has no entry and cannot be deleted or copied." },
  { component: "StepBox", id: "step/problem", note: "Same destructive ring and counted pill as a state, so findings read alike on both canvases." },
  { component: "AutomationFrame", id: "automation/default", note: "The example automation lowers and checks clean; Run in the cell is disabled with its reason." },
  { component: "AutomationFrame", id: "automation/route-selected", note: "A selected route lists its branches with reorder and delete; its connections take the accent and a slow flow dash (still under reduced motion)." },
  { component: "AutomationFrame", id: "automation/tool-selected", note: "A selected tool step shows one typed field per input its schema declares." },
  { component: "StepBox", id: "step/ask", note: "Ask a person: one exit per option, then next; the option count is the detail line." },
  { component: "DecisionTree", id: "decision/default", note: "An elevated question card (who asks, risk, decided by, expiry) on a spine of numbered option cards; the agent's pick is marked, never chosen." },
  { component: "DecisionTree", id: "decision/path", note: "Choosing opens the follow-up; Then 1 – 2 sums the path, and later steps say they are pre-answered and rechecked when reached." },
  { component: "AutomationFrame", id: "automation/ask-selected", note: "A selected ask step edits question, risk, deciders and options, and previews the decision under the canvas." },
  { component: "CanvasPage", id: "page/loading", note: "Reading definitions is a status line in the panel, not a blank page." },
  { component: "CanvasPage", id: "page/error", note: "A failed read names the error in an alert." },
  { component: "CanvasPage", id: "page/no-lifecycles", note: "No lifecycle declared: the shared empty state." },
  { component: "FactsTable", id: "locale/facts", note: "Facts and enum values in the reader's language from a labels artifact; a fact without a label keeps its id in words, marked lang=en; Technical ids isolated." },
  { component: "CodedMessage", id: "locale/message", note: "A coded checker message from the catalog; artifact ids, versions and a +20 phone number stay isolated inside Arabic text." },
  { component: "CanvasFrame", id: "locale/problems", note: "Problems list in the reader's language; codes stay as sent." },
]
