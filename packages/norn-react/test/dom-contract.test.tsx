// L6 DOM contract (#1762 milestone C): the renderer declares `agent:redaction` and `agent:confirm`,
// and emits them. A value the server masked is `data-agent-secret`, one it withheld is
// `data-agent-hidden`, and a control that starts a ledgered or consequential action, an action a
// placement asks to confirm, or one whose effects the server has not said, is `data-agent-confirm`
// (it fails closed). Rendered to markup from the admitted fixture page and the stand-in's real
// `NornObjectPage` envelope; nothing here decides what is masked.
import { expect, test } from "bun:test"
import { useState } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import type { ActionEffects } from "../src/agent.ts"
import type { IslandRegistry } from "../src/islands.ts"
import { lazyEngine } from "../src/machine/evaluate.ts"
import type { Machine } from "../src/machine/machine.ts"
import { Node, type PageScope } from "../src/nodes.tsx"
import type { BindingCallState, BindingReadState } from "../src/reads.ts"
import { refusalsOf } from "../src/renderer.tsx"
import type { MachineRuntime } from "../src/runtime.ts"
import { nodesOf, text, type WidgetNode, type WidgetUx } from "../src/ux.ts"
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

interface Variant {
  readonly ux?: WidgetUx
  readonly effects?: (artifactId: string) => ActionEffects | undefined
  readonly islands?: IslandRegistry
}

