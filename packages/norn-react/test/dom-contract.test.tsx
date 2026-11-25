// L6 DOM contract (#1762 milestone C): the renderer declares `agent:redaction` and `agent:confirm`,
// and emits them. A value the server masked is `data-agent-secret`, one it withheld is
// `data-agent-hidden`, and a control that starts a ledgered or consequential action is
// `data-agent-confirm`. Rendered to markup from the admitted fixture page and the stand-in's
// real `NornObjectPage` envelope; nothing here decides what is masked.
import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { lazyEngine } from "../src/machine/evaluate.ts"
import type { Machine } from "../src/machine/machine.ts"
import { Node, type PageScope } from "../src/nodes.tsx"
import type { BindingCallState, BindingReadState } from "../src/reads.ts"
import type { MachineRuntime } from "../src/runtime.ts"
import { nodesOf, type WidgetUx } from "../src/ux.ts"
import { ACTION, matrixArtifact } from "./fixtures/matrix-page.ts"

const ux = matrixArtifact().payload.ux as WidgetUx
const page = ux.pages[0]!
const machine = page.machine as Machine
const records = {
  objectType: "canon.ValuationPolicy",
  records: [
    { id: "vp-1", values: { id: "vp-1", label: "•••", valuation_method: "fifo" }, masked: { label: { mode: "redacted", markings: ["confidential"], revealable: false } } },
    { id: "vp-2", values: { id: "vp-2", label: "Moving average" }, masked: { valuation_method: { mode: "refuse", markings: ["restricted"], revealable: false } } },
    { id: "vp-3", values: null, withheld: true, withheldProperties: [] },
  ],
}

function scope(stateId: string, reads: ReadonlyMap<string, BindingReadState>, calls: ReadonlyMap<string, BindingCallState> = new Map(), ledgered = true): PageScope {
  const engine = lazyEngine(() => Promise.reject(new Error("markup only")))
  const runtime: MachineRuntime = { snapshot: { stateId, fields: { note: "" }, entry: 1 }, engine: "idle", evaluate: engine.evaluate, send: () => {}, reads, calls }
  return {
    ux, page, machine, runtime, islands: {}, locale: "en", navigate: () => {}, raisedBy: { current: null },
    actionEffects: (artifactId) => (artifactId === ACTION.id ? { irreversible: false, destructive: false, money: [], ledgered } : undefined),
  }
}
const node = (nodeId: string) => nodesOf(page.root).find((candidate) => candidate.nodeId === nodeId)!
const ok = new Map<string, BindingReadState>([["records", { kind: "ok", document: records, freshness: "snapshot" }]])
const attributes = (markup: string, attribute: string) => markup.match(new RegExp(`${attribute}="[^"]*"`, "g")) ?? []

test("agent:redaction: a masked value is secret, a withheld value or row hidden, a plain one unmarked", () => {
  const markup = renderToStaticMarkup(<Node scope={scope("browsing", ok)} node={node("records-list")} />)
  expect(attributes(markup, "data-agent-secret")).toHaveLength(1)
  expect(markup).toMatch(/<span data-agent-secret="" data-value="masked" data-mask="redacted">•••<\/span>/)
  // The refused value (mode `refuse`) and the withheld row: never in the clear, hidden from the agent.
  expect(markup).toMatch(/<span data-agent-hidden="" [^>]*data-value="withheld">Withheld<\/span>/)
  expect(markup).toMatch(/<tr data-agent-hidden=""/)
  expect(markup).toContain('data-value="plain"')
})

test("agent:confirm: the control that starts a ledgered action needs a person; the others do not", () => {
  const confirm = renderToStaticMarkup(<Node scope={scope("confirming", ok)} node={node("confirm-node")} />)
  expect(attributes(confirm, "data-agent-confirm")).toEqual([`data-agent-confirm="${ACTION.id}: is recorded in the ledger"`])
  const save = renderToStaticMarkup(<Node scope={scope("browsing", ok)} node={node("save-node")} />)
  expect(attributes(save, "data-agent-confirm")).toEqual([])
  // The mark is the server's answer: an action it does not call ledgered or consequential is unmarked.
  const plain = renderToStaticMarkup(<Node scope={scope("confirming", ok, new Map(), false)} node={node("confirm-node")} />)
  expect(attributes(plain, "data-agent-confirm")).toEqual([])
})

test("every read state draws through matchReadState, and a call's state through matchActionState", () => {
  const states: BindingReadState[] = [
    { kind: "pending" },
    { kind: "refused", refusal: { code: "authz.denied", category: "permission", document: { message: "You can't read these." } } },
    { kind: "not_served", error: new Error("Not served here.") },
    { kind: "unavailable", error: new Error("No answer."), retry: true },
    { kind: "answer_moved", error: new Error("Moved.") },
    { kind: "timed_out", error: new Error("Too slow.") },
    { kind: "ok", document: records, freshness: "stale" },
  ]
  const drawn = states.map((state) => renderToStaticMarkup(<Node scope={scope("browsing", new Map([["records", state]]))} node={node("records-list")} />))
  expect(drawn.map((markup) => /data-read="([a-z_]+)"/.exec(markup)?.[1])).toEqual(["pending", "refused", "not_served", "unavailable", "answer_moved", "timed_out", "ok"])
  expect(drawn[1]).toContain("You can&#x27;t read these.")
  expect(drawn[6]).toContain('data-freshness="stale"')
})

test("a control is enabled exactly when the current state can take its event", () => {
  const browsing = renderToStaticMarkup(<Node scope={scope("browsing", ok)} node={node("save-node")} />)
  const saving = renderToStaticMarkup(<Node scope={scope("saving", ok)} node={node("save-node")} />)
  // Disabled but focusable (`aria-disabled`), so focus never falls out of a waiting layer.
  expect(browsing).not.toMatch(/aria-disabled="true"/)
  expect(saving).toMatch(/aria-disabled="true"/)
})
