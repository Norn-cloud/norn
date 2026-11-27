// Vocab v2 coverage: Tabs (`WidgetSectionKind::Tab`), Selectable Lists (`single` | `multiple`),
// `component:dialog` (L1 blocking + dismissalMethods + @ui-sdk/react/dialog), and `component:form`
// (all WidgetFieldControl kinds, exact decimal strings, dirty tracking, duplicate-submit guard,
// server refusal categories, zero client permission logic, zero Radix).
import { describe, expect, test } from "bun:test"
import { dialog as dialogPrimitive } from "@ui-sdk/core/dialog"
import { tabs as tabsPrimitive } from "@ui-sdk/core/tabs"
import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { formConfirmReason, type ActionEffects } from "../src/agent.ts"
import { missingCapabilities, referencesRecordVersion, RENDERED_COMPONENTS, RENDERER_CAPABILITIES, requiredCapabilities } from "../src/capabilities.ts"
import { lazyEngine, type CheckerEvaluator } from "../src/machine/evaluate.ts"
import type { Classified, Clock } from "../src/machine/index.ts"
import type { Fields, Machine, MachineEvent } from "../src/machine/machine.ts"
import {
  buildFormDraftPayload,
  canDismissDialog,
  canDispatchFormConfirmation,
  computeFormDirty,
  envelopeRowId,
  evaluateFieldCondition,
  flattenSelectionRecord,
  formatRefusalCategory,
  isDialogBlocked,
  mergePrefillSecurity,
  nextListRowIndex,
  nextSelection,
  nextTabIndex,
  Node,
  parseExactInteger,
  partitionTabRuns,
  readPrefill,
  reconcileSelection,
  resolveFieldPrefillSecurity,
  selectionEventPayload,
  validateExactNumber,
  type PageScope,
} from "../src/nodes.tsx"
import type { BindingCallState, BindingReadState, WidgetCalls, WidgetReader } from "../src/reads.ts"
import { NornAppRenderer } from "../src/renderer.tsx"
import { createPageRuntime, DEFAULT_SUBMIT_DEADLINE_MS, mergeReadSecurity, needsEngine, securitySince, type MachineRuntime } from "../src/runtime.ts"
import type { DismissalMethod, FieldPresentation, WidgetForm, WidgetNode, WidgetUx } from "../src/ux.ts"

const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

function manualClock(): Clock & { advance(ms: number): void } {
  let now = 0
  let next = 0
  const pending = new Map<number, { at: number; callback: () => void }>()
  return {
    setTimeout(callback, ms) {
      next += 1
      pending.set(next, { at: now + ms, callback })
      return next
    },
    clearTimeout(handle) {
      pending.delete(handle as number)
    },
    advance(ms) {
      now += ms
      for (const [handle, timer] of [...pending].sort((a, b) => a[1].at - b[1].at)) {
        if (timer.at <= now && pending.delete(handle)) timer.callback()
      }
    },
  }
}

/** Exact slot allowlist mirrored from `crates/norn-checker-wasm/src/evaluate.rs:parse_expression_inputs`. */
const ALLOWED_WASM_SLOTS = new Set(["record", "recordKey", "row", "draft", "selection", "context", "fields", "event"])

const pathEngine: CheckerEvaluator = {
  evaluateExpression(expression, inputJson) {
    const slots = JSON.parse(inputJson) as Record<string, unknown>
    if (Object.hasOwn(slots, "recordKey") && !Object.hasOwn(slots, "record")) {
      return JSON.stringify({ ok: false, error: { code: "expr.input.invalid", message: "recordKey requires record" } })
    }
    for (const [name, slotValue] of Object.entries(slots)) {
      if (!ALLOWED_WASM_SLOTS.has(name)) {
        return JSON.stringify({ ok: false, error: { code: "expr.input.invalid", message: `unknown WidgetIR input slot "${name}"` } })
      }
      if (name === "recordKey" && (typeof slotValue !== "string" || slotValue === "")) {
        return JSON.stringify({ ok: false, error: { code: "expr.input.invalid", message: "recordKey must be a nonempty string" } })
      }
    }
    const source = expression.trim()
    const resolvePath = (path: string): unknown => {
      let value: unknown = slots
      const tokens = path.slice("input".length).match(/\.[A-Za-z_][A-Za-z0-9_]*|\[\d+\]/g) ?? []
      for (const token of tokens) {
        if (token.startsWith(".")) {
          const part = token.slice(1)
          value = typeof value === "object" && value !== null ? (value as Record<string, unknown>)[part] : undefined
        } else {
          const index = Number(token.slice(1, -1))
          value = Array.isArray(value) ? value[index] : undefined
        }
      }
      return value
    }
    const eqMatch = /^(input(?:\.[A-Za-z_][A-Za-z0-9_]*|\[\d+\])+)\s*==\s*("[^"]*")$/.exec(source)
    if (eqMatch !== null) {
      const actual = resolvePath(eqMatch[1]!)
      const expected = JSON.parse(eqMatch[2]!) as string
      return JSON.stringify({ ok: true, value: actual === expected })
    }
    if (/^input(\.[A-Za-z_][A-Za-z0-9_]*|\[\d+\])+$/.test(source)) {
      const value = resolvePath(source)
      return JSON.stringify(value === undefined ? { ok: false, error: { code: "unknown_field", message: `${source} names nothing.` } } : { ok: true, value })
    }
    try {
      return JSON.stringify({ ok: true, value: JSON.parse(source) as unknown })
    } catch {
      return JSON.stringify({ ok: false, error: { code: "parse", message: `${source} is not supported.` } })
    }
  },
}