function scope(stateId: string, reads: ReadonlyMap<string, BindingReadState>, calls: ReadonlyMap<string, BindingCallState> = new Map(), ledgered = true, variant: Variant = {}): PageScope {
  const engine = lazyEngine(() => Promise.reject(new Error("markup only")))
  const runtime: MachineRuntime = { snapshot: { stateId, fields: { note: "" }, entry: 1 }, engine: "idle", evaluate: engine.evaluate, send: () => {}, retryEngine: () => {}, reads, calls, waiting: 0, fault: null }
  const shown = variant.ux ?? ux
  return {
    ux: shown, page: shown.pages[0]!, machine: shown.pages[0]!.machine as Machine, runtime, islands: variant.islands ?? {}, locale: "en", navigate: () => {}, raisedBy: { current: null },
    actionEffects: variant.effects ?? ((artifactId) => (artifactId === ACTION.id ? { irreversible: false, destructive: false, money: [], ledgered } : undefined)),
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

test("agent:confirm fails closed: an action whose effects the server has not said needs a person, at every control that starts it", () => {
  const unsaid = { effects: () => undefined }
  const confirm = renderToStaticMarkup(<Node scope={scope("confirming", ok, new Map(), true, unsaid)} node={node("confirm-node")} />)
  expect(attributes(confirm, "data-agent-confirm")).toEqual([`data-agent-confirm="${ACTION.id}: its effects are not known"`])
  const retry = renderToStaticMarkup(<Node scope={scope("save-failed", ok, new Map(), true, unsaid)} node={node("retry-save")} />)
  expect(attributes(retry, "data-agent-confirm")).toEqual([`data-agent-confirm="${ACTION.id}: its effects are not known"`])
  // A control that starts no action is never marked.
  const save = renderToStaticMarkup(<Node scope={scope("browsing", ok, new Map(), true, unsaid)} node={node("save-node")} />)
  expect(attributes(save, "data-agent-confirm")).toEqual([])
})

/** The fixture with its save placement's confirmation at `severity`. */
const placed = (severity: "confirm" | "destructive"): WidgetUx => ({
  ...ux,
  actionPlacements: ux.actionPlacements.map((placement) => ({ ...placement, confirmation: { ...placement.confirmation, severity } })),
})

test("agent:confirm: a placement that is not `safe` marks every control that starts its binding, not only itself", () => {
  const variant = { ux: placed("destructive") }
  const retry = renderToStaticMarkup(<Node scope={scope("save-failed", ok, new Map(), false, variant)} node={node("retry-save")} />)
  expect(attributes(retry, "data-agent-confirm")).toEqual(['data-agent-confirm="Confirm: destructive"'])
  const confirm = renderToStaticMarkup(<Node scope={scope("confirming", ok, new Map(), false, variant)} node={node("confirm-node")} />)
  expect(attributes(confirm, "data-agent-confirm")).toEqual(['data-agent-confirm="Confirm: destructive"'])
})

/** The fixture's meter island, with its `pick` starting the save call, and its caption read from the page's fields. */
function islandPage(): WidgetUx {
  const page = ux.pages[0]!
  const meter = (child: WidgetNode): WidgetNode => (child.nodeId === "meter-node" && child.component.kind === "catalog" ? { ...child, component: { ...child.component, props: { caption: "input.fields.note" } } } : child)
  return {
    ...ux,
    pages: [{
      ...page,
      root: { ...page.root, children: page.root.children.map(meter) },
      machine: { ...page.machine!, transitions: page.machine!.transitions.map((transition) => (transition.transitionId === "pick" ? { ...transition, target: "saving", effects: [{ kind: "start" as const, call: { kind: "action" as const, bindingId: "save" } }] } : transition)) },
    }],
  }
}

test("agent:confirm: an island that can emit a consequential action carries the mark", () => {
  let drawn = 0
  const islands: IslandRegistry = { "fixture-meter": () => { drawn += 1; return "meter" }, "fixture-status": () => null }
  const fixtureMeter = renderToStaticMarkup(<Node scope={scope("browsing", ok, new Map(), true, { islands })} node={node("meter-node")} />)
  expect(attributes(fixtureMeter, "data-agent-confirm")).toEqual([])
  const variant = { ux: islandPage(), islands }
  const meter = nodesOf(variant.ux.pages[0]!.root).find((candidate) => candidate.nodeId === "meter-node")!
  const marked = renderToStaticMarkup(<Node scope={scope("browsing", ok, new Map(), true, variant)} node={meter} />)
  expect(attributes(marked, "data-agent-confirm")).toEqual([`data-agent-confirm="${ACTION.id}: is recorded in the ledger"`])
  expect(drawn).toBe(1)
})

test("an island is held until every prop evaluates: it never sees a prop its type says is there missing", () => {
  const seen: unknown[] = []
  const islands: IslandRegistry = { "fixture-meter": (input: { readonly props: unknown }) => { seen.push(input.props); return "meter" }, "fixture-status": () => null }
  const variant = { ux: islandPage(), islands }
  const meter = nodesOf(variant.ux.pages[0]!.root).find((candidate) => candidate.nodeId === "meter-node")!
  // The caption reads a field: until the engine is here it cannot evaluate, so the island waits.
  const waiting = renderToStaticMarkup(<Node scope={scope("browsing", ok, new Map(), true, variant)} node={meter} />)
  expect(waiting).toMatch(/data-island="fixture-meter"[^>]*data-engine="waiting"[^>]*aria-busy="true"/)
  expect(seen).toEqual([])
  // A constant caption needs no engine: the island draws with it.
  renderToStaticMarkup(<Node scope={scope("browsing", ok, new Map(), true, { islands })} node={node("meter-node")} />)
  expect(seen).toEqual([{ caption: "Records read" }])
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

test("prototype keys never bypass island refusals, locale text, or masking, and Hook-using islands render as components", () => {
  const protoUx: WidgetUx = {
    ...ux,
    catalog: [{ entryId: "toString", label: { en: "Proto" }, trust: "opaque_island" }],
  }
  expect(refusalsOf(protoUx, {}, true)).toContain("island:toString")
  expect(text({ en: "English" }, "toString")).toBe("English")

  const protoRow = { id: "r1", values: { toString: "literal" }, masked: {} }
  const fieldNode: WidgetNode = { nodeId: "proto-field", component: { kind: "field", fieldId: "f1" }, children: [] }
  const fieldUx: WidgetUx = {
    ...ux,
    fields: [{ fieldId: "f1", label: { en: "Field" }, target: { kind: "property", property: { bindingId: "rec", propertyId: "toString" } } }],
  }
  const okMarkup = renderToStaticMarkup(<Node scope={scope("browsing", new Map([["rec", { kind: "ok", document: protoRow, freshness: "snapshot" }]]), new Map(), false, { ux: fieldUx })} node={fieldNode} />)
  expect(okMarkup).toContain('data-value="plain"')
  expect(okMarkup).not.toContain("data-agent-secret")

  const pendingMarkup = renderToStaticMarkup(<Node scope={scope("browsing", new Map([["rec", { kind: "pending" }]]), new Map(), false, { ux: fieldUx })} node={fieldNode} />)
  expect(pendingMarkup).toContain('data-read="pending"')

  function HookIsland(input: { readonly props: Record<string, unknown> }) {
    const [tag] = useState("hooked")
    return <span>{`${tag}:${String(input.props.caption ?? "")}`}</span>
  }
  const islands: IslandRegistry = { "fixture-meter": HookIsland, "fixture-status": () => null }
  const hookMarkup = renderToStaticMarkup(<Node scope={scope("browsing", ok, new Map(), false, { islands })} node={node("meter-node")} />)
  expect(hookMarkup).toContain("hooked:Records read")
})