const baseMachine: Machine = {
  machineVersion: "0.1",
  initialStateId: "idle",
  fields: [],
  events: [
    { eventId: "opened", source: { kind: "page_opened" } },
    { eventId: "row-picked", source: { kind: "user", nodeId: "selectable-list" }, payload: [{ fieldId: "id", type: "text", required: false }] },
    { eventId: "rows-picked", source: { kind: "user", nodeId: "multi-list" }, payload: [{ fieldId: "ids", type: "json", required: true }, { fieldId: "count", type: "integer", required: true }] },
    { eventId: "save-form", source: { kind: "user", nodeId: "order-form" } },
    { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
    { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
    { eventId: "network_failure", source: { kind: "outcome", outcome: "network_failure" } },
    { eventId: "not_served", source: { kind: "outcome", outcome: "not_served" } },
    { eventId: "timeout", source: { kind: "outcome", outcome: "timeout" } },
  ],
  states: [
    { stateId: "idle", role: "idle" },
    { stateId: "saving", role: "pending", awaits: { kind: "action", bindingId: "submit-order" }, deadlineMs: 10_000 },
    { stateId: "saved", role: "idle" },
    { stateId: "failed", role: "error" },
  ],
  transitions: [
    { transitionId: "t-save", from: "idle", on: "save-form", target: "saving", effects: [{ kind: "start", call: { kind: "action", bindingId: "submit-order" } }] },
    { transitionId: "t-ok", from: "saving", on: "accepted", target: "saved" },
    { transitionId: "t-refused", from: "saving", on: "refused", target: "failed" },
    { transitionId: "t-net", from: "saving", on: "network_failure", target: "failed" },
    { transitionId: "t-ns", from: "saving", on: "not_served", target: "failed" },
    { transitionId: "t-to", from: "saving", on: "timeout", target: "failed" },
  ],
}

function makeUx(root: WidgetNode, extra: Partial<WidgetUx> = {}, machine: Machine = baseMachine): WidgetUx {
  return {
    uxVersion: "0.1",
    pages: [{ pageId: "main", name: "Main", contextParams: [], root, machine }],
    dataBindings: extra.dataBindings ?? [],
    actionBindings: extra.actionBindings ?? [],
    fields: extra.fields ?? [],
    forms: extra.forms ?? [],
    actionPlacements: extra.actionPlacements ?? [],
    navigationItems: extra.navigationItems ?? [],
    overrides: [],
    catalog: extra.catalog ?? [],
  }
}

function makeScope(
  ux: WidgetUx,
  options: {
    readonly stateId?: string
    readonly reads?: ReadonlyMap<string, BindingReadState>
    readonly calls?: ReadonlyMap<string, BindingCallState>
    readonly dir?: "ltr" | "rtl"
    readonly context?: Readonly<Record<string, unknown>>
    readonly effects?: (actionId: string) => ActionEffects | undefined
    readonly evaluate?: MachineRuntime["evaluate"]
  } = {},
): PageScope {
  const page = ux.pages[0]!
  const machine = page.machine!
  const engine = lazyEngine(() => Promise.reject(new Error("markup only")))
  const runtime: MachineRuntime = {
    snapshot: { stateId: options.stateId ?? machine.initialStateId, fields: {}, entry: 1 },
    engine: "idle",
    evaluate: options.evaluate ?? engine.evaluate,
    send: () => {},
    retryEngine: () => {},
    reads: options.reads ?? new Map(),
    calls: options.calls ?? new Map(),
    waiting: 0,
    fault: null,
  }
  return {
    ux,
    page,
    machine,
    runtime,
    islands: {},
    locale: "en",
    dir: options.dir ?? "ltr",
    context: options.context ?? {},
    actionEffects: options.effects ?? (() => ({ irreversible: false, destructive: false, money: [], ledgered: false })),
    navigate: () => {},
    raisedBy: { current: null },
  }
}

describe("Tabs (WidgetSectionKind::Tab)", () => {
  const tabA: WidgetNode = {
    nodeId: "tab-summary",
    component: { kind: "section", section: { sectionId: "summary", kind: "tab", label: { en: "Summary" }, fields: [{ bindingId: "rec", propertyId: "title" }], order: 1 } },
    children: [],
  }
  const tabB: WidgetNode = {
    nodeId: "tab-history",
    component: { kind: "section", section: { sectionId: "history", kind: "tab", label: { en: "History" }, fields: [], order: 2 } },
    children: [{ nodeId: "hist-btn", component: { kind: "control", label: { en: "Refresh" }, input: "trigger" }, children: [] }],
  }
  const normalSection: WidgetNode = {
    nodeId: "plain-sec",
    component: { kind: "section", section: { sectionId: "plain", kind: "section", label: { en: "Overview" }, fields: [], order: 0 } },
    children: [],
  }

  test("sibling tab sections render as WAI-ARIA tablist + tabs + tabpanel sorted by section.order while non-tab sections stay unchanged", () => {
    const root: WidgetNode = {
      nodeId: "root",
      component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Page" }, fields: [], order: 0 } },
      // Deliberately reverse tab order to confirm `section.order` sorting.
      children: [normalSection, tabB, tabA],
    }
    const segments = partitionTabRuns(root.children)
    expect(segments).toHaveLength(2)
    expect(segments[0]!.kind).toBe("node")
    expect(segments[1]!.kind).toBe("tabs")
    if (segments[1]!.kind === "tabs") {
      expect(segments[1]!.nodes.map((node) => node.nodeId)).toEqual(["tab-summary", "tab-history"])
    }

    const ux = makeUx(root, {
      fields: [{ fieldId: "f-title", target: { kind: "property", property: { bindingId: "rec", propertyId: "title" } }, label: { en: "Title" }, control: "display" }],
    })
    const reads = new Map<string, BindingReadState>([["rec", { kind: "ok", document: { id: "r1", values: { title: "Invoice #42" } }, freshness: "snapshot" }]])
    const markup = renderToStaticMarkup(<Node scope={makeScope(ux, { reads })} node={root} />)

    // Non-tab section renders identically with <section> + <h2>.
    expect(markup).toContain('<section aria-label="Overview" data-node="plain-sec"')
    expect(markup).toContain(">Overview</h2>")

    // Tab group has role="tablist", role="tab", role="tabpanel", aria-selected, aria-controls, aria-labelledby.
    expect(markup).toContain('role="tablist"')
    expect(markup).toContain('role="tab"')
    expect(markup).toContain('role="tabpanel"')
    expect(markup).toContain('aria-selected="true"')
    expect(markup).toContain('aria-selected="false"')
    expect(markup).toContain('tabindex="0"')
    expect(markup).toContain('tabindex="-1"')
    expect(markup).toContain("Invoice #42")

    // Verify exact id <-> aria-controls and id <-> aria-labelledby linkage for BOTH active and inactive tabs (no dangling aria-controls).
    const tabMatches = [...markup.matchAll(/id="([^"]+tab-[^"]+)"[^>]*aria-controls="([^"]+)"/g)]
    expect(tabMatches).toHaveLength(2)
    for (const [, tabDomId, panelDomId] of tabMatches) {
      expect(markup).toContain(`id="${panelDomId}"`)
      expect(markup).toContain(`aria-labelledby="${tabDomId}"`)
    }
    // Inactive panel stays mounted hidden while active panel renders its body.
    expect(markup).toMatch(/id="[^"]+panel-tab-history"[^>]*hidden=""/)
  })

  test("tab buttons carry data-agent-confirm when their declared machine event starts a consequential action", () => {
    const root: WidgetNode = {
      nodeId: "root",
      component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Page" }, fields: [], order: 0 } },
      children: [tabA, tabB],
    }
    const tabActionMachine: Machine = {
      ...baseMachine,
      events: [...baseMachine.events, { eventId: "open-history", source: { kind: "user", nodeId: "tab-history" } }],
      transitions: [
        ...baseMachine.transitions,
        { transitionId: "t-hist", from: "idle", on: "open-history", target: "saving", effects: [{ kind: "start", call: { kind: "action", bindingId: "submit-order" } }] },
      ],
    }
    const ux = makeUx(
      root,
      { actionBindings: [{ bindingId: "submit-order", action: { id: "canon.audit.action", version: "0.1.0" }, inputMapping: {}, inputs: {}, requiredCapability: "canon.audit" }] },
      tabActionMachine,
    )
    const markup = renderToStaticMarkup(<Node scope={makeScope(ux, { effects: () => ({ irreversible: true, destructive: false, money: [] }) })} node={root} />)
    expect(markup).toMatch(/data-agent-confirm="canon\.audit\.action: cannot be undone"[^>]*data-tab-node="tab-history"/)
    expect(markup).not.toMatch(/data-agent-confirm="[^"]*"[^>]*data-tab-node="tab-summary"/)
  })

  test("roving keyboard navigation honors ArrowLeft/ArrowRight/Home/End in LTR and flips logical direction in RTL", () => {
    // LTR: ArrowRight forward, ArrowLeft backward, wrap-around, Home/End.
    expect(nextTabIndex(0, "ArrowRight", 3, "ltr")).toBe(1)
    expect(nextTabIndex(2, "ArrowRight", 3, "ltr")).toBe(0)
    expect(nextTabIndex(0, "ArrowLeft", 3, "ltr")).toBe(2)
    expect(nextTabIndex(1, "ArrowLeft", 3, "ltr")).toBe(0)
    expect(nextTabIndex(2, "Home", 3, "ltr")).toBe(0)
    expect(nextTabIndex(0, "End", 3, "ltr")).toBe(2)
    expect(nextTabIndex(0, "Tab", 3, "ltr")).toBeNull()

    // RTL: ArrowLeft moves logically forward, ArrowRight moves logically backward.
    expect(nextTabIndex(0, "ArrowLeft", 3, "rtl")).toBe(1)
    expect(nextTabIndex(2, "ArrowLeft", 3, "rtl")).toBe(0)
    expect(nextTabIndex(0, "ArrowRight", 3, "rtl")).toBe(2)
    expect(nextTabIndex(1, "ArrowRight", 3, "rtl")).toBe(0)
    expect(nextTabIndex(2, "Home", 3, "rtl")).toBe(0)
    expect(nextTabIndex(0, "End", 3, "rtl")).toBe(2)

    // Markup carries dir="rtl" and inline direction:rtl for @ui-sdk/core/tabs isRTL().
    const root: WidgetNode = {
      nodeId: "root",
      component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Page" }, fields: [], order: 0 } },
      children: [tabA, tabB],
    }
    const markup = renderToStaticMarkup(<Node scope={makeScope(makeUx(root), { dir: "rtl" })} node={root} />)
    expect(markup).toContain('dir="rtl"')
    expect(markup).toContain("direction:rtl")
  })

  test("@ui-sdk/core tabs primitive switches active tab and fires onValueChange", () => {
    const seen: Array<string | null> = []
    const instance = tabsPrimitive({
      id: "test-tabs",
      defaultValue: "tab-summary",
      onValueChange: (value) => seen.push(value),
    })
    try {
      expect(instance.api.getValue()).toBe("tab-summary")
      instance.api.select("tab-history")
      expect(instance.api.getValue()).toBe("tab-history")
      expect(seen).toEqual(["tab-history"])
    } finally {
      instance.destroy()
    }
  })
})

describe("Selectable Lists (WidgetComponent::List)", () => {
  const sampleRecords = {
    records: [
      { id: "row-1", values: { name: "Alpha", status: "open" } },
      { id: "row-2", values: { name: "Beta", status: "closed" } },
      { id: "row-3", values: null, withheld: true, withheldProperties: [] },
    ],
  }
  const reads = new Map<string, BindingReadState>([["items", { kind: "ok", document: sampleRecords, freshness: "snapshot" }]])
  const fields: FieldPresentation[] = [
    { fieldId: "f-name", target: { kind: "property", property: { bindingId: "items", propertyId: "name" } }, label: { en: "Name" }, control: "display" },
    { fieldId: "f-status", target: { kind: "property", property: { bindingId: "items", propertyId: "status" } }, label: { en: "Status" }, control: "display" },
  ]
  const refs = [
    { bindingId: "items", propertyId: "name" },
    { bindingId: "items", propertyId: "status" },
  ]

  test("selection: none stays read-only with zero selection chrome", () => {
    const listNode: WidgetNode = { nodeId: "ro-list", component: { kind: "list", dataBindingId: "items", fields: refs, selection: "none" }, children: [] }
    const markup = renderToStaticMarkup(<Node scope={makeScope(makeUx(listNode, { fields }), { reads })} node={listNode} />)
    expect(markup).not.toContain("data-selection")
    expect(markup).not.toContain("aria-selected")
    expect(markup).not.toContain("data-indicator")
    expect(markup).not.toContain("tabindex")
  })

  test("selection: single renders radio indicators, aria-selected, roving tabindex, monochrome focus ring, and keeps withheld rows disabled", () => {
    const listNode: WidgetNode = { nodeId: "selectable-list", component: { kind: "list", dataBindingId: "items", fields: refs, selection: "single" }, children: [] }
    const markup = renderToStaticMarkup(<Node scope={makeScope(makeUx(listNode, { fields }), { reads })} node={listNode} />)
    expect(markup).toContain('data-selection="single"')
    expect(markup).not.toContain("aria-multiselectable")
    expect(markup).toContain('data-indicator="radio"')
    expect(markup).toContain('aria-selected="false"')
    expect(markup).toContain('tabindex="0"')
    expect(markup).toContain('tabindex="-1"')
    expect(markup).toContain("focus-visible:outline-ring")
    expect(markup).toMatch(/data-agent-hidden=""[^>]*data-row-id="row-3"[^>]*aria-disabled="true"/)
  })

  test("selection: multiple renders checkbox indicators and aria-multiselectable=true", () => {
    const listNode: WidgetNode = { nodeId: "multi-list", component: { kind: "list", dataBindingId: "items", fields: refs, selection: "multiple" }, children: [] }
    const markup = renderToStaticMarkup(<Node scope={makeScope(makeUx(listNode, { fields }), { reads })} node={listNode} />)
    expect(markup).toContain('data-selection="multiple"')
    expect(markup).toContain('aria-multiselectable="true"')
    expect(markup).toContain('data-indicator="checkbox"')
  })

  test("single and multiple selection transitions, keyboard row navigation, and declared payload filtering", () => {
    // Single mode: replaces active row, toggles off on repeat click.
    expect(nextSelection("single", [], "row-1")).toEqual(["row-1"])
    expect(nextSelection("single", ["row-1"], "row-2")).toEqual(["row-2"])
    expect(nextSelection("single", ["row-2"], "row-2")).toEqual([])

    // Multiple mode: independent toggles.
    expect(nextSelection("multiple", [], "row-1")).toEqual(["row-1"])
    expect(nextSelection("multiple", ["row-1"], "row-2")).toEqual(["row-1", "row-2"])
    expect(nextSelection("multiple", ["row-1", "row-2"], "row-1")).toEqual(["row-2"])

    // Keyboard vertical row navigation.
    expect(nextListRowIndex(0, "ArrowDown", 3)).toBe(1)
    expect(nextListRowIndex(2, "ArrowDown", 3)).toBe(2)
    expect(nextListRowIndex(1, "ArrowUp", 3)).toBe(0)
    expect(nextListRowIndex(0, "ArrowUp", 3)).toBe(0)
    expect(nextListRowIndex(2, "Home", 3)).toBe(0)
    expect(nextListRowIndex(0, "End", 3)).toBe(2)

    // Payload filtering matches raise(): ONLY declared payload fields are emitted.
    const singleEvent: MachineEvent = { eventId: "pick", source: { kind: "user", nodeId: "l" }, payload: [{ fieldId: "id", type: "text", required: false }] }
    expect(selectionEventPayload(singleEvent, "single", "row-1", ["row-1"], [{ id: "row-1" }])).toEqual({ id: "row-1" })
    expect(selectionEventPayload(singleEvent, "single", "row-1", [], [])).toEqual({ id: null })

    const multiEvent: MachineEvent = {
      eventId: "pick-many",
      source: { kind: "user", nodeId: "l" },
      payload: [
        { fieldId: "ids", type: "json", required: true },
        { fieldId: "count", type: "integer", required: true },
      ],
    }
    expect(selectionEventPayload(multiEvent, "multiple", "row-2", ["row-1", "row-2"], [{ id: "row-1" }, { id: "row-2" }])).toEqual({
      ids: ["row-1", "row-2"],
      count: 2,
    })

    const noPayloadEvent: MachineEvent = { eventId: "pick-bare", source: { kind: "user", nodeId: "l" } }
    expect(selectionEventPayload(noPayloadEvent, "single", "row-1", ["row-1"], [{ id: "row-1" }])).toEqual({})
  })

  test("flattenSelectionRecord, reconcileSelection pruning, agent:confirm on rows, and input.selection[0].name mapping", async () => {
    // 1. Flattened entity record shape (properties at top level, not wrapped under `values`).
    expect(flattenSelectionRecord({ id: "row-1", values: { name: "Alpha", status: "open" }, masked: {} })).toEqual({
      id: "row-1",
      name: "Alpha",
      status: "open",
    })

    // 2. Refresh pruning removes deleted and newly-withheld rows immediately.
    const refreshed = [
      { id: "row-1", values: { name: "Alpha v2", status: "open" } },
      { id: "row-3", values: null, withheld: true, withheldProperties: [] },
    ]
    const reconciled = reconcileSelection(["row-1", "row-2", "row-3"], refreshed)
    expect(reconciled.changed).toBe(true)
    expect(reconciled.ids).toEqual(["row-1"])
    expect(reconciled.records).toEqual([{ id: "row-1", name: "Alpha v2", status: "open" }])

    // 3. Selectable rows carry data-agent-confirm when their declared machine event starts a consequential action.
    const rowActionMachine: Machine = {
      ...baseMachine,
      events: [...baseMachine.events, { eventId: "pick-run", source: { kind: "user", nodeId: "multi-list" } }],
      transitions: [
        ...baseMachine.transitions,
        { transitionId: "t-pick", from: "idle", on: "pick-run", target: "saving", effects: [{ kind: "start", call: { kind: "action", bindingId: "bulk-act" } }] },
      ],
    }
    const listNode: WidgetNode = { nodeId: "multi-list", component: { kind: "list", dataBindingId: "items", fields: refs, selection: "multiple" }, children: [] }
    const confirmUx = makeUx(
      listNode,
      {
        fields,
        actionBindings: [{ bindingId: "bulk-act", action: { id: "canon.item.bulk.action", version: "0.1.0" }, inputMapping: {}, inputs: { selectionBindingId: "items" }, requiredCapability: "canon.item.bulk" }],
      },
      rowActionMachine,
    )
    const confirmMarkup = renderToStaticMarkup(
      <Node scope={makeScope(confirmUx, { reads, effects: () => ({ irreversible: false, destructive: true, money: [] }) })} node={listNode} />,
    )
    expect(confirmMarkup).toMatch(/data-agent-confirm="canon\.item\.bulk\.action: may destroy data"[^>]*data-row-id="row-1"/)
    expect(confirmMarkup).not.toMatch(/data-agent-confirm="[^"]*"[^>]*data-row-id="row-3"/)

    // 4. End-to-end runtime evaluates `input.selection[0].name` directly against flattened records.
    const sent: Array<{ actionId: string; input: Fields }> = []
    const selectionMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "idle",
      fields: [],
      events: [
        { eventId: "run", source: { kind: "user", nodeId: "bulk-btn" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
      ],
      states: [
        { stateId: "idle", role: "idle" },
        { stateId: "running", role: "pending", awaits: { kind: "action", bindingId: "bulk-act" }, deadlineMs: 5000 },
        { stateId: "done", role: "idle" },
      ],
      transitions: [
        { transitionId: "t1", from: "idle", on: "run", target: "running", effects: [{ kind: "start", call: { kind: "action", bindingId: "bulk-act" } }] },
        { transitionId: "t2", from: "running", on: "accepted", target: "done" },
        { transitionId: "t3", from: "running", on: "refused", target: "idle" },
      ],
    }
    const selUx = makeUx(
      listNode,
      {
        actionBindings: [{
          bindingId: "bulk-act",
          action: { id: "canon.item.bulk.action", version: "0.1.0" },
          inputMapping: { firstName: "input.selection[0].name" },
          inputs: { selectionBindingId: "items" },
          requiredCapability: "canon.item.bulk",
        }],
      },
      selectionMachine,
    )
    const runtime = createPageRuntime(
      {
        ux: selUx,
        machine: selectionMachine,
        reader: { kind: "live", widget: () => ({ queryKey: ["noop"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: {} }) }) },
        calls: { call: async (actionId, input) => { sent.push({ actionId, input }); return { outcome: "accepted", payload: {}, body: {} } } },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({ context: {}, objectTypeOf: () => undefined, actionIdOf: (id) => (id === "canon.item.bulk.action" ? "canon.item.bulk" : undefined) }),
      },
      () => {},
    )
    runtime.setSelection("items", reconciled.records, reconciled.ids)
    runtime.send({ kind: "user", eventId: "run", payload: {} })
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("done")
    expect(sent).toEqual([{ actionId: "canon.item.bulk", input: { firstName: "Alpha v2" } }])
    runtime.dispose()
  })

  test("runtime synchronously reconciles list selection BEFORE dispatching subscription or read-outcome events", async () => {
    const sent: Array<{ actionId: string; input: Fields }> = []
    let pushSnapshot: ((doc: unknown) => void) | undefined
    const subMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "opening",
      fields: [],
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        { eventId: "items-changed", source: { kind: "subscription", bindingId: "items" } },
        { eventId: "reread", source: { kind: "user", nodeId: "reread-btn" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "items" }, deadlineMs: 5000 },
        { stateId: "ready", role: "idle" },
        { stateId: "syncing", role: "pending", awaits: { kind: "action", bindingId: "sync-selected" }, deadlineMs: 5000 },
        { stateId: "errored", role: "error" },
      ],
      transitions: [
        { transitionId: "t-open", from: "opening", on: "opened", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "items" } }] },
        { transitionId: "t-loaded", from: "loading", on: "accepted", target: "ready" },
        // Read refusal immediately triggers an action reading `input.selection` (must see []).
        { transitionId: "t-read-refused", from: "loading", on: "refused", target: "syncing", effects: [{ kind: "start", call: { kind: "action", bindingId: "sync-selected" } }] },
        // Subscription snapshot immediately triggers an action reading `input.selection` BEFORE any React effect runs!
        { transitionId: "t-sub", from: "ready", on: "items-changed", target: "syncing", effects: [{ kind: "start", call: { kind: "action", bindingId: "sync-selected" } }] },
        { transitionId: "t-sync-ok", from: "syncing", on: "accepted", target: "ready" },
        { transitionId: "t-sync-err", from: "syncing", on: "refused", target: "errored" },
        { transitionId: "t-reread", from: "ready", on: "reread", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "items" } }] },
      ],
    }
    const listNode: WidgetNode = { nodeId: "multi-list", component: { kind: "list", dataBindingId: "items", fields: refs, selection: "multiple" }, children: [] }
    const subUx = makeUx(
      listNode,
      {
        dataBindings: [{ bindingId: "items", object: { id: "canon.Item.entity", version: "0.1.0" }, query: { kind: "object_list", filters: [], sort: [], page: { size: 10, maxSize: 20 } } }],
        actionBindings: [{
          bindingId: "sync-selected",
          action: { id: "canon.item.sync.action", version: "0.1.0" },
          inputMapping: { selected: "input.selection" },
          inputs: { selectionBindingId: "items" },
          requiredCapability: "canon.item.sync",
        }],
      },
      subMachine,
    )
    const answers: Classified[] = [
      {
        outcome: "accepted",
        payload: {},
        body: {
          records: [
            { id: "row-1", values: { name: "Alpha" } },
            { id: "row-2", values: { name: "Beta" } },
            { id: "row-3", values: { name: "Gamma" } },
          ],
        },
      },
      { outcome: "refused", category: "permission", payload: { code: "authz.denied" }, body: { code: "authz.denied" } },
    ]
    const runtime = createPageRuntime(
      {
        ux: subUx,
        machine: subMachine,
        reader: { kind: "live", widget: () => ({ queryKey: ["items"], queryFn: async () => answers.shift()! }) },
        calls: { call: async (actionId, input) => { sent.push({ actionId, input }); return { outcome: "accepted", payload: {}, body: {} } } },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({
          context: {},
          objectTypeOf: () => "canon.Item",
          actionIdOf: (id) => (id === "canon.item.sync.action" ? "canon.item.sync" : undefined),
          live: { subscribe: (_read, listener) => { pushSnapshot = listener.snapshot; return () => {} } },
        }),
      },
      () => {},
    )
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("ready")

    // Select all 3 rows from initial snapshot.
    runtime.setSelection(
      "items",
      [
        { id: "row-1", name: "Alpha" },
        { id: "row-2", name: "Beta" },
        { id: "row-3", name: "Gamma" },
      ],
      ["row-1", "row-2", "row-3"],
    )

    // Live subscription snapshot removes row-2, withholds row-3, updates row-1, and synchronously triggers sync-selected.
    pushSnapshot?.({
      records: [
        { id: "row-1", values: { name: "Alpha updated" } },
        { id: "row-3", values: null, withheld: true, withheldProperties: [] },
      ],
    })
    await flush()
    expect(sent).toHaveLength(1)
    expect(sent[0]!.input).toEqual({ selected: [{ id: "row-1", name: "Alpha updated" }] })

    // Re-read settles to `refused` -> selection is synchronously cleared to [] before outcome triggers sync-selected.
    runtime.send({ kind: "user", eventId: "reread", payload: {} })
    await flush()
    expect(sent).toHaveLength(2)
    expect(sent[1]!.input).toEqual({ selected: [] })
    runtime.dispose()
  })
})

describe("component:dialog (WidgetComponent::Dialog)", () => {
  const makeDialogNode = (options: { readonly blocking: boolean; readonly bindingId?: string; readonly dismissalMethods: readonly DismissalMethod[] }): WidgetNode => ({
    nodeId: "confirm-dialog",
    component: {
      kind: "dialog",
      blocking: options.blocking,
      ...(options.bindingId === undefined ? {} : { bindingId: options.bindingId }),
      dismissalMethods: options.dismissalMethods,
    },
    children: [
      {
        nodeId: "dlg-title",
        component: { kind: "section", section: { sectionId: "dlg-title", kind: "section", label: { en: "Post Journal Entry" }, fields: [], order: 1 } },
        children: [{ nodeId: "dlg-trigger", component: { kind: "control", label: { en: "Post" }, input: "trigger" }, children: [] }],
      },
    ],
  })

  test("L1 blocking refuses Escape, scrim, swipe, and close while bound bindingId call is pending, and allows declared methods once idle or errored", () => {
    const dialogNode = makeDialogNode({ blocking: true, bindingId: "submit-order", dismissalMethods: ["escape", "scrim", "visible_close"] })
    const ux = makeUx(dialogNode)

    // Idle: not blocked.
    const idleScope = makeScope(ux, { stateId: "idle" })
    expect(isDialogBlocked(idleScope, dialogNode.component as Extract<WidgetNode["component"], { kind: "dialog" }>)).toBe(false)
    const idleMarkup = renderToStaticMarkup(<Node scope={idleScope} node={dialogNode} />)
    expect(idleMarkup).toContain('data-slot="dialog-content"')
    expect(idleMarkup).toContain('role="dialog"')
    expect(idleMarkup).toContain('aria-modal="true"')
    expect(idleMarkup).toContain('data-blocking="true"')
    expect(idleMarkup).toContain('data-pending="false"')
    expect(idleMarkup).toContain('data-slot="dialog-close"')
    expect(idleMarkup).toContain("Post Journal Entry")

    // Pending (call state = sending): blocked!
    const sendingCalls = new Map<string, BindingCallState>([["submit-order", { kind: "sending", sent: { input: "{}", key: "k1" } }]])
    const pendingScope = makeScope(ux, { stateId: "saving", calls: sendingCalls })
    expect(isDialogBlocked(pendingScope, dialogNode.component as Extract<WidgetNode["component"], { kind: "dialog" }>)).toBe(true)
    const pendingMarkup = renderToStaticMarkup(<Node scope={pendingScope} node={dialogNode} />)
    expect(pendingMarkup).toContain('data-pending="true"')
    expect(pendingMarkup).toContain('aria-busy="true"')
    expect(pendingMarkup).toContain('data-dialog-close=""')
    expect(pendingMarkup).toContain('aria-disabled="true"')
    expect(pendingMarkup).not.toContain('data-slot="dialog-close"')

    // Refused (terminal/error): no longer blocked!
    const refusedCalls = new Map<string, BindingCallState>([
      ["submit-order", { kind: "refused", refusal: { code: "ledger.unbalanced", category: "precondition", document: { message: "Debits must equal credits." } } }],
    ])
    const errorScope = makeScope(ux, { stateId: "failed", calls: refusedCalls })
    expect(isDialogBlocked(errorScope, dialogNode.component as Extract<WidgetNode["component"], { kind: "dialog" }>)).toBe(false)
    const errorMarkup = renderToStaticMarkup(<Node scope={errorScope} node={dialogNode} />)
    expect(errorMarkup).toContain('data-pending="false"')
    expect(errorMarkup).toContain('data-slot="dialog-close"')
    expect(errorMarkup).toContain("Debits must equal credits.")

    // Pure L1 dismissal gate:
    for (const reason of ["escape-key", "outside-press", "swipe", "close-press", "back"]) {
      expect(canDismissDialog(true, ["escape", "scrim", "swipe", "visible_close"], reason)).toBe(false)
    }
    expect(canDismissDialog(false, ["escape", "visible_close"], "escape-key")).toBe(true)
    expect(canDismissDialog(false, ["escape", "visible_close"], "close-press")).toBe(true)
    expect(canDismissDialog(false, ["escape", "visible_close"], "outside-press")).toBe(false)
    expect(canDismissDialog(false, ["escape", "visible_close"], "swipe")).toBe(false)
    expect(canDismissDialog(false, ["scrim"], "outside-press")).toBe(true)
    expect(canDismissDialog(false, ["scrim"], "escape-key")).toBe(false)
  })

  test("@ui-sdk/core dialog primitive refuses Escape & outside-press when dismissible=false and fires onDismissAttempt", () => {
    const attempts: string[] = []
    const changes: boolean[] = []
    const instance = dialogPrimitive({
      id: "l1-dialog",
      defaultOpen: true,
      dismissible: false,
      closeOnOutsidePress: false,
      onOpenChange: (open) => changes.push(open),
      onDismissAttempt: (details) => attempts.push(details.reason),
    })
    try {
      expect(instance.api.isOpen()).toBe(true)
      instance.api.close({ reason: "escape-key" })
      instance.api.close({ reason: "outside-press" })
      expect(instance.api.isOpen()).toBe(true)
      expect(changes).toEqual([])
      expect(attempts).toEqual(["escape-key", "outside-press"])
    } finally {
      instance.destroy()
    }
  })
})

describe("component:form (WidgetComponent::Form)", () => {
  const formDef: WidgetForm = {
    formId: "create-order",
    mode: "create",
    inputNames: ["memo", "notes", "quantity", "amount", "expedite", "currency", "due_date", "posted_at", "internal_tag"],
    submitBindingId: "submit-order",
    submit: {
      label: { en: "Create Order" },
      confirmation: { severity: "safe", strength: "none" },
    },
  }

  const formFields: FieldPresentation[] = [
    {
      fieldId: "f-memo",
      target: { kind: "form_input", formId: "create-order", inputName: "memo" },
      label: { en: "Memo" },
      help: { en: "Short summary shown in ledger." },
      control: "text",
      requiredWhen: { expression: "true", inputs: {} },
    },
    {
      fieldId: "f-notes",
      target: { kind: "form_input", formId: "create-order", inputName: "notes" },
      label: { en: "Notes" },
      control: "text_area",
    },
    {
      fieldId: "f-qty",
      target: { kind: "form_input", formId: "create-order", inputName: "quantity" },
      label: { en: "Quantity" },
      control: "number",
      format: "integer",
    },
    {
      fieldId: "f-amount",
      target: { kind: "form_input", formId: "create-order", inputName: "amount", initial: { bindingId: "prefill", propertyId: "amount" } },
      label: { en: "Amount" },
      control: "number",
      format: "decimal",
    },
    {
      fieldId: "f-expedite",
      target: { kind: "form_input", formId: "create-order", inputName: "expedite" },
      label: { en: "Expedite" },
      control: "checkbox",
    },
    {
      fieldId: "f-currency",
      target: { kind: "form_input", formId: "create-order", inputName: "currency" },
      label: { en: "Currency" },
      control: "select",
      optionQueryBindingId: "currencies",
    },
    {
      fieldId: "f-due",
      target: { kind: "form_input", formId: "create-order", inputName: "due_date" },
      label: { en: "Due Date" },
      control: "date",
    },
    {
      fieldId: "f-posted",
      target: { kind: "form_input", formId: "create-order", inputName: "posted_at", initial: { bindingId: "prefill", propertyId: "posted_at" } },
      label: { en: "Posted At" },
      control: "date_time",
    },
    {
      fieldId: "f-hidden",
      target: { kind: "form_input", formId: "create-order", inputName: "internal_tag", initial: { bindingId: "prefill", propertyId: "internal_tag" } },
      label: { en: "Internal Tag" },
      control: "text",
      visibleWhen: { expression: "false", inputs: {} },
    },
    {
      fieldId: "f-secret-display",
      target: { kind: "property", property: { bindingId: "prefill", propertyId: "tax_id" } },
      label: { en: "Tax ID" },
      control: "display",
    },
  ]

  const formNode: WidgetNode = {
    nodeId: "order-form",
    component: { kind: "form", formId: "create-order" },
    children: formFields.map((field) => ({ nodeId: `node-${field.fieldId}`, component: { kind: "field", fieldId: field.fieldId }, children: [] })),
  }

  const formUx = makeUx(formNode, {
    fields: formFields,
    forms: [formDef],
    actionBindings: [{
      bindingId: "submit-order",
      action: { id: "canon.order.create.action", version: "0.1.0" },
      inputMapping: {
        memo: "input.draft.memo",
        amount: "input.draft.amount",
        quantity: "input.draft.quantity",
      },
      inputs: { draftFormId: "create-order" },
      // Client must NEVER check this capability; server decides!
      requiredCapability: "canon.order.create.restricted",
    }],
  })

  test("renders all WidgetFieldControl kinds with exact decimal string prefill, hidden field, select options, and L6 masking", () => {
    const exactDecimal = "9007199254740993.12345678901234567890"
    const zonedTimestamp = "2026-11-26T14:30:00+02:00"
    const reads = new Map<string, BindingReadState>([
      ["prefill", {
        kind: "ok",
        document: {
          id: "ord-1",
          values: { amount: exactDecimal, posted_at: zonedTimestamp, internal_tag: "batch-7", tax_id: "•••-99" },
          masked: { tax_id: { mode: "last_n", markings: ["pii"], revealable: false } },
        },
        freshness: "snapshot",
      }],
      ["currencies", {
        kind: "ok",
        document: { records: [{ id: "USD", values: { label: "US Dollar" } }, { id: "EUR", values: { label: "Euro" } }] },
        freshness: "snapshot",
      }],
    ])
    const markup = renderToStaticMarkup(<Node scope={makeScope(formUx, { reads })} node={formNode} />)

    expect(markup).toContain('data-form="create-order"')
    expect(markup).toContain('data-dirty="false"')
    expect(markup).toContain('data-control="text"')
    expect(markup).toContain('aria-required="true"')
    expect(markup).toContain("Short summary shown in ledger.")
    expect(markup).toContain('data-control="text_area"')
    expect(markup).toContain("<textarea")
    expect(markup).toContain('data-control="number" data-format="integer"')
    expect(markup).toContain('inputMode="numeric"')
    expect(markup).toContain('data-control="number" data-format="decimal"')
    expect(markup).toContain('inputMode="decimal"')
    // Exact decimal string preserved verbatim — never converted through JS float or type="number".
    expect(markup).toContain(`value="${exactDecimal}"`)
    expect(markup).not.toContain('type="number"')
    expect(markup).toContain('data-control="checkbox"')
    expect(markup).toContain('role="checkbox"')
    expect(markup).toContain('data-control="select"')
    expect(markup).toContain('role="combobox"')
    expect(markup).toContain('data-control="date"')
    expect(markup).toContain('type="date"')
    expect(markup).toContain('data-control="date_time"')
    expect(markup).toContain(`value="${zonedTimestamp}"`)
    expect(markup).toContain('type="hidden"')
    expect(markup).toContain('data-control="hidden"')
    expect(markup).toContain('value="batch-7"')
    // Display field preserves server redaction mark (L6).
    expect(markup).toContain('data-agent-secret=""')
    expect(markup).toContain("Create Order")
  })

  test("exact-string integer/decimal validation, integer JSON number parsing without float rounding, and draft/dirty helpers", () => {
    const exact = "123456789012345678901234567890.000000000000000001"
    expect(validateExactNumber(exact, "decimal")).toBeNull()
    expect(validateExactNumber("-42.50", "currency")).toBeNull()
    expect(validateExactNumber("100", "integer")).toBeNull()
    expect(validateExactNumber("-7", "integer")).toBeNull()
    expect(validateExactNumber(String(Number.MAX_SAFE_INTEGER), "integer")).toBeNull()
    expect(validateExactNumber("12.5", "integer")).toBe("Enter a whole number.")
    expect(validateExactNumber("9007199254740993", "integer")).toBe("Whole number exceeds exact integer range.")
    expect(validateExactNumber("-9007199254740993", "integer")).toBe("Whole number exceeds exact integer range.")
    expect(validateExactNumber("12.5.1", "decimal")).toBe("Enter a valid decimal number.")
    expect(validateExactNumber("abc", "decimal")).toBe("Enter a valid decimal number.")
    expect(validateExactNumber("", "decimal")).toBeNull()

    expect(parseExactInteger("42")).toEqual({ ok: true, value: 42 })
    expect(parseExactInteger("-0")).toEqual({ ok: true, value: 0 })
    expect(parseExactInteger("9007199254740993")).toEqual({ ok: false, message: "Whole number exceeds exact integer range." })

    const initial = { memo: "", amount: exact, expedite: false }
    expect(computeFormDirty(initial, { ...initial })).toBe(false)
    expect(computeFormDirty(initial, { ...initial, memo: "Edited" })).toBe(true)

    const payload = buildFormDraftPayload(formFields, { memo: "Payroll", amount: exact, quantity: "10", expedite: true })
    expect(payload.amount).toBe(exact)
    expect(typeof payload.amount).toBe("string")
    // Integer inputs reach the cell as JSON integer numbers, never strings.
    expect(payload.quantity).toBe(10)
    expect(typeof payload.quantity).toBe("number")
    expect(Number.isInteger(payload.quantity)).toBe(true)
    expect(payload.expedite).toBe(true)

    // Unrepresentable integer is NEVER silently rounded to 9007199254740992.
    const unsafePayload = buildFormDraftPayload(formFields, { quantity: "9007199254740993" })
    expect(unsafePayload.quantity).not.toBe(9007199254740992)
  })

  test("declared click and typed confirmation gates submission and formConfirmReason covers machine-started actions", () => {
    expect(canDispatchFormConfirmation({ severity: "safe", strength: "none" }, { confirming: false, typedPhrase: "" }, "en")).toEqual({ kind: "dispatch" })

    const clickConfirm = { severity: "confirm" as const, strength: "click" as const, message: { en: "Post this entry?" } }
    expect(canDispatchFormConfirmation(clickConfirm, { confirming: false, typedPhrase: "" }, "en")).toEqual({ kind: "confirm" })
    expect(canDispatchFormConfirmation(clickConfirm, { confirming: true, typedPhrase: "" }, "en")).toEqual({ kind: "dispatch" })

    const typedConfirm = { severity: "irreversible" as const, strength: "typed" as const, phrase: { en: "DELETE ORDER" } }
    expect(canDispatchFormConfirmation(typedConfirm, { confirming: false, typedPhrase: "" }, "en")).toEqual({ kind: "confirm" })
    expect(canDispatchFormConfirmation(typedConfirm, { confirming: true, typedPhrase: "delete order" }, "en")).toEqual({
      kind: "error",
      message: 'Type "DELETE ORDER" to confirm.',
    })
    expect(canDispatchFormConfirmation(typedConfirm, { confirming: true, typedPhrase: "DELETE ORDER" }, "en")).toEqual({ kind: "dispatch" })
    // Typed confirmation with missing phrase fails closed.
    expect(canDispatchFormConfirmation({ severity: "irreversible", strength: "typed" }, { confirming: true, typedPhrase: "" }, "en")).toEqual({
      kind: "error",
      message: "Confirmation phrase is not configured.",
    })

    // formConfirmReason checks BOTH submitBindingId AND any machine-started binding on the form node.
    const machineWithExtraAction: Machine = {
      ...baseMachine,
      transitions: [
        ...baseMachine.transitions,
        { transitionId: "t-extra", from: "idle", on: "save-form", target: "saving", effects: [{ kind: "start", call: { kind: "action", bindingId: "extra-ledger" } }] },
      ],
    }
    const extraUx = makeUx(
      formNode,
      {
        forms: [formDef],
        actionBindings: [
          ...formUx.actionBindings,
          { bindingId: "extra-ledger", action: { id: "canon.ledger.post.action", version: "0.1.0" }, inputMapping: {}, inputs: {}, requiredCapability: "canon.ledger.post" },
        ],
      },
      machineWithExtraAction,
    )
    const reason = formConfirmReason(
      extraUx,
      machineWithExtraAction,
      "order-form",
      formDef,
      (actionId) => (actionId === "canon.ledger.post.action" ? { irreversible: true, destructive: false, money: [] } : { irreversible: false, destructive: false, money: [] }),
      "en",
    )
    expect(reason).toContain("canon.ledger.post.action: cannot be undone")
  })

  test("prefill propagates server masked and withheldProperties metadata onto editable and hidden inputs without leaking withheld values", () => {
    const maskedFormDef: WidgetForm = {
      formId: "secret-form",
      mode: "edit",
      dataBindingId: "secret-rec",
      inputNames: ["ssn", "secret_note", "hidden_token", "hidden_refused"],
      submitBindingId: "submit-order",
      submit: { label: { en: "Save" }, confirmation: { severity: "safe", strength: "none" } },
    }
    const maskedFields: FieldPresentation[] = [
      { fieldId: "f-ssn", target: { kind: "form_input", formId: "secret-form", inputName: "ssn" }, label: { en: "SSN" }, control: "text" },
      { fieldId: "f-secret-note", target: { kind: "form_input", formId: "secret-form", inputName: "secret_note" }, label: { en: "Secret Note" }, control: "text" },
      { fieldId: "f-hidden-tok", target: { kind: "form_input", formId: "secret-form", inputName: "hidden_token" }, label: { en: "Token" }, control: "text", visibleWhen: { expression: "false", inputs: {} } },
      { fieldId: "f-hidden-ref", target: { kind: "form_input", formId: "secret-form", inputName: "hidden_refused" }, label: { en: "Refused" }, control: "text", visibleWhen: { expression: "false", inputs: {} } },
    ]
    const maskedNode: WidgetNode = {
      nodeId: "secret-form-node",
      component: { kind: "form", formId: "secret-form" },
      children: maskedFields.map((field) => ({ nodeId: `n-${field.fieldId}`, component: { kind: "field", fieldId: field.fieldId }, children: [] })),
    }
    const maskedUx = makeUx(maskedNode, { fields: maskedFields, forms: [maskedFormDef] })
    const reads = new Map<string, BindingReadState>([
      ["secret-rec", {
        kind: "ok",
        document: {
          id: "rec-1",
          values: { ssn: "•••-1234", secret_note: "TOP-SECRET-LEAK", hidden_token: "•••-tok", hidden_refused: "HIDDEN-SECRET-LEAK" },
          masked: {
            ssn: { mode: "last_n", markings: ["pii"], revealable: false },
            hidden_token: { mode: "redacted", markings: ["secret"], revealable: false },
            hidden_refused: { mode: "refuse", markings: ["restricted"], revealable: false },
          },
          withheldProperties: ["secret_note"],
        },
        freshness: "snapshot",
      }],
    ])
    const sc = makeScope(maskedUx, { reads })
    expect(readPrefill(sc, maskedFormDef, maskedFields[0]!).masked?.mode).toBe("last_n")
    expect(readPrefill(sc, maskedFormDef, maskedFields[1]!)).toMatchObject({ value: undefined, withheld: true })
    expect(readPrefill(sc, maskedFormDef, maskedFields[3]!)).toMatchObject({ value: undefined, withheld: true })

    const markup = renderToStaticMarkup(<Node scope={sc} node={maskedNode} />)
    // Editable masked input & hidden masked input both carry data-agent-secret.
    expect(markup).toMatch(/data-agent-secret=""[^>]*value="•••-1234"/)
    expect(markup).toMatch(/data-agent-secret=""[^>]*type="hidden"[^>]*name="hidden_token"[^>]*value="•••-tok"/)
    // Withheld editable and hidden inputs carry data-agent-hidden and NEVER leak values into DOM.
    expect(markup).toContain('data-agent-hidden=""')
    expect(markup).not.toContain("TOP-SECRET-LEAK")
    expect(markup).not.toContain("HIDDEN-SECRET-LEAK")

    // During a pending or refused refresh while dirty, pinned L6 masking & withholding metadata stay intact!
    const pinnedDoc = reads.get("secret-rec")!.kind === "ok" ? (reads.get("secret-rec") as { readonly kind: "ok"; readonly document: unknown }).document : undefined
    const pinnedRecord = { bindingId: "secret-rec", document: pinnedDoc }
    for (const degradedState of [
      { kind: "pending" as const },
      { kind: "refused" as const, refusal: { code: "authz.denied", category: "permission", document: { message: "Denied." } } },
    ]) {
      const degradedScope = makeScope(maskedUx, { reads: new Map([["secret-rec", degradedState]]) })
      const liveMasked = readPrefill(degradedScope, maskedFormDef, maskedFields[0]!)
      expect(liveMasked.masked).toBeUndefined()
      const pinnedMasked = readPrefill(degradedScope, maskedFormDef, maskedFields[0]!, pinnedRecord)
      const mergedMasked = mergePrefillSecurity(liveMasked, pinnedMasked)
      expect(mergedMasked.masked?.mode).toBe("last_n")

      const liveWithheld = readPrefill(degradedScope, maskedFormDef, maskedFields[1]!)
      const pinnedWithheld = readPrefill(degradedScope, maskedFormDef, maskedFields[1]!, pinnedRecord)
      const mergedWithheld = mergePrefillSecurity(liveWithheld, pinnedWithheld)
      expect(mergedWithheld.withheld).toBe(true)
      expect(mergedWithheld.value).toBeUndefined()
    }
  })

  test("edit form does not mount editable inputs or allow submitting before its initial baseline record loads", () => {
    const editFormDef: WidgetForm = {
      formId: "edit-order",
      mode: "edit",
      dataBindingId: "order-detail",
      inputNames: ["memo"],
      submitBindingId: "submit-order",
      submit: { label: { en: "Save Changes" }, confirmation: { severity: "safe", strength: "none" } },
      versionConflictBinding: {
        property: { bindingId: "order-detail", propertyId: "version" },
        inputName: "expectedVersion",
        onConflict: "reject_and_retain_draft",
      },
    }
    const editFields: FieldPresentation[] = [
      { fieldId: "f-edit-memo", target: { kind: "form_input", formId: "edit-order", inputName: "memo" }, label: { en: "Memo" }, control: "text" },
    ]
    const editNode: WidgetNode = {
      nodeId: "edit-order-node",
      component: { kind: "form", formId: "edit-order" },
      children: [{ nodeId: "n-edit-memo", component: { kind: "field", fieldId: "f-edit-memo" }, children: [] }],
    }
    const editUx = makeUx(editNode, { fields: editFields, forms: [editFormDef] })

    // Before initial record loads (idle, pending, refused, unavailable): NO <form>, NO <input>, NO submit button.
    for (const [readState, expectedAttr] of [
      [undefined, 'data-read="idle"'],
      [{ kind: "pending" as const }, 'data-read="pending"'],
      [{ kind: "refused" as const, refusal: { code: "authz.denied", category: "permission", document: { message: "Forbidden." } } }, 'data-read="refused"'],
      [{ kind: "unavailable" as const, error: new Error("Offline."), retry: true }, 'data-read="unavailable"'],
    ] as const) {
      const reads = readState === undefined ? new Map<string, BindingReadState>() : new Map<string, BindingReadState>([["order-detail", readState]])
      const markup = renderToStaticMarkup(<Node scope={makeScope(editUx, { reads })} node={editNode} />)
      expect(markup).toContain('data-form-awaiting-baseline="true"')
      expect(markup).toContain(expectedAttr)
      expect(markup).not.toContain("<form")
      expect(markup).not.toContain("<input")
      expect(markup).not.toContain("Save Changes")
    }

    // Once initial record loads (`ok`), interactive <form> mounts hydrated with baseline values.
    const okReads = new Map<string, BindingReadState>([
      ["order-detail", { kind: "ok", document: { id: "ord-1", values: { memo: "Loaded Rev 1", version: 1 } }, freshness: "snapshot" }],
    ])
    const loadedMarkup = renderToStaticMarkup(<Node scope={makeScope(editUx, { reads: okReads })} node={editNode} />)
    expect(loadedMarkup).not.toContain("data-form-awaiting-baseline")
    expect(loadedMarkup).toContain("<form")
    expect(loadedMarkup).toContain('value="Loaded Rev 1"')
    expect(loadedMarkup).toContain("Save Changes")
  })

  test("field conditions receive recordKey paired with record (input.recordKey == \"ord-1\")", () => {
    const condFormDef: WidgetForm = {
      formId: "key-cond-form",
      mode: "edit",
      dataBindingId: "order-detail",
      inputNames: ["special_note"],
      submitBindingId: "submit-order",
      submit: { label: { en: "Save" }, confirmation: { severity: "safe", strength: "none" } },
    }
    const condField: FieldPresentation = {
      fieldId: "f-special",
      target: { kind: "form_input", formId: "key-cond-form", inputName: "special_note" },
      label: { en: "Special Note" },
      control: "text",
      visibleWhen: { expression: 'input.recordKey == "ord-1"', inputs: { recordBindingId: "order-detail" } },
    }
    const condNode: WidgetNode = {
      nodeId: "key-cond-node",
      component: { kind: "form", formId: "key-cond-form" },
      children: [{ nodeId: "n-special", component: { kind: "field", fieldId: "f-special" }, children: [] }],
    }
    const condUx = makeUx(condNode, { fields: [condField], forms: [condFormDef] })
    const syncEval: MachineRuntime["evaluate"] = (expr, input) => {
      const parsed = JSON.parse(pathEngine.evaluateExpression(expr, JSON.stringify(input))) as { ok: boolean; value?: unknown }
      if (!parsed.ok) throw new Error("eval failed")
      return parsed.value
    }

    const scopeOrd1 = makeScope(condUx, {
      reads: new Map([["order-detail", { kind: "ok", document: { id: "ord-1", values: { special_note: "Match" } }, freshness: "snapshot" }]]),
      evaluate: syncEval,
    })
    const scopeOrd2 = makeScope(condUx, {
      reads: new Map([["order-detail", { kind: "ok", document: { id: "ord-2", values: { special_note: "Other" } }, freshness: "snapshot" }]]),
      evaluate: syncEval,
    })
    expect(evaluateFieldCondition(scopeOrd1, condField.visibleWhen, {}, false)).toBe(true)
    expect(evaluateFieldCondition(scopeOrd2, condField.visibleWhen, {}, true)).toBe(false)

    // In rendered form: visible text input for ord-1, hidden input for ord-2.
    const markup1 = renderToStaticMarkup(<Node scope={scopeOrd1} node={condNode} />)
    expect(markup1).toContain('data-field="f-special" data-control="text"')
    const markup2 = renderToStaticMarkup(<Node scope={scopeOrd2} node={condNode} />)
    expect(markup2).toContain('type="hidden"')
    expect(markup2).toContain('data-field="f-special" data-control="hidden"')
  })

  test("pending state disables submit button; server refusals render RefusalCategory + server message across all closed states", () => {
    // Pending disables submit button and marks aria-busy.
    const sendingCalls = new Map<string, BindingCallState>([["submit-order", { kind: "sending", sent: { input: "{}", key: "k1" } }]])
    const pendingMarkup = renderToStaticMarkup(<Node scope={makeScope(formUx, { stateId: "saving", calls: sendingCalls })} node={formNode} />)
    expect(pendingMarkup).toContain('data-pending="true"')
    expect(pendingMarkup).toContain('aria-disabled="true"')
    expect(pendingMarkup).toContain('data-call="sending"')
    expect(pendingMarkup).toContain("Submitting…")

    // Refused surfaces RefusalCategory + exact server message.
    const categories = [
      ["permission", "Permission refused"],
      ["invalid", "Invalid input"],
      ["conflict", "Conflict"],
      ["precondition", "Precondition failed"],
      ["visibility", "Not visible"],
      ["limit", "Limit reached"],
      ["internal", "Server error"],
    ] as const
    for (const [category, expectedLabel] of categories) {
      expect(formatRefusalCategory(category)).toBe(expectedLabel)
      const calls = new Map<string, BindingCallState>([
        ["submit-order", { kind: "refused", refusal: { code: `err.${category}`, category, document: { message: `Server said ${category}.` } } }],
      ])
      const refusedMarkup = renderToStaticMarkup(<Node scope={makeScope(formUx, { stateId: "failed", calls })} node={formNode} />)
      expect(refusedMarkup).toContain('role="alert"')
      expect(refusedMarkup).toContain('data-call="refused"')
      expect(refusedMarkup).toContain(`data-category="${category}"`)
      expect(refusedMarkup).toContain(`data-code="err.${category}"`)
      expect(refusedMarkup).toContain(expectedLabel)
      expect(refusedMarkup).toContain(`Server said ${category}.`)
    }

    // Closed call states: pending_approval, committed, not_served, unavailable, timed_out.
    const otherStates: BindingCallState[] = [
      { kind: "pending_approval", decision: { id: "d1" } },
      { kind: "committed", receipt: {}, undo: null },
      { kind: "not_served" },
      { kind: "unavailable", sent: { input: "{}", key: "k1" } },
      { kind: "timed_out", sent: { input: "{}", key: "k1" }, message: "Timed out." },
    ]
    for (const state of otherStates) {
      const markup = renderToStaticMarkup(<Node scope={makeScope(formUx, { calls: new Map([["submit-order", state]]) })} node={formNode} />)
      expect(markup).toContain(`data-call="${state.kind}"`)
    }

    // Select option read failure surfaces honest closed read states.
    const refusedRead = new Map<string, BindingReadState>([
      ["currencies", { kind: "refused", refusal: { code: "authz.denied", category: "permission", document: { message: "Cannot list currencies." } } }],
    ])
    const selectRefusedMarkup = renderToStaticMarkup(<Node scope={makeScope(formUx, { reads: refusedRead })} node={formNode} />)
    expect(selectRefusedMarkup).toContain('data-read="refused"')
    expect(selectRefusedMarkup).toContain("Cannot list currencies.")
  })

  test("L6 agent:confirm marks form submit button when effects are unknown or consequential", () => {
    const unknownMarkup = renderToStaticMarkup(<Node scope={makeScope(formUx, { effects: () => undefined })} node={formNode} />)
    expect(unknownMarkup).toContain('data-agent-confirm="canon.order.create.action: its effects are not known"')
  })

  test("runtime submits form draft via machine event and direct submitForm, blocks duplicate submits, and never checks permissions client-side", async () => {
    const exactAmount = "999999999999999999.99"
    const sent: Array<{ actionId: string; input: Fields; key: string }> = []
    let release!: (answer: Classified) => void
    const calls: WidgetCalls = {
      call: (actionId, input, options) => {
        sent.push({ actionId, input, key: options.idempotencyKey })
        return new Promise<Classified>((resolve) => { release = resolve })
      },
    }
    const reader: WidgetReader = { kind: "live", widget: () => ({ queryKey: ["noop"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: {} }) }) }
    const runtime = createPageRuntime(
      {
        ux: formUx,
        machine: baseMachine,
        reader,
        calls,
        engine: lazyEngine(async () => pathEngine),
        host: () => ({ context: {}, objectTypeOf: () => undefined, actionIdOf: (id) => (id === "canon.order.create.action" ? "canon.order.create" : undefined) }),
      },
      () => {},
    )
    expect(needsEngine(formUx, baseMachine)).toBe(true)

    // 1. Direct submitForm: maps input.draft, preserves exact decimal string + integer number, deduplicates while pending.
    runtime.submitForm("create-order", "submit-order", { memo: "Q4", amount: exactAmount, quantity: 3 })
    expect(runtime.view().calls.get("submit-order")?.kind).toBe("sending")
    // Duplicate submit while sending is ignored.
    runtime.submitForm("create-order", "submit-order", { memo: "Q4", amount: exactAmount, quantity: 3 })
    await flush()
    expect(sent).toHaveLength(1)
    expect(sent[0]!.actionId).toBe("canon.order.create")
    expect(sent[0]!.input).toEqual({ memo: "Q4", amount: exactAmount, quantity: 3 })
    expect(typeof sent[0]!.input.quantity).toBe("number")

    // Server refuses with permission category -> surfaced cleanly on runtime.
    release({ outcome: "refused", category: "permission", payload: { code: "authz.denied" }, body: { code: "authz.denied", message: "Actor lacks canon.order.create." } })
    await flush()
    expect(runtime.view().calls.get("submit-order")).toMatchObject({
      kind: "refused",
      refusal: { code: "authz.denied", category: "permission" },
    })

    // 2. Machine-driven form submit using runtime.setDraft:
    runtime.setDraft("create-order", { memo: "Retry", amount: exactAmount, quantity: 4 })
    runtime.send({ kind: "user", eventId: "save-form", payload: {} })
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("saving")
    expect(sent).toHaveLength(2)
    expect(sent[1]!.input).toEqual({ memo: "Retry", amount: exactAmount, quantity: 4 })
    release({ outcome: "accepted", payload: {}, body: { ok: true } })
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("saved")
    expect(runtime.view().calls.get("submit-order")?.kind).toBe("committed")
    runtime.dispose()
  })

  test("versioned records send only WASM-allowed slots, and dirty edits submit their pinned baseline revision even after a newer revision arrives", async () => {
    const revA = { id: "ord-1", version: 1, values: { memo: "Original", version: 1 } }
    const revB = { id: "ord-1", version: 2, values: { memo: "Updated elsewhere", version: 2 } }
    const sent: Array<{ actionId: string; input: Fields }> = []
    let pushSnapshot: ((doc: unknown) => void) | undefined

    const editMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "opening",
      fields: [],
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        { eventId: "live-update", source: { kind: "subscription", bindingId: "order-detail" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "order-detail" }, deadlineMs: 5000 },
        { stateId: "editing", role: "idle" },
      ],
      transitions: [
        { transitionId: "t-open", from: "opening", on: "opened", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "order-detail" } }] },
        { transitionId: "t-loaded", from: "loading", on: "accepted", target: "editing" },
        { transitionId: "t-ref", from: "loading", on: "refused", target: "editing" },
      ],
    }

    const editFormDef: WidgetForm = {
      formId: "edit-order",
      mode: "edit",
      dataBindingId: "order-detail",
      inputNames: ["memo"],
      submitBindingId: "save-edit",
      submit: { label: { en: "Save" }, confirmation: { severity: "safe", strength: "none" } },
      versionConflictBinding: {
        property: { bindingId: "order-detail", propertyId: "version" },
        inputName: "expectedVersion",
        onConflict: "reject_and_retain_draft",
      },
    }
    const editNode: WidgetNode = { nodeId: "edit-form", component: { kind: "form", formId: "edit-order" }, children: [] }
    const editUx = makeUx(
      editNode,
      {
        dataBindings: [{ bindingId: "order-detail", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_runtime_detail", pageId: "main", parameterId: "orderId" } }],
        forms: [editFormDef],
        actionBindings: [{
          bindingId: "save-edit",
          action: { id: "canon.order.edit.action", version: "0.1.0" },
          inputMapping: {
            id: "input.recordKey",
            memo: "input.draft.memo",
            expectedVersion: "input.record.version",
          },
          inputs: { draftFormId: "edit-order", recordBindingId: "order-detail" },
          requiredCapability: "canon.order.edit",
        }],
      },
      editMachine,
    )

    const runtime = createPageRuntime(
      {
        ux: editUx,
        machine: editMachine,
        reader: { kind: "live", widget: () => ({ queryKey: ["detail"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: revA }) }) },
        calls: { call: async (actionId, input) => { sent.push({ actionId, input }); return { outcome: "accepted", payload: {}, body: {} } } },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({
          context: { orderId: "ord-1" },
          objectTypeOf: () => "canon.Order",
          actionIdOf: (id) => (id === "canon.order.edit.action" ? "canon.order.edit" : undefined),
          live: { subscribe: (_read, listener) => { pushSnapshot = listener.snapshot; return () => {} } },
        }),
      },
      () => {},
    )
    await flush()
    expect(runtime.view().reads.get("order-detail")).toMatchObject({ kind: "ok", document: revA })

    // User makes a dirty edit against revA -> pins revA.
    const pinnedRevA = { bindingId: "order-detail", document: revA }
    runtime.setDraft("edit-order", { memo: "User edit on revA" }, pinnedRevA)

    // Background refresh delivers revB (version: 2).
    pushSnapshot?.(revB)
    expect(runtime.view().reads.get("order-detail")).toMatchObject({ kind: "ok", document: revB })

    // Submitting must pass WASM strict slot check (no `recordVersion` slot) AND send revA's expectedVersion (1), NOT revB (2)!
    runtime.submitForm("edit-order", "save-edit", { memo: "User edit on revA" }, pinnedRevA)
    await flush()
    expect(runtime.view().fault).toBeNull()
    expect(sent).toEqual([{ actionId: "canon.order.edit", input: { id: "ord-1", memo: "User edit on revA", expectedVersion: 1 } }])
    runtime.dispose()
  })

  test("direct submitForm enforces a bounded deadline on stalled engine or transport and reuses idempotency key on retry", async () => {
    const clock = manualClock()
    const keys: string[] = []
    const stalledEngineRuntime = createPageRuntime(
      {
        ux: formUx,
        machine: baseMachine,
        clock,
        reader: { kind: "live", widget: () => ({ queryKey: ["noop"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: {} }) }) },
        calls: { call: async (_id, _input, opts) => { keys.push(opts.idempotencyKey); return new Promise(() => {}) } },
        engine: lazyEngine(() => new Promise(() => {})),
        host: () => ({ context: {}, objectTypeOf: () => undefined, actionIdOf: (id) => (id === "canon.order.create.action" ? "canon.order.create" : undefined) }),
      },
      () => {},
    )
    stalledEngineRuntime.submitForm("create-order", "submit-order", { memo: "Stalled eval", amount: "10.00", quantity: 1 })
    expect(stalledEngineRuntime.view().calls.get("submit-order")?.kind).toBe("sending")
    clock.advance(DEFAULT_SUBMIT_DEADLINE_MS)
    await flush()
    expect(stalledEngineRuntime.view().calls.get("submit-order")?.kind).toBe("timed_out")
    expect(keys).toEqual([])
    stalledEngineRuntime.dispose()

    // Stalled transport times out at deadline and keeps idempotency key for retry.
    const transportClock = manualClock()
    const transportRuntime = createPageRuntime(
      {
        ux: formUx,
        machine: baseMachine,
        clock: transportClock,
        reader: { kind: "live", widget: () => ({ queryKey: ["noop"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: {} }) }) },
        calls: { call: async (_id, _input, opts) => { keys.push(opts.idempotencyKey); return new Promise(() => {}) } },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({ context: {}, objectTypeOf: () => undefined, actionIdOf: (id) => (id === "canon.order.create.action" ? "canon.order.create" : undefined) }),
      },
      () => {},
    )
    transportRuntime.submitForm("create-order", "submit-order", { memo: "Slow cell", amount: "10.00", quantity: 1 })
    await flush()
    expect(transportRuntime.view().calls.get("submit-order")?.kind).toBe("sending")
    transportClock.advance(DEFAULT_SUBMIT_DEADLINE_MS)
    await flush()
    expect(transportRuntime.view().calls.get("submit-order")?.kind).toBe("timed_out")
    // Retry with identical draft reuses the exact idempotency key.
    transportRuntime.submitForm("create-order", "submit-order", { memo: "Slow cell", amount: "10.00", quantity: 1 })
    await flush()
    expect(keys).toHaveLength(2)
    expect(keys[0]).toBe(keys[1])
    transportRuntime.dispose()
  })

  test("needsEngine returns true when a page's only expression is a non-literal field condition and false for literals", () => {
    const bareMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "idle",
      fields: [],
      events: [],
      states: [{ stateId: "idle", role: "idle" }],
      transitions: [],
    }
    const bareForm: WidgetForm = {
      formId: "cond-form",
      mode: "create",
      inputNames: ["reason"],
      submitBindingId: "bare-submit",
      submit: { label: { en: "Submit" }, confirmation: { severity: "safe", strength: "none" } },
    }
    const node: WidgetNode = {
      nodeId: "cond-form-node",
      component: { kind: "form", formId: "cond-form" },
      children: [{ nodeId: "n-reason", component: { kind: "field", fieldId: "f-reason" }, children: [] }],
    }
    const literalUx = makeUx(
      node,
      {
        forms: [bareForm],
        actionBindings: [{ bindingId: "bare-submit", action: { id: "a", version: "0.1.0" }, inputMapping: {}, inputs: {}, requiredCapability: "a" }],
        fields: [{ fieldId: "f-reason", target: { kind: "form_input", formId: "cond-form", inputName: "reason" }, label: { en: "Reason" }, control: "text", requiredWhen: { expression: "true", inputs: {} } }],
      },
      bareMachine,
    )
    expect(needsEngine(literalUx, bareMachine)).toBe(false)

    const exprUx = makeUx(
      node,
      {
        forms: [bareForm],
        actionBindings: [{ bindingId: "bare-submit", action: { id: "a", version: "0.1.0" }, inputMapping: {}, inputs: {}, requiredCapability: "a" }],
        fields: [{ fieldId: "f-reason", target: { kind: "form_input", formId: "cond-form", inputName: "reason" }, label: { en: "Reason" }, control: "text", visibleWhen: { expression: "input.draft.expedite == true", inputs: { draftFormId: "cond-form" } } }],
      },
      bareMachine,
    )
    expect(needsEngine(exprUx, bareMachine)).toBe(true)
  })
})

describe("Capabilities parity and zero-Radix invariant", () => {
  test("RENDERED_COMPONENTS includes dialog and form, and combined vocab v2 page has zero missing capabilities", () => {
    expect(RENDERED_COMPONENTS).toContain("dialog")
    expect(RENDERED_COMPONENTS).toContain("form")
    expect(RENDERER_CAPABILITIES.has("component:dialog")).toBe(true)
    expect(RENDERER_CAPABILITIES.has("component:form")).toBe(true)
    expect(RENDERER_CAPABILITIES.has("view:form")).toBe(true)

    const combined: WidgetUx = makeUx({
      nodeId: "root",
      component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Root" }, fields: [], order: 0 } },
      children: [
        { nodeId: "t1", component: { kind: "section", section: { sectionId: "t1", kind: "tab", label: { en: "Tab 1" }, fields: [], order: 1 } }, children: [] },
        { nodeId: "l1", component: { kind: "list", dataBindingId: "items", fields: [], selection: "multiple" }, children: [] },
        { nodeId: "d1", component: { kind: "dialog", bindingId: "submit-order", blocking: true, dismissalMethods: ["escape"] }, children: [] },
        { nodeId: "f1", component: { kind: "form", formId: "create-order" }, children: [] },
      ],
    })
    expect(requiredCapabilities(combined)).toContain("component:dialog")
    expect(requiredCapabilities(combined)).toContain("component:form")
    expect(requiredCapabilities(combined)).toContain("component:list")
    expect(requiredCapabilities(combined)).toContain("component:section")
    expect(missingCapabilities(combined)).toEqual([])
  })

  test("NornAppRenderer refuses contracts referencing input.recordVersion or unformatted number controls whole", () => {
    const recVersionUx = makeUx(
      { nodeId: "root", component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Root" }, fields: [], order: 0 } }, children: [] },
      {
        actionBindings: [{
          bindingId: "edit-parent",
          action: { id: "canon.order.edit.action", version: "0.1.0" },
          inputMapping: { expectedVersion: "input.recordVersion" },
          inputs: { recordBindingId: "order" },
          requiredCapability: "canon.order.edit",
        }],
      },
    )
    const refusedMarkup = renderToStaticMarkup(
      <NornAppRenderer
        ux={recVersionUx}
        workspace={{ kind: "deciding" }}
        calls={{ call: async () => ({ outcome: "not_served", payload: {} }) }}
        engine={() => Promise.reject(new Error("unused"))}
        objectTypeOf={() => undefined}
        actionIdOf={() => undefined}
      />,
    )
    expect(refusedMarkup).toContain('data-norn-refused=""')
    expect(refusedMarkup).toContain("expr:recordVersion (action:edit-parent.expectedVersion)")

    const unformattedNumUx = makeUx(
      { nodeId: "root", component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Root" }, fields: [], order: 0 } }, children: [] },
      {
        fields: [{ fieldId: "f-qty", target: { kind: "form_input", formId: "f", inputName: "qty" }, label: { en: "Qty" }, control: "number" }],
      },
    )
    const numRefusedMarkup = renderToStaticMarkup(
      <NornAppRenderer
        ux={unformattedNumUx}
        workspace={{ kind: "deciding" }}
        calls={{ call: async () => ({ outcome: "not_served", payload: {} }) }}
        engine={() => Promise.reject(new Error("unused"))}
        objectTypeOf={() => undefined}
        actionIdOf={() => undefined}
      />,
    )
    expect(numRefusedMarkup).toContain('data-norn-refused=""')
    expect(numRefusedMarkup).toContain("field:number:unformatted (f-qty)")
  })

  test("referencesRecordVersion detects parenthesized, optional, indexed, and conditional accesses while ignoring literals, arrays, and nested properties", () => {
    expect(referencesRecordVersion("input.recordVersion")).toBe(true)
    expect(referencesRecordVersion("(input).recordVersion")).toBe(true)
    expect(referencesRecordVersion("(true ? input : input).recordVersion")).toBe(true)
    expect(referencesRecordVersion("((input))['recordVersion']")).toBe(true)
    expect(referencesRecordVersion('((input))["recordVersion"]')).toBe(true)
    expect(referencesRecordVersion("input?.recordVersion")).toBe(true)
    expect(referencesRecordVersion('input?.["recordVersion"]')).toBe(true)
    expect(referencesRecordVersion("[input][0].recordVersion")).toBe(true)
    expect(referencesRecordVersion("({ wrapped: input }).wrapped.recordVersion")).toBe(true)
    expect(referencesRecordVersion('({ wrapped: input })["wrapped"]["recordVersion"]')).toBe(true)
    expect(referencesRecordVersion("({ input }).input.recordVersion")).toBe(true)
    expect(referencesRecordVersion('({ ["wrapped"]: input }).wrapped.recordVersion')).toBe(true)
    expect(referencesRecordVersion('input["record" + "Version"]')).toBe(true)
    expect(referencesRecordVersion("(input.record.amount > 10 ? input : input).recordVersion")).toBe(true)
    expect(referencesRecordVersion("(input.record.amount in [1..5] ? input : input).recordVersion")).toBe(true)
    expect(referencesRecordVersion("(input.record.amount in [1..5) ? input : input).recordVersion")).toBe(true)
    expect(referencesRecordVersion("(input.record.amount in (1..5] ? input : input).recordVersion")).toBe(true)
    expect(referencesRecordVersion("(input.record.rate >= 3.14 ? input : input).recordVersion")).toBe(true)
    expect(referencesRecordVersion('"input.recordVersion"')).toBe(false)
    expect(referencesRecordVersion("'input.recordVersion'")).toBe(false)
    expect(referencesRecordVersion("input.record.version")).toBe(false)
    expect(referencesRecordVersion('input.draft.kind in ["recordVersion"]')).toBe(false)
    expect(referencesRecordVersion('["recordVersion"]')).toBe(false)
    expect(referencesRecordVersion("input.record.recordVersion")).toBe(false)
    expect(referencesRecordVersion("((input).record).recordVersion")).toBe(false)
    expect(referencesRecordVersion('input.draft["recordVersion"]')).toBe(false)
  })

  test("runtime.readSecurity synchronously captures withholding across batched ok -> pending transitions", async () => {
    let pushSnapshot: ((doc: unknown) => void) | undefined
    const revVisible = { id: "ord-1", values: { secret: "plain" } }
    const revWithheld = { id: "ord-1", values: {}, withheldProperties: ["secret"] }
    let resolveReread!: (value: Classified) => void
    let reads = 0

    const batchMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "opening",
      fields: [],
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
        { eventId: "sub", source: { kind: "subscription", bindingId: "order-detail" } },
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "order-detail" }, deadlineMs: 5000 },
        { stateId: "ready", role: "idle" },
      ],
      transitions: [
        { transitionId: "t-open", from: "opening", on: "opened", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "order-detail" } }] },
        { transitionId: "t-ok", from: "loading", on: "accepted", target: "ready" },
        { transitionId: "t-ref", from: "loading", on: "refused", target: "ready" },
        // Subscription snapshot synchronously starts a re-read, immediately flipping read state ok -> pending!
        { transitionId: "t-sub", from: "ready", on: "sub", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "order-detail" } }] },
      ],
    }

    const ux = makeUx(
      { nodeId: "root", component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Root" }, fields: [], order: 0 } }, children: [] },
      {
        dataBindings: [{ bindingId: "order-detail", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_runtime_detail", pageId: "main", parameterId: "orderId" } }],
      },
      batchMachine,
    )

    const runtime = createPageRuntime(
      {
        ux,
        machine: batchMachine,
        reader: {
          kind: "live",
          widget: () => ({
            queryKey: ["order", reads],
            queryFn: () => {
              reads += 1
              if (reads === 1) return Promise.resolve({ outcome: "accepted" as const, payload: {}, body: revVisible })
              return new Promise<Classified>((resolve) => { resolveReread = resolve })
            },
          }),
        },
        calls: { call: async () => ({ outcome: "not_served", payload: {} }) },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({
          context: { orderId: "ord-1" },
          objectTypeOf: () => "canon.Order",
          actionIdOf: () => undefined,
          live: { subscribe: (_read, listener) => { pushSnapshot = listener.snapshot; return () => {} } },
        }),
      },
      () => {},
    )
    await flush()
    expect(runtime.view().reads.get("order-detail")?.kind).toBe("ok")

    // Subscription delivers revWithheld AND synchronously transitions binding back to pending.
    pushSnapshot?.(revWithheld)
    expect(runtime.view().reads.get("order-detail")?.kind).toBe("pending")
    // Even though React never committed while `ok`, runtime.readSecurity synchronously recorded withholding!
    expect(runtime.view().readSecurity?.get("order-detail")?.withheldProperties).toContain("secret")
    const resolved = resolveFieldPrefillSecurity(
      { value: undefined, masked: undefined, withheld: false },
      { value: "plain", masked: undefined, withheld: false },
      true,
      false,
      { value: undefined, masked: undefined, withheld: true },
    )
    expect(resolved.withheld).toBe(true)
    resolveReread({ outcome: "accepted", payload: {}, body: revWithheld })
    await flush()
    runtime.dispose()
  })

  test("resolveFieldPrefillSecurity ratchets stricter masking while dirty and restores withheld fields on clean ok refresh", () => {
    const masked = { mode: "redacted" as const, markings: ["pii"], revealable: false }
    const visible = { value: "raw", masked: undefined, withheld: false }
    const maskedLive = { value: "***", masked, withheld: false }
    const withheldLive = { value: undefined, masked: undefined, withheld: true }
    const pendingEmpty = { value: undefined, masked: undefined, withheld: false }

    // Dirty form: live ok refresh upgrades masking -> saved into pinned -> subsequent pending/refused keeps mask.
    const ratcheted = resolveFieldPrefillSecurity(maskedLive, visible, true, true)
    expect(ratcheted.masked).toEqual(masked)
    const afterPending = resolveFieldPrefillSecurity(pendingEmpty, ratcheted, true, false)
    expect(afterPending.masked).toEqual(masked)

    // Clean form: previously withheld field restored on ok refresh clears withholding immediately.
    const restored = resolveFieldPrefillSecurity(visible, withheldLive, false, true)
    expect(restored.withheld).toBe(false)
    expect(restored.value).toBe("raw")

    // Clean form: non-ok refresh preserves pinned withholding/masking.
    const stillWithheld = resolveFieldPrefillSecurity(pendingEmpty, withheldLive, false, false)
    expect(stillWithheld.withheld).toBe(true)
  })

  test("synchronous selection reconciliation keys by envelope row.id so colliding values.id never substitutes another row", async () => {
    const rowA = { id: "env-A", values: { id: "env-B", name: "Alpha" } }
    const rowB = { id: "env-B", values: { id: "env-B", name: "Beta" } }
    const sent: Array<{ actionId: string; input: Fields }> = []
    let pushSnapshot: ((doc: unknown) => void) | undefined

    const subMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "opening",
      fields: [],
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
        { eventId: "list-changed", source: { kind: "subscription", bindingId: "orders" } },
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "orders" }, deadlineMs: 5000 },
        { stateId: "ready", role: "idle" },
        { stateId: "acting", role: "pending", awaits: { kind: "action", bindingId: "act-selected" }, deadlineMs: 5000 },
      ],
      transitions: [
        { transitionId: "t-open", from: "opening", on: "opened", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "orders" } }] },
        { transitionId: "t-loaded", from: "loading", on: "accepted", target: "ready" },
        { transitionId: "t-ref", from: "loading", on: "refused", target: "ready" },
        { transitionId: "t-sub", from: "ready", on: "list-changed", target: "acting", effects: [{ kind: "start", call: { kind: "action", bindingId: "act-selected" } }] },
        { transitionId: "t-act-ok", from: "acting", on: "accepted", target: "ready" },
      ],
    }

    const listNode: WidgetNode = {
      nodeId: "orders-list",
      component: { kind: "list", dataBindingId: "orders", fields: [{ bindingId: "orders", propertyId: "name" }], selection: "single" },
      children: [],
    }
    const subUx = makeUx(
      listNode,
      {
        dataBindings: [{ bindingId: "orders", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_list", filters: [], sort: [], page: { size: 10, maxSize: 20 } } }],
        actionBindings: [{
          bindingId: "act-selected",
          action: { id: "canon.order.act.action", version: "0.1.0" },
          inputMapping: { selected: "input.selection" },
          inputs: { selectionBindingId: "orders" },
          requiredCapability: "canon.order.act",
        }],
      },
      subMachine,
    )

    const runtime = createPageRuntime(
      {
        ux: subUx,
        machine: subMachine,
        reader: { kind: "live", widget: () => ({ queryKey: ["orders"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: { records: [rowA, rowB] } }) }) },
        calls: { call: async (actionId, input) => { sent.push({ actionId, input }); return { outcome: "accepted", payload: {}, body: {} } } },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({
          context: {},
          objectTypeOf: () => "canon.Order",
          actionIdOf: (id) => (id === "canon.order.act.action" ? "canon.order.act" : undefined),
          live: { subscribe: (_read, listener) => { pushSnapshot = listener.snapshot; return () => {} } },
        }),
      },
      () => {},
    )
    await flush()
    // Select row A using its canonical envelope id "env-A" even though values.id === "env-B".
    runtime.setSelection("orders", [flattenSelectionRecord(rowA)], ["env-A"])
    // Live snapshot removes row A while keeping row B ("env-B"): reconciliation must NOT substitute row B!
    pushSnapshot?.({ records: [rowB] })
    await flush()
    expect(sent).toEqual([{ actionId: "canon.order.act", input: { selected: [] } }])

    // Select row B by envelope id, then push an envelope-less row carrying values.id === "env-B":
    // neither reconcileSelection nor runtime selection reconciliation may infer identity from values.id!
    runtime.setSelection("orders", [flattenSelectionRecord(rowB)], ["env-B"])
    const envelopeLessImpostor = { values: { id: "env-B", name: "Impostor" } }
    expect(envelopeRowId(envelopeLessImpostor)).toBeNull()
    expect(reconcileSelection(["env-B"], [envelopeLessImpostor])).toEqual({ ids: [], records: [], changed: true })
    pushSnapshot?.({ records: [envelopeLessImpostor] })
    await flush()
    expect(sent).toEqual([
      { actionId: "canon.order.act", input: { selected: [] } },
      { actionId: "canon.order.act", input: { selected: [] } },
    ])
    runtime.dispose()
  })

  test("mergeReadSecurity preserves mode:'refuse' against later 'redacted', and readSecurityLog resets cleanly on restored ok reads", async () => {
    let pushSnapshot: ((doc: unknown) => void) | undefined
    const revRefused = { id: "ord-1", values: {}, masked: { secret: { mode: "refuse", markings: ["pii"], revealable: false } } }
    const revRedacted = { id: "ord-1", values: { secret: "***" }, masked: { secret: { mode: "redacted", markings: ["pii"], revealable: false } } }
    const revRestored = { id: "ord-1", values: { secret: "visible-again" } }

    // Direct unit law: mergeReadSecurity and mergePrefillSecurity never downgrade mode:'refuse' to 'redacted'.
    const mergedSec = mergeReadSecurity(
      { withheld: false, masked: { secret: { mode: "refuse" } }, withheldProperties: ["secret"] },
      { withheld: false, masked: { secret: { mode: "redacted" } }, withheldProperties: [] },
    )
    expect((mergedSec.masked.secret as { mode?: string })?.mode).toBe("refuse")
    expect(mergedSec.withheldProperties).toContain("secret")
    const mergedPrefill = mergePrefillSecurity(
      { value: "***", masked: { mode: "redacted", markings: ["pii"], revealable: false }, withheld: false },
      { value: undefined, masked: { mode: "refuse", markings: ["pii"], revealable: false }, withheld: true },
    )
    expect(mergedPrefill.withheld).toBe(true)
    expect(mergedPrefill.masked?.mode).toBe("refuse")

    const liveMachine: Machine = {
      machineVersion: "0.1",
      initialStateId: "opening",
      fields: [],
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
        { eventId: "sub", source: { kind: "subscription", bindingId: "order-detail" } },
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "order-detail" }, deadlineMs: 5000 },
        { stateId: "ready", role: "idle" },
      ],
      transitions: [
        { transitionId: "t-open", from: "opening", on: "opened", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "order-detail" } }] },
        { transitionId: "t-ok", from: "loading", on: "accepted", target: "ready" },
        { transitionId: "t-ref", from: "loading", on: "refused", target: "ready" },
        { transitionId: "t-sub", from: "ready", on: "sub", target: "ready" },
      ],
    }

    const ux = makeUx(
      { nodeId: "root", component: { kind: "section", section: { sectionId: "root", kind: "section", label: { en: "Root" }, fields: [], order: 0 } }, children: [] },
      { dataBindings: [{ bindingId: "order-detail", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_runtime_detail", pageId: "main", parameterId: "orderId" } }] },
      liveMachine,
    )

    const runtime = createPageRuntime(
      {
        ux,
        machine: liveMachine,
        reader: { kind: "live", widget: () => ({ queryKey: ["order"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: revRestored }) }) },
        calls: { call: async () => ({ outcome: "not_served", payload: {} }) },
        engine: lazyEngine(async () => pathEngine),
        host: () => ({
          context: { orderId: "ord-1" },
          objectTypeOf: () => "canon.Order",
          actionIdOf: () => undefined,
          live: { subscribe: (_read, listener) => { pushSnapshot = listener.snapshot; return () => {} } },
        }),
      },
      () => {},
    )
    await flush()
    const seqInitial = runtime.view().readSecurityLog?.get("order-detail")?.latestSeq ?? 0
    expect(seqInitial).toBeGreaterThan(0)

    // While dirty (pinned at seqInitial), push mode:'refuse' then synchronously mode:'redacted':
    // securitySince(log, seqInitial) MUST retain 'refuse' and withhold 'secret'.
    pushSnapshot?.(revRefused)
    pushSnapshot?.(revRedacted)
    const sinceInitial = securitySince(runtime.view().readSecurityLog?.get("order-detail"), seqInitial)
    expect((sinceInitial?.masked.secret as { mode?: string })?.mode).toBe("refuse")
    expect(sinceInitial?.withheldProperties).toContain("secret")

    // Later, a clean read restores 'secret'. Clean form advances its watermark to latestSeq.
    pushSnapshot?.(revRestored)
    const seqRestored = runtime.view().readSecurityLog?.get("order-detail")?.latestSeq ?? 0
    expect(seqRestored).toBeGreaterThan(seqInitial)
    // Next local edit makes the form dirty while restored read remains ok: securitySince(log, seqRestored)
    // is undefined, so prior withholding is NOT resurrected!
    const sinceRestored = securitySince(runtime.view().readSecurityLog?.get("order-detail"), seqRestored)
    expect(sinceRestored).toBeUndefined()

    // Even after 300+ subsequent reads oscillating masking on an unrelated field, the old refusal
    // of `secret` from before `seqRestored` is NEVER resurrected!
    for (let i = 0; i < 300; i += 1) {
      pushSnapshot?.({
        id: "ord-1",
        values: { secret: "visible-again", other: "x" },
        ...(i % 2 === 0 ? { masked: { other: { mode: "redacted", markings: ["pii"], revealable: false } } } : {}),
      })
    }
    const sinceMany = securitySince(runtime.view().readSecurityLog?.get("order-detail"), seqRestored)
    expect(sinceMany?.withheld).toBe(false)
    expect(sinceMany?.withheldProperties).not.toContain("secret")
    expect(sinceMany?.masked.secret).toBeUndefined()
    expect((sinceMany?.masked.other as { mode?: string })?.mode).toBe("redacted")
    runtime.dispose()
  })

  test("packages/norn-react/src never imports Radix", () => {
    const srcDir = join(import.meta.dir, "../src")
    const walk = (dir: string): string[] =>
      readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
        const full = join(dir, entry.name)
        return entry.isDirectory() ? walk(full) : [full]
      })
    for (const file of walk(srcDir)) {
      const content = readFileSync(file, "utf8")
      expect(content).not.toMatch(/@radix-ui\//)
    }
  })
})
