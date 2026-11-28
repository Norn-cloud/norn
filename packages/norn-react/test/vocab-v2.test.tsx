import { describe, expect, test } from "bun:test"
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
      for (const token of path.slice("input".length).match(/\.[A-Za-z_][A-Za-z0-9_]*|\[\d+\]/g) ?? []) {
        if (token.startsWith(".")) {
          value = typeof value === "object" && value !== null ? (value as Record<string, unknown>)[token.slice(1)] : undefined
        } else {
          value = Array.isArray(value) ? value[Number(token.slice(1, -1))] : undefined
        }
      }
      return value
    }
    const eqMatch = /^(input(?:\.[A-Za-z_][A-Za-z0-9_]*|\[\d+\])+)\s*==\s*("[^"]*")$/.exec(source)
    if (eqMatch !== null) {
      return JSON.stringify({ ok: true, value: resolvePath(eqMatch[1]!) === (JSON.parse(eqMatch[2]!) as string) })
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

const sectionNode = (
  nodeId: string,
  label: string,
  options: {
    readonly sectionId?: string
    readonly kind?: "section" | "tab"
    readonly order?: number
    readonly fields?: readonly { readonly bindingId: string; readonly propertyId: string }[]
    readonly children?: readonly WidgetNode[]
  } = {},
): WidgetNode => ({
  nodeId,
  component: {
    kind: "section",
    section: {
      sectionId: options.sectionId ?? nodeId,
      kind: options.kind ?? "section",
      label: { en: label },
      fields: options.fields ?? [],
      order: options.order ?? 0,
    },
  },
  children: options.children ?? [],
})

const rootNode = (children: readonly WidgetNode[] = []): WidgetNode =>
  sectionNode("root", "Root", { sectionId: "root", children })

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

function makeSubMachine(options: {
  readonly bindingId: string
  readonly subEventId?: string
  readonly readyStateId?: string
  readonly extraEvents?: readonly MachineEvent[]
  readonly extraStates?: Machine["states"]
  readonly extraTransitions?: Machine["transitions"]
  readonly onReadRefusedTarget?: string
  readonly onReadRefusedEffects?: Machine["transitions"][number]["effects"]
}): Machine {
  const ready = options.readyStateId ?? "ready"
  const subEvent = options.subEventId ?? "sub"
  return {
    machineVersion: "0.1",
    initialStateId: "opening",
    fields: [],
    events: [
      { eventId: "opened", source: { kind: "page_opened" } },
      { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
      { eventId: "refused", source: { kind: "outcome", outcome: "refused" } },
      { eventId: subEvent, source: { kind: "subscription", bindingId: options.bindingId } },
      ...(options.extraEvents ?? []),
    ],
    states: [
      { stateId: "opening", role: "idle" },
      { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: options.bindingId }, deadlineMs: 5000 },
      { stateId: ready, role: "idle" },
      ...(options.extraStates ?? []),
    ],
    transitions: [
      { transitionId: "t-open", from: "opening", on: "opened", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: options.bindingId } }] },
      { transitionId: "t-ok", from: "loading", on: "accepted", target: ready },
      {
        transitionId: "t-ref",
        from: "loading",
        on: "refused",
        target: options.onReadRefusedTarget ?? ready,
        ...(options.onReadRefusedEffects === undefined ? {} : { effects: options.onReadRefusedEffects }),
      },
      ...(options.extraTransitions ?? []),
    ],
  }
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

const renderNode = (ux: WidgetUx, node: WidgetNode, options?: Parameters<typeof makeScope>[1]) =>
  renderToStaticMarkup(<Node scope={makeScope(ux, options)} node={node} />)

const noopReader: WidgetReader = {
  kind: "live",
  widget: () => ({ queryKey: ["noop"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: {} }) }),
}

function startRuntime(options: {
  readonly ux: WidgetUx
  readonly machine?: Machine
  readonly clock?: Clock
  readonly reader?: WidgetReader
  readonly calls?: WidgetCalls
  readonly engine?: ReturnType<typeof lazyEngine>
  readonly context?: Readonly<Record<string, unknown>>
  readonly objectType?: string
  readonly actionMap?: Readonly<Record<string, string>>
  readonly subscribe?: boolean
}) {
  let pushSnapshot: ((doc: unknown) => void) | undefined
  const sent: Array<{ actionId: string; input: Fields; key: string }> = []
  const machine = options.machine ?? options.ux.pages[0]!.machine!
  const runtime = createPageRuntime(
    {
      ux: options.ux,
      machine,
      ...(options.clock === undefined ? {} : { clock: options.clock }),
      reader: options.reader ?? noopReader,
      calls: options.calls ?? {
        call: async (actionId, input, callOpts) => {
          sent.push({ actionId, input, key: callOpts.idempotencyKey })
          return { outcome: "accepted", payload: {}, body: {} }
        },
      },
      engine: options.engine ?? lazyEngine(async () => pathEngine),
      host: () => ({
        context: options.context ?? {},
        objectTypeOf: () => options.objectType,
        actionIdOf: (id) => options.actionMap?.[id],
        ...(options.subscribe
          ? { live: { subscribe: (_read, listener) => { pushSnapshot = listener.snapshot; return () => {} } } }
          : {}),
      }),
    },
    () => {},
  )
  return { runtime, sent, push: (doc: unknown) => pushSnapshot?.(doc) }
}

describe("Tabs (WidgetSectionKind::Tab)", () => {
  const tabA = sectionNode("tab-summary", "Summary", { sectionId: "summary", kind: "tab", order: 1, fields: [{ bindingId: "rec", propertyId: "title" }] })
  const tabB = sectionNode("tab-history", "History", {
    sectionId: "history",
    kind: "tab",
    order: 2,
    children: [{ nodeId: "hist-btn", component: { kind: "control", label: { en: "Refresh" }, input: "trigger" }, children: [] }],
  })
  const normalSection = sectionNode("plain-sec", "Overview", { sectionId: "plain", order: 0 })

  test("sibling tab sections render as WAI-ARIA tablist + tabs + tabpanel sorted by section.order while non-tab sections stay unchanged", () => {
    const root = sectionNode("root", "Page", { children: [normalSection, tabB, tabA] })
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
    const markup = renderNode(ux, root, { reads })

    for (const expected of [
      '<section aria-label="Overview" data-node="plain-sec"',
      ">Overview</h2>",
      'role="tablist"',
      'role="tab"',
      'role="tabpanel"',
      'aria-selected="true"',
      'aria-selected="false"',
      'tabindex="0"',
      'tabindex="-1"',
      "Invoice #42",
    ]) {
      expect(markup).toContain(expected)
    }

    const tabMatches = [...markup.matchAll(/id="([^"]+tab-[^"]+)"[^>]*aria-controls="([^"]+)"/g)]
    expect(tabMatches).toHaveLength(2)
    for (const [, tabDomId, panelDomId] of tabMatches) {
      expect(markup).toContain(`id="${panelDomId}"`)
      expect(markup).toContain(`aria-labelledby="${tabDomId}"`)
    }
    expect(markup).toMatch(/id="[^"]+panel-tab-history"[^>]*hidden=""/)
  })

  test("tab buttons carry data-agent-confirm when their declared machine event starts a consequential action", () => {
    const root = sectionNode("root", "Page", { children: [tabA, tabB] })
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
    const markup = renderNode(ux, root, { effects: () => ({ irreversible: true, destructive: false, money: [] }) })
    expect(markup).toMatch(/data-agent-confirm="canon\.audit\.action: cannot be undone"[^>]*data-tab-node="tab-history"/)
    expect(markup).not.toMatch(/data-agent-confirm="[^"]*"[^>]*data-tab-node="tab-summary"/)
  })

  test("roving keyboard navigation honors ArrowLeft/ArrowRight/Home/End in LTR and flips logical direction in RTL", () => {
    const navCases: readonly [number, string, "ltr" | "rtl", number | null][] = [
      [0, "ArrowRight", "ltr", 1],
      [2, "ArrowRight", "ltr", 0],
      [0, "ArrowLeft", "ltr", 2],
      [1, "ArrowLeft", "ltr", 0],
      [2, "Home", "ltr", 0],
      [0, "End", "ltr", 2],
      [0, "Tab", "ltr", null],
      [0, "ArrowLeft", "rtl", 1],
      [2, "ArrowLeft", "rtl", 0],
      [0, "ArrowRight", "rtl", 2],
      [1, "ArrowRight", "rtl", 0],
      [2, "Home", "rtl", 0],
      [0, "End", "rtl", 2],
    ]
    for (const [idx, key, dir, expected] of navCases) {
      expect(nextTabIndex(idx, key, 3, dir)).toBe(expected)
    }

    const root = sectionNode("root", "Page", { children: [tabA, tabB] })
    const markup = renderNode(makeUx(root), root, { dir: "rtl" })
    expect(markup).toContain('dir="rtl"')
    expect(markup).toContain("direction:rtl")
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
  const makeListNode = (nodeId: string, selection: "none" | "single" | "multiple"): WidgetNode =>
    ({ nodeId, component: { kind: "list", dataBindingId: "items", fields: refs, selection }, children: [] })

  test("selection modes render read-only, single radio (with withheld rows disabled), and multiple checkbox chrome", () => {
    const noneNode = makeListNode("ro-list", "none")
    const noneMarkup = renderNode(makeUx(noneNode, { fields }), noneNode, { reads })
    for (const absent of ["data-selection", "aria-selected", "data-indicator", "tabindex"]) {
      expect(noneMarkup).not.toContain(absent)
    }

    const singleNode = makeListNode("selectable-list", "single")
    const singleMarkup = renderNode(makeUx(singleNode, { fields }), singleNode, { reads })
    for (const present of ['data-selection="single"', 'data-indicator="radio"', 'aria-selected="false"', 'tabindex="0"', 'tabindex="-1"', "focus-visible:outline-ring"]) {
      expect(singleMarkup).toContain(present)
    }
    expect(singleMarkup).not.toContain("aria-multiselectable")
    expect(singleMarkup).toMatch(/data-agent-hidden=""[^>]*data-row-id="row-3"[^>]*aria-disabled="true"/)

    const multiNode = makeListNode("multi-list", "multiple")
    const multiMarkup = renderNode(makeUx(multiNode, { fields }), multiNode, { reads })
    for (const present of ['data-selection="multiple"', 'aria-multiselectable="true"', 'data-indicator="checkbox"']) {
      expect(multiMarkup).toContain(present)
    }
  })

  test("single and multiple selection transitions, keyboard row navigation, and declared payload filtering", () => {
    const selCases: readonly ["single" | "multiple", readonly string[], string, readonly string[]][] = [
      ["single", [], "row-1", ["row-1"]],
      ["single", ["row-1"], "row-2", ["row-2"]],
      ["single", ["row-2"], "row-2", []],
      ["multiple", [], "row-1", ["row-1"]],
      ["multiple", ["row-1"], "row-2", ["row-1", "row-2"]],
      ["multiple", ["row-1", "row-2"], "row-1", ["row-2"]],
    ]
    for (const [mode, prev, id, expected] of selCases) {
      expect(nextSelection(mode, prev, id)).toEqual([...expected])
    }

    const keyCases: readonly [number, string, number][] = [
      [0, "ArrowDown", 1],
      [2, "ArrowDown", 2],
      [1, "ArrowUp", 0],
      [0, "ArrowUp", 0],
      [2, "Home", 0],
      [0, "End", 2],
    ]
    for (const [idx, key, expected] of keyCases) {
      expect(nextListRowIndex(idx, key, 3)).toBe(expected)
    }

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
    expect(flattenSelectionRecord({ id: "row-1", values: { name: "Alpha", status: "open" }, masked: {} })).toEqual({
      id: "row-1",
      name: "Alpha",
      status: "open",
    })

    const reconciled = reconcileSelection(["row-1", "row-2", "row-3"], [
      { id: "row-1", values: { name: "Alpha v2", status: "open" } },
      { id: "row-3", values: null, withheld: true, withheldProperties: [] },
    ])
    expect(reconciled).toEqual({ changed: true, ids: ["row-1"], records: [{ id: "row-1", name: "Alpha v2", status: "open" }] })

    const rowActionMachine: Machine = {
      ...baseMachine,
      events: [...baseMachine.events, { eventId: "pick-run", source: { kind: "user", nodeId: "multi-list" } }],
      transitions: [
        ...baseMachine.transitions,
        { transitionId: "t-pick", from: "idle", on: "pick-run", target: "saving", effects: [{ kind: "start", call: { kind: "action", bindingId: "bulk-act" } }] },
      ],
    }
    const listNode = makeListNode("multi-list", "multiple")
    const confirmUx = makeUx(
      listNode,
      {
        fields,
        actionBindings: [{ bindingId: "bulk-act", action: { id: "canon.item.bulk.action", version: "0.1.0" }, inputMapping: {}, inputs: { selectionBindingId: "items" }, requiredCapability: "canon.item.bulk" }],
      },
      rowActionMachine,
    )
    const confirmMarkup = renderNode(confirmUx, listNode, { reads, effects: () => ({ irreversible: false, destructive: true, money: [] }) })
    expect(confirmMarkup).toMatch(/data-agent-confirm="canon\.item\.bulk\.action: may destroy data"[^>]*data-row-id="row-1"/)
    expect(confirmMarkup).not.toMatch(/data-agent-confirm="[^"]*"[^>]*data-row-id="row-3"/)

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
    const { runtime, sent } = startRuntime({ ux: selUx, actionMap: { "canon.item.bulk.action": "canon.item.bulk" } })
    runtime.setSelection("items", reconciled.records, reconciled.ids)
    runtime.send({ kind: "user", eventId: "run", payload: {} })
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("done")
    expect(sent.map(({ actionId, input }) => ({ actionId, input }))).toEqual([{ actionId: "canon.item.bulk", input: { firstName: "Alpha v2" } }])
    runtime.dispose()
  })

  test("runtime synchronously reconciles list selection BEFORE dispatching subscription or read-outcome events", async () => {
    const subMachine = makeSubMachine({
      bindingId: "items",
      subEventId: "items-changed",
      onReadRefusedTarget: "syncing",
      onReadRefusedEffects: [{ kind: "start", call: { kind: "action", bindingId: "sync-selected" } }],
      extraEvents: [{ eventId: "reread", source: { kind: "user", nodeId: "reread-btn" } }],
      extraStates: [
        { stateId: "syncing", role: "pending", awaits: { kind: "action", bindingId: "sync-selected" }, deadlineMs: 5000 },
        { stateId: "errored", role: "error" },
      ],
      extraTransitions: [
        { transitionId: "t-sub", from: "ready", on: "items-changed", target: "syncing", effects: [{ kind: "start", call: { kind: "action", bindingId: "sync-selected" } }] },
        { transitionId: "t-sync-ok", from: "syncing", on: "accepted", target: "ready" },
        { transitionId: "t-sync-err", from: "syncing", on: "refused", target: "errored" },
        { transitionId: "t-reread", from: "ready", on: "reread", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "items" } }] },
      ],
    })
    const subUx = makeUx(
      makeListNode("multi-list", "multiple"),
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
      { outcome: "accepted", payload: {}, body: { records: [{ id: "row-1", values: { name: "Alpha" } }, { id: "row-2", values: { name: "Beta" } }, { id: "row-3", values: { name: "Gamma" } }] } },
      { outcome: "refused", category: "permission", payload: { code: "authz.denied" }, body: { code: "authz.denied" } },
    ]
    const { runtime, sent, push } = startRuntime({
      ux: subUx,
      reader: { kind: "live", widget: () => ({ queryKey: ["items"], queryFn: async () => answers.shift()! }) },
      objectType: "canon.Item",
      actionMap: { "canon.item.sync.action": "canon.item.sync" },
      subscribe: true,
    })
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("ready")

    runtime.setSelection("items", [{ id: "row-1", name: "Alpha" }, { id: "row-2", name: "Beta" }, { id: "row-3", name: "Gamma" }], ["row-1", "row-2", "row-3"])
    push({ records: [{ id: "row-1", values: { name: "Alpha updated" } }, { id: "row-3", values: null, withheld: true, withheldProperties: [] }] })
    await flush()
    expect(sent).toHaveLength(1)
    expect(sent[0]!.input).toEqual({ selected: [{ id: "row-1", name: "Alpha updated" }] })

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
      sectionNode("dlg-title", "Post Journal Entry", {
        order: 1,
        children: [{ nodeId: "dlg-trigger", component: { kind: "control", label: { en: "Post" }, input: "trigger" }, children: [] }],
      }),
    ],
  })

  test("L1 blocking refuses Escape, scrim, swipe, and close while bound bindingId call is pending, and allows declared methods once idle or errored", () => {
    const dialogNode = makeDialogNode({ blocking: true, bindingId: "submit-order", dismissalMethods: ["escape", "scrim", "visible_close"] })
    const dialogComp = dialogNode.component as Extract<WidgetNode["component"], { kind: "dialog" }>
    const ux = makeUx(dialogNode)

    const idleScope = makeScope(ux, { stateId: "idle" })
    expect(isDialogBlocked(idleScope, dialogComp)).toBe(false)
    const idleMarkup = renderToStaticMarkup(<Node scope={idleScope} node={dialogNode} />)
    for (const expected of ['data-slot="dialog-content"', 'role="dialog"', 'aria-modal="true"', 'data-blocking="true"', 'data-pending="false"', 'data-slot="dialog-close"', "Post Journal Entry"]) {
      expect(idleMarkup).toContain(expected)
    }

    const pendingScope = makeScope(ux, {
      stateId: "saving",
      calls: new Map<string, BindingCallState>([["submit-order", { kind: "sending", sent: { input: "{}", key: "k1" } }]]),
    })
    expect(isDialogBlocked(pendingScope, dialogComp)).toBe(true)
    const pendingMarkup = renderToStaticMarkup(<Node scope={pendingScope} node={dialogNode} />)
    for (const expected of ['data-pending="true"', 'aria-busy="true"', 'data-dialog-close=""', 'aria-disabled="true"']) {
      expect(pendingMarkup).toContain(expected)
    }
    expect(pendingMarkup).not.toContain('data-slot="dialog-close"')

    const errorScope = makeScope(ux, {
      stateId: "failed",
      calls: new Map<string, BindingCallState>([
        ["submit-order", { kind: "refused", refusal: { code: "ledger.unbalanced", category: "precondition", document: { message: "Debits must equal credits." } } }],
      ]),
    })
    expect(isDialogBlocked(errorScope, dialogComp)).toBe(false)
    const errorMarkup = renderToStaticMarkup(<Node scope={errorScope} node={dialogNode} />)
    for (const expected of ['data-pending="false"', 'data-slot="dialog-close"', "Debits must equal credits."]) {
      expect(errorMarkup).toContain(expected)
    }

    for (const reason of ["escape-key", "outside-press", "swipe", "close-press", "back"]) {
      expect(canDismissDialog(true, ["escape", "scrim", "swipe", "visible_close"], reason)).toBe(false)
    }
    const dismissCases: readonly [readonly DismissalMethod[], string, boolean][] = [
      [["escape", "visible_close"], "escape-key", true],
      [["escape", "visible_close"], "close-press", true],
      [["escape", "visible_close"], "outside-press", false],
      [["escape", "visible_close"], "swipe", false],
      [["scrim"], "outside-press", true],
      [["scrim"], "escape-key", false],
    ]
    for (const [methods, reason, expected] of dismissCases) {
      expect(canDismissDialog(false, methods, reason)).toBe(expected)
    }
  })
})

describe("component:form (WidgetComponent::Form)", () => {
  const formDef: WidgetForm = {
    formId: "create-order",
    mode: "create",
    inputNames: ["memo", "notes", "quantity", "amount", "expedite", "currency", "due_date", "posted_at", "internal_tag"],
    submitBindingId: "submit-order",
    submit: { label: { en: "Create Order" }, confirmation: { severity: "safe", strength: "none" } },
  }

  const formFields: FieldPresentation[] = [
    { fieldId: "f-memo", target: { kind: "form_input", formId: "create-order", inputName: "memo" }, label: { en: "Memo" }, help: { en: "Short summary shown in ledger." }, control: "text", requiredWhen: { expression: "true", inputs: {} } },
    { fieldId: "f-notes", target: { kind: "form_input", formId: "create-order", inputName: "notes" }, label: { en: "Notes" }, control: "text_area" },
    { fieldId: "f-qty", target: { kind: "form_input", formId: "create-order", inputName: "quantity" }, label: { en: "Quantity" }, control: "number", format: "integer" },
    { fieldId: "f-amount", target: { kind: "form_input", formId: "create-order", inputName: "amount", initial: { bindingId: "prefill", propertyId: "amount" } }, label: { en: "Amount" }, control: "number", format: "decimal" },
    { fieldId: "f-expedite", target: { kind: "form_input", formId: "create-order", inputName: "expedite" }, label: { en: "Expedite" }, control: "checkbox" },
    { fieldId: "f-currency", target: { kind: "form_input", formId: "create-order", inputName: "currency" }, label: { en: "Currency" }, control: "select", optionQueryBindingId: "currencies" },
    { fieldId: "f-due", target: { kind: "form_input", formId: "create-order", inputName: "due_date" }, label: { en: "Due Date" }, control: "date" },
    { fieldId: "f-posted", target: { kind: "form_input", formId: "create-order", inputName: "posted_at", initial: { bindingId: "prefill", propertyId: "posted_at" } }, label: { en: "Posted At" }, control: "date_time" },
    { fieldId: "f-hidden", target: { kind: "form_input", formId: "create-order", inputName: "internal_tag", initial: { bindingId: "prefill", propertyId: "internal_tag" } }, label: { en: "Internal Tag" }, control: "text", visibleWhen: { expression: "false", inputs: {} } },
    { fieldId: "f-secret-display", target: { kind: "property", property: { bindingId: "prefill", propertyId: "tax_id" } }, label: { en: "Tax ID" }, control: "display" },
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
      inputMapping: { memo: "input.draft.memo", amount: "input.draft.amount", quantity: "input.draft.quantity" },
      inputs: { draftFormId: "create-order" },
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
    const markup = renderNode(formUx, formNode, { reads })

    for (const expected of [
      'data-form="create-order"',
      'data-dirty="false"',
      'data-control="text"',
      'aria-required="true"',
      "Short summary shown in ledger.",
      'data-control="text_area"',
      "<textarea",
      'data-control="number" data-format="integer"',
      'inputMode="numeric"',
      'data-control="number" data-format="decimal"',
      'inputMode="decimal"',
      `value="${exactDecimal}"`,
      'data-control="checkbox"',
      'role="checkbox"',
      'data-control="select"',
      'role="combobox"',
      'data-control="date"',
      'type="date"',
      'data-control="date_time"',
      `value="${zonedTimestamp}"`,
      'type="hidden"',
      'data-control="hidden"',
      'value="batch-7"',
      'data-agent-secret=""',
      "Create Order",
    ]) {
      expect(markup).toContain(expected)
    }
    expect(markup).not.toContain('type="number"')
  })

  test("exact-string integer/decimal validation, integer JSON number parsing without float rounding, and draft/dirty helpers", () => {
    const exact = "123456789012345678901234567890.000000000000000001"
    const validationCases: readonly [string, "integer" | "decimal" | "currency", string | null][] = [
      [exact, "decimal", null],
      ["-42.50", "currency", null],
      ["100", "integer", null],
      ["-7", "integer", null],
      [String(Number.MAX_SAFE_INTEGER), "integer", null],
      ["12.5", "integer", "Enter a whole number."],
      ["9007199254740993", "integer", "Whole number exceeds exact integer range."],
      ["-9007199254740993", "integer", "Whole number exceeds exact integer range."],
      ["12.5.1", "decimal", "Enter a valid decimal number."],
      ["abc", "decimal", "Enter a valid decimal number."],
      ["", "decimal", null],
    ]
    for (const [value, format, expected] of validationCases) {
      expect(validateExactNumber(value, format)).toBe(expected)
    }

    expect(parseExactInteger("42")).toEqual({ ok: true, value: 42 })
    expect(parseExactInteger("-0")).toEqual({ ok: true, value: 0 })
    expect(parseExactInteger("9007199254740993")).toEqual({ ok: false, message: "Whole number exceeds exact integer range." })

    const initial = { memo: "", amount: exact, expedite: false }
    expect(computeFormDirty(initial, { ...initial })).toBe(false)
    expect(computeFormDirty(initial, { ...initial, memo: "Edited" })).toBe(true)

    const payload = buildFormDraftPayload(formFields, { memo: "Payroll", amount: exact, quantity: "10", expedite: true })
    expect(payload.amount).toBe(exact)
    expect(typeof payload.amount).toBe("string")
    expect(payload.quantity).toBe(10)
    expect(typeof payload.quantity).toBe("number")
    expect(Number.isInteger(payload.quantity)).toBe(true)
    expect(payload.expedite).toBe(true)

    expect(buildFormDraftPayload(formFields, { quantity: "9007199254740993" }).quantity).not.toBe(9007199254740992)
  })

  test("declared click and typed confirmation gates submission and formConfirmReason covers machine-started actions", () => {
    expect(canDispatchFormConfirmation({ severity: "safe", strength: "none" }, { confirming: false, typedPhrase: "" }, "en")).toEqual({ kind: "dispatch" })

    const clickConfirm = { severity: "confirm" as const, strength: "click" as const, message: { en: "Post this entry?" } }
    expect(canDispatchFormConfirmation(clickConfirm, { confirming: false, typedPhrase: "" }, "en")).toEqual({ kind: "confirm" })
    expect(canDispatchFormConfirmation(clickConfirm, { confirming: true, typedPhrase: "" }, "en")).toEqual({ kind: "dispatch" })

    const typedConfirm = { severity: "irreversible" as const, strength: "typed" as const, phrase: { en: "DELETE ORDER" } }
    expect(canDispatchFormConfirmation(typedConfirm, { confirming: false, typedPhrase: "" }, "en")).toEqual({ kind: "confirm" })
    expect(canDispatchFormConfirmation(typedConfirm, { confirming: true, typedPhrase: "delete order" }, "en")).toEqual({ kind: "error", message: 'Type "DELETE ORDER" to confirm.' })
    expect(canDispatchFormConfirmation(typedConfirm, { confirming: true, typedPhrase: "DELETE ORDER" }, "en")).toEqual({ kind: "dispatch" })
    expect(canDispatchFormConfirmation({ severity: "irreversible", strength: "typed" }, { confirming: true, typedPhrase: "" }, "en")).toEqual({ kind: "error", message: "Confirmation phrase is not configured." })

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
    const secretDoc = {
      id: "rec-1",
      values: { ssn: "•••-1234", secret_note: "TOP-SECRET-LEAK", hidden_token: "•••-tok", hidden_refused: "HIDDEN-SECRET-LEAK" },
      masked: {
        ssn: { mode: "last_n", markings: ["pii"], revealable: false },
        hidden_token: { mode: "redacted", markings: ["secret"], revealable: false },
        hidden_refused: { mode: "refuse", markings: ["restricted"], revealable: false },
      },
      withheldProperties: ["secret_note"],
    }
    const reads = new Map<string, BindingReadState>([["secret-rec", { kind: "ok", document: secretDoc, freshness: "snapshot" }]])
    const sc = makeScope(maskedUx, { reads })
    expect(readPrefill(sc, maskedFormDef, maskedFields[0]!).masked?.mode).toBe("last_n")
    expect(readPrefill(sc, maskedFormDef, maskedFields[1]!)).toMatchObject({ value: undefined, withheld: true })
    expect(readPrefill(sc, maskedFormDef, maskedFields[3]!)).toMatchObject({ value: undefined, withheld: true })

    const markup = renderToStaticMarkup(<Node scope={sc} node={maskedNode} />)
    expect(markup).toMatch(/data-agent-secret=""[^>]*value="•••-1234"/)
    expect(markup).toMatch(/data-agent-secret=""[^>]*type="hidden"[^>]*name="hidden_token"[^>]*value="•••-tok"/)
    expect(markup).toContain('data-agent-hidden=""')
    expect(markup).not.toContain("TOP-SECRET-LEAK")
    expect(markup).not.toContain("HIDDEN-SECRET-LEAK")

    const pinnedRecord = { bindingId: "secret-rec", document: secretDoc }
    for (const degradedState of [
      { kind: "pending" as const },
      { kind: "refused" as const, refusal: { code: "authz.denied", category: "permission", document: { message: "Denied." } } },
    ]) {
      const degradedScope = makeScope(maskedUx, { reads: new Map([["secret-rec", degradedState]]) })
      const liveMasked = readPrefill(degradedScope, maskedFormDef, maskedFields[0]!)
      expect(liveMasked.masked).toBeUndefined()
      expect(mergePrefillSecurity(liveMasked, readPrefill(degradedScope, maskedFormDef, maskedFields[0]!, pinnedRecord)).masked?.mode).toBe("last_n")

      const mergedWithheld = mergePrefillSecurity(
        readPrefill(degradedScope, maskedFormDef, maskedFields[1]!),
        readPrefill(degradedScope, maskedFormDef, maskedFields[1]!, pinnedRecord),
      )
      expect(mergedWithheld).toMatchObject({ withheld: true, value: undefined })
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
      versionConflictBinding: { property: { bindingId: "order-detail", propertyId: "version" }, inputName: "expectedVersion", onConflict: "reject_and_retain_draft" },
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

    for (const [readState, expectedAttr] of [
      [undefined, 'data-read="idle"'],
      [{ kind: "pending" as const }, 'data-read="pending"'],
      [{ kind: "refused" as const, refusal: { code: "authz.denied", category: "permission", document: { message: "Forbidden." } } }, 'data-read="refused"'],
      [{ kind: "unavailable" as const, error: new Error("Offline."), retry: true }, 'data-read="unavailable"'],
    ] as const) {
      const reads = readState === undefined ? new Map<string, BindingReadState>() : new Map<string, BindingReadState>([["order-detail", readState]])
      const markup = renderNode(editUx, editNode, { reads })
      expect(markup).toContain('data-form-awaiting-baseline="true"')
      expect(markup).toContain(expectedAttr)
      for (const absent of ["<form", "<input", "Save Changes"]) expect(markup).not.toContain(absent)
    }

    const okReads = new Map<string, BindingReadState>([
      ["order-detail", { kind: "ok", document: { id: "ord-1", values: { memo: "Loaded Rev 1", version: 1 } }, freshness: "snapshot" }],
    ])
    const loadedMarkup = renderNode(editUx, editNode, { reads: okReads })
    expect(loadedMarkup).not.toContain("data-form-awaiting-baseline")
    for (const expected of ["<form", 'value="Loaded Rev 1"', "Save Changes"]) expect(loadedMarkup).toContain(expected)
  })

  test('field conditions receive recordKey paired with record (input.recordKey == "ord-1")', () => {
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

    const scopeFor = (id: string, note: string) => makeScope(condUx, {
      reads: new Map([["order-detail", { kind: "ok", document: { id, values: { special_note: note } }, freshness: "snapshot" }]]),
      evaluate: syncEval,
    })
    const scopeOrd1 = scopeFor("ord-1", "Match")
    const scopeOrd2 = scopeFor("ord-2", "Other")
    expect(evaluateFieldCondition(scopeOrd1, condField.visibleWhen, {}, false)).toBe(true)
    expect(evaluateFieldCondition(scopeOrd2, condField.visibleWhen, {}, true)).toBe(false)

    expect(renderToStaticMarkup(<Node scope={scopeOrd1} node={condNode} />)).toContain('data-field="f-special" data-control="text"')
    const markup2 = renderToStaticMarkup(<Node scope={scopeOrd2} node={condNode} />)
    expect(markup2).toContain('type="hidden"')
    expect(markup2).toContain('data-field="f-special" data-control="hidden"')
  })

  test("pending state disables submit button; server refusals render RefusalCategory + server message across all closed states", () => {
    const pendingMarkup = renderNode(formUx, formNode, {
      stateId: "saving",
      calls: new Map<string, BindingCallState>([["submit-order", { kind: "sending", sent: { input: "{}", key: "k1" } }]]),
    })
    for (const expected of ['data-pending="true"', 'aria-disabled="true"', 'data-call="sending"', "Submitting…"]) {
      expect(pendingMarkup).toContain(expected)
    }

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
      const refusedMarkup = renderNode(formUx, formNode, { stateId: "failed", calls })
      for (const expected of ['role="alert"', 'data-call="refused"', `data-category="${category}"`, `data-code="err.${category}"`, expectedLabel, `Server said ${category}.`]) {
        expect(refusedMarkup).toContain(expected)
      }
    }

    const otherStates: BindingCallState[] = [
      { kind: "pending_approval", decision: { id: "d1" } },
      { kind: "committed", receipt: {}, undo: null },
      { kind: "not_served" },
      { kind: "unavailable", sent: { input: "{}", key: "k1" } },
      { kind: "timed_out", sent: { input: "{}", key: "k1" }, message: "Timed out." },
    ]
    for (const state of otherStates) {
      expect(renderNode(formUx, formNode, { calls: new Map([["submit-order", state]]) })).toContain(`data-call="${state.kind}"`)
    }

    const selectRefusedMarkup = renderNode(formUx, formNode, {
      reads: new Map<string, BindingReadState>([
        ["currencies", { kind: "refused", refusal: { code: "authz.denied", category: "permission", document: { message: "Cannot list currencies." } } }],
      ]),
    })
    expect(selectRefusedMarkup).toContain('data-read="refused"')
    expect(selectRefusedMarkup).toContain("Cannot list currencies.")
  })

  test("L6 agent:confirm marks form submit button when effects are unknown or consequential", () => {
    expect(renderNode(formUx, formNode, { effects: () => undefined })).toContain('data-agent-confirm="canon.order.create.action: its effects are not known"')
  })

  test("runtime submits form draft via machine event and direct submitForm, blocks duplicate submits, and never checks permissions client-side", async () => {
    const exactAmount = "999999999999999999.99"
    const sent: Array<{ actionId: string; input: Fields; key: string }> = []
    let release!: (answer: Classified) => void
    const { runtime } = startRuntime({
      ux: formUx,
      calls: {
        call: (actionId, input, options) => {
          sent.push({ actionId, input, key: options.idempotencyKey })
          return new Promise<Classified>((resolve) => { release = resolve })
        },
      },
      actionMap: { "canon.order.create.action": "canon.order.create" },
    })
    expect(needsEngine(formUx, baseMachine)).toBe(true)

    runtime.submitForm("create-order", "submit-order", { memo: "Q4", amount: exactAmount, quantity: 3 })
    expect(runtime.view().calls.get("submit-order")?.kind).toBe("sending")
    runtime.submitForm("create-order", "submit-order", { memo: "Q4", amount: exactAmount, quantity: 3 })
    await flush()
    expect(sent).toHaveLength(1)
    expect(sent[0]!.actionId).toBe("canon.order.create")
    expect(sent[0]!.input).toEqual({ memo: "Q4", amount: exactAmount, quantity: 3 })
    expect(typeof sent[0]!.input.quantity).toBe("number")

    release({ outcome: "refused", category: "permission", payload: { code: "authz.denied" }, body: { code: "authz.denied", message: "Actor lacks canon.order.create." } })
    await flush()
    expect(runtime.view().calls.get("submit-order")).toMatchObject({ kind: "refused", refusal: { code: "authz.denied", category: "permission" } })

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
    const editMachine = makeSubMachine({ bindingId: "order-detail", subEventId: "live-update", readyStateId: "editing" })
    const editFormDef: WidgetForm = {
      formId: "edit-order",
      mode: "edit",
      dataBindingId: "order-detail",
      inputNames: ["memo"],
      submitBindingId: "save-edit",
      submit: { label: { en: "Save" }, confirmation: { severity: "safe", strength: "none" } },
      versionConflictBinding: { property: { bindingId: "order-detail", propertyId: "version" }, inputName: "expectedVersion", onConflict: "reject_and_retain_draft" },
    }
    const editUx = makeUx(
      { nodeId: "edit-form", component: { kind: "form", formId: "edit-order" }, children: [] },
      {
        dataBindings: [{ bindingId: "order-detail", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_runtime_detail", pageId: "main", parameterId: "orderId" } }],
        forms: [editFormDef],
        actionBindings: [{
          bindingId: "save-edit",
          action: { id: "canon.order.edit.action", version: "0.1.0" },
          inputMapping: { id: "input.recordKey", memo: "input.draft.memo", expectedVersion: "input.record.version" },
          inputs: { draftFormId: "edit-order", recordBindingId: "order-detail" },
          requiredCapability: "canon.order.edit",
        }],
      },
      editMachine,
    )
    const { runtime, sent, push } = startRuntime({
      ux: editUx,
      reader: { kind: "live", widget: () => ({ queryKey: ["detail"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: revA }) }) },
      context: { orderId: "ord-1" },
      objectType: "canon.Order",
      actionMap: { "canon.order.edit.action": "canon.order.edit" },
      subscribe: true,
    })
    await flush()
    expect(runtime.view().reads.get("order-detail")).toMatchObject({ kind: "ok", document: revA })

    const pinnedRevA = { bindingId: "order-detail", document: revA }
    runtime.setDraft("edit-order", { memo: "User edit on revA" }, pinnedRevA)
    push(revB)
    expect(runtime.view().reads.get("order-detail")).toMatchObject({ kind: "ok", document: revB })

    runtime.submitForm("edit-order", "save-edit", { memo: "User edit on revA" }, pinnedRevA)
    await flush()
    expect(runtime.view().fault).toBeNull()
    expect(sent.map(({ actionId, input }) => ({ actionId, input }))).toEqual([
      { actionId: "canon.order.edit", input: { id: "ord-1", memo: "User edit on revA", expectedVersion: 1 } },
    ])
    runtime.dispose()
  })

  test("direct submitForm enforces a bounded deadline on stalled engine or transport and reuses idempotency key on retry", async () => {
    const keys: string[] = []
    const hangingCalls: WidgetCalls = { call: async (_id, _input, opts) => { keys.push(opts.idempotencyKey); return new Promise(() => {}) } }
    const engineClock = manualClock()
    const { runtime: stalledEngine } = startRuntime({
      ux: formUx,
      clock: engineClock,
      calls: hangingCalls,
      engine: lazyEngine(() => new Promise(() => {})),
      actionMap: { "canon.order.create.action": "canon.order.create" },
    })
    stalledEngine.submitForm("create-order", "submit-order", { memo: "Stalled eval", amount: "10.00", quantity: 1 })
    expect(stalledEngine.view().calls.get("submit-order")?.kind).toBe("sending")
    engineClock.advance(DEFAULT_SUBMIT_DEADLINE_MS)
    await flush()
    expect(stalledEngine.view().calls.get("submit-order")?.kind).toBe("timed_out")
    expect(keys).toEqual([])
    stalledEngine.dispose()

    const transportClock = manualClock()
    const { runtime: stalledTransport } = startRuntime({
      ux: formUx,
      clock: transportClock,
      calls: hangingCalls,
      actionMap: { "canon.order.create.action": "canon.order.create" },
    })
    stalledTransport.submitForm("create-order", "submit-order", { memo: "Slow cell", amount: "10.00", quantity: 1 })
    await flush()
    expect(stalledTransport.view().calls.get("submit-order")?.kind).toBe("sending")
    transportClock.advance(DEFAULT_SUBMIT_DEADLINE_MS)
    await flush()
    expect(stalledTransport.view().calls.get("submit-order")?.kind).toBe("timed_out")
    stalledTransport.submitForm("create-order", "submit-order", { memo: "Slow cell", amount: "10.00", quantity: 1 })
    await flush()
    expect(keys).toHaveLength(2)
    expect(keys[0]).toBe(keys[1])
    stalledTransport.dispose()
  })

  test("needsEngine returns true when a page's only expression is a non-literal field condition and false for literals", () => {
    const bareMachine: Machine = { machineVersion: "0.1", initialStateId: "idle", fields: [], events: [], states: [{ stateId: "idle", role: "idle" }], transitions: [] }
    const bareForm: WidgetForm = { formId: "cond-form", mode: "create", inputNames: ["reason"], submitBindingId: "bare-submit", submit: { label: { en: "Submit" }, confirmation: { severity: "safe", strength: "none" } } }
    const node: WidgetNode = { nodeId: "cond-form-node", component: { kind: "form", formId: "cond-form" }, children: [{ nodeId: "n-reason", component: { kind: "field", fieldId: "f-reason" }, children: [] }] }
    const build = (fieldExtra: Partial<FieldPresentation>) => makeUx(
      node,
      {
        forms: [bareForm],
        actionBindings: [{ bindingId: "bare-submit", action: { id: "a", version: "0.1.0" }, inputMapping: {}, inputs: {}, requiredCapability: "a" }],
        fields: [{ fieldId: "f-reason", target: { kind: "form_input", formId: "cond-form", inputName: "reason" }, label: { en: "Reason" }, control: "text", ...fieldExtra }],
      },
      bareMachine,
    )
    expect(needsEngine(build({ requiredWhen: { expression: "true", inputs: {} } }), bareMachine)).toBe(false)
    expect(needsEngine(build({ visibleWhen: { expression: "input.draft.expedite == true", inputs: { draftFormId: "cond-form" } } }), bareMachine)).toBe(true)
  })
})

describe("Capabilities parity and zero-Radix invariant", () => {
  const renderApp = (ux: WidgetUx) => renderToStaticMarkup(
    <NornAppRenderer
      ux={ux}
      workspace={{ kind: "deciding" }}
      calls={{ call: async () => ({ outcome: "not_served", payload: {} }) }}
      engine={() => Promise.reject(new Error("unused"))}
      objectTypeOf={() => undefined}
      actionIdOf={() => undefined}
    />,
  )

  test("RENDERED_COMPONENTS includes dialog and form, and combined vocab v2 page has zero missing capabilities", () => {
    expect(RENDERED_COMPONENTS).toContain("dialog")
    expect(RENDERED_COMPONENTS).toContain("form")
    for (const cap of ["component:dialog", "component:form", "view:form"]) expect(RENDERER_CAPABILITIES.has(cap)).toBe(true)

    const combined = makeUx(rootNode([
      sectionNode("t1", "Tab 1", { kind: "tab", order: 1 }),
      { nodeId: "l1", component: { kind: "list", dataBindingId: "items", fields: [], selection: "multiple" }, children: [] },
      { nodeId: "d1", component: { kind: "dialog", bindingId: "submit-order", blocking: true, dismissalMethods: ["escape"] }, children: [] },
      { nodeId: "f1", component: { kind: "form", formId: "create-order" }, children: [] },
    ]))
    const required = requiredCapabilities(combined)
    for (const cap of ["component:dialog", "component:form", "component:list", "component:section"]) expect(required).toContain(cap)
    expect(missingCapabilities(combined)).toEqual([])
  })

  test("NornAppRenderer refuses contracts referencing input.recordVersion or unformatted number controls whole", () => {
    const recVersionMarkup = renderApp(makeUx(rootNode(), {
      actionBindings: [{ bindingId: "edit-parent", action: { id: "canon.order.edit.action", version: "0.1.0" }, inputMapping: { expectedVersion: "input.recordVersion" }, inputs: { recordBindingId: "order" }, requiredCapability: "canon.order.edit" }],
    }))
    expect(recVersionMarkup).toContain('data-norn-refused=""')
    expect(recVersionMarkup).toContain("expr:recordVersion (action:edit-parent.expectedVersion)")

    const numMarkup = renderApp(makeUx(rootNode(), {
      fields: [{ fieldId: "f-qty", target: { kind: "form_input", formId: "f", inputName: "qty" }, label: { en: "Qty" }, control: "number" }],
    }))
    expect(numMarkup).toContain('data-norn-refused=""')
    expect(numMarkup).toContain("field:number:unformatted (f-qty)")
  })

  test("referencesRecordVersion detects parenthesized, optional, indexed, and conditional accesses while ignoring literals, arrays, and nested properties", () => {
    const positive = [
      "input.recordVersion",
      "(input).recordVersion",
      "(true ? input : input).recordVersion",
      "((input))['recordVersion']",
      '((input))["recordVersion"]',
      "input?.recordVersion",
      'input?.["recordVersion"]',
      "[input][0].recordVersion",
      "({ wrapped: input }).wrapped.recordVersion",
      '({ wrapped: input })["wrapped"]["recordVersion"]',
      "({ input }).input.recordVersion",
      '({ ["wrapped"]: input }).wrapped.recordVersion',
      '({ 10: 0, ["wrapped"]: input }).wrapped.recordVersion',
      "({ 10: input })[10].recordVersion",
      '({ 10: input })["10"].recordVersion',
      '({ 10.: input })["10."].recordVersion',
      '({ "v1.00": input })[`v${1.00}`].recordVersion',
      '({ "v2.00": input, v2: input.record })[`v${1.00 + 1}`].recordVersion',
      '({ "v700000000000000.2": input, "v700000000000000.3": input.record })[`v${0.1 * 7000000000000002}`].recordVersion',
      '({ "v2.0000000000000": input, v2: input.record })[`v${1.0000000000000 + 1}`].recordVersion',
      '({ [`v${2 / 1}`]: input })["v2"].recordVersion',
      "({ v2: input.record, [input.draft.key]: input }).v2.recordVersion",
      '({ keep: input, [({ which: "other", [input.draft.key]: "keep" }).which]: input.record }).keep.recordVersion',
      '({ v512: input, v64: input.record })[`v${2 ** 3 ** 2}`].recordVersion',
      "(null ?? !false ? input : input).recordVersion",
      "`${input.recordVersion}`",
      "`\\${input.recordVersion}`",
      'input["record" + "Version"]',
      "(input.record.amount > 10 ? input : input).recordVersion",
      "(input.record.amount in [1..5] ? input : input).recordVersion",
      "(input.record.amount in [1..5) ? input : input).recordVersion",
      "(input.record.amount in (1..5] ? input : input).recordVersion",
      "(input.record.rate >= 3.14 ? input : input).recordVersion",
    ]
    const negative = [
      '"input.recordVersion"',
      "'input.recordVersion'",
      "input.record.version",
      'input.draft.kind in ["recordVersion"]',
      '["recordVersion"]',
      "input.record.recordVersion",
      "((input).record).recordVersion",
      'input.draft["recordVersion"]',
      "({ v12: input, v3: input.record })[`v${1 + 2}`].recordVersion",
      "({ v36: input, v18: input.record })[`v${2 * 3 ** 2}`].recordVersion",
      '({ keep: input, [input.draft.flag ? "keep" : "keep"]: input.record }).keep.recordVersion',
    ]
    for (const expr of positive) expect(referencesRecordVersion(expr), expr).toBe(true)
    for (const expr of negative) expect(referencesRecordVersion(expr), expr).toBe(false)
  })

  test("runtime.readSecurity synchronously captures withholding across batched ok -> pending transitions", async () => {
    const revVisible = { id: "ord-1", values: { secret: "plain" } }
    const revWithheld = { id: "ord-1", values: {}, withheldProperties: ["secret"] }
    let resolveReread!: (value: Classified) => void
    let reads = 0

    const batchMachine = makeSubMachine({
      bindingId: "order-detail",
      extraTransitions: [{ transitionId: "t-sub", from: "ready", on: "sub", target: "loading", effects: [{ kind: "start", call: { kind: "read", bindingId: "order-detail" } }] }],
    })
    const ux = makeUx(
      rootNode(),
      { dataBindings: [{ bindingId: "order-detail", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_runtime_detail", pageId: "main", parameterId: "orderId" } }] },
      batchMachine,
    )
    const { runtime, push } = startRuntime({
      ux,
      reader: {
        kind: "live",
        widget: () => ({
          queryKey: ["order", reads],
          queryFn: () => {
            reads += 1
            return reads === 1
              ? Promise.resolve({ outcome: "accepted" as const, payload: {}, body: revVisible })
              : new Promise<Classified>((resolve) => { resolveReread = resolve })
          },
        }),
      },
      calls: { call: async () => ({ outcome: "not_served", payload: {} }) },
      context: { orderId: "ord-1" },
      objectType: "canon.Order",
      subscribe: true,
    })
    await flush()
    expect(runtime.view().reads.get("order-detail")?.kind).toBe("ok")

    push(revWithheld)
    expect(runtime.view().reads.get("order-detail")?.kind).toBe("pending")
    expect(runtime.view().readSecurity?.get("order-detail")?.withheldProperties).toContain("secret")
    expect(resolveFieldPrefillSecurity(
      { value: undefined, masked: undefined, withheld: false },
      { value: "plain", masked: undefined, withheld: false },
      true,
      false,
      { value: undefined, masked: undefined, withheld: true },
    ).withheld).toBe(true)
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

    const ratcheted = resolveFieldPrefillSecurity(maskedLive, visible, true, true)
    expect(ratcheted.masked).toEqual(masked)
    expect(resolveFieldPrefillSecurity(pendingEmpty, ratcheted, true, false).masked).toEqual(masked)
    expect(resolveFieldPrefillSecurity(visible, withheldLive, false, true)).toMatchObject({ withheld: false, value: "raw" })
    expect(resolveFieldPrefillSecurity(pendingEmpty, withheldLive, false, false).withheld).toBe(true)
  })

  test("synchronous selection reconciliation keys by envelope row.id so colliding values.id never substitutes another row", async () => {
    const rowA = { id: "env-A", values: { id: "env-B", name: "Alpha" } }
    const rowB = { id: "env-B", values: { id: "env-B", name: "Beta" } }
    const subMachine = makeSubMachine({
      bindingId: "orders",
      subEventId: "list-changed",
      extraStates: [{ stateId: "acting", role: "pending", awaits: { kind: "action", bindingId: "act-selected" }, deadlineMs: 5000 }],
      extraTransitions: [
        { transitionId: "t-sub", from: "ready", on: "list-changed", target: "acting", effects: [{ kind: "start", call: { kind: "action", bindingId: "act-selected" } }] },
        { transitionId: "t-act-ok", from: "acting", on: "accepted", target: "ready" },
      ],
    })
    const subUx = makeUx(
      { nodeId: "orders-list", component: { kind: "list", dataBindingId: "orders", fields: [{ bindingId: "orders", propertyId: "name" }], selection: "single" }, children: [] },
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
    const { runtime, sent, push } = startRuntime({
      ux: subUx,
      reader: { kind: "live", widget: () => ({ queryKey: ["orders"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: { records: [rowA, rowB] } }) }) },
      objectType: "canon.Order",
      actionMap: { "canon.order.act.action": "canon.order.act" },
      subscribe: true,
    })
    await flush()
    runtime.setSelection("orders", [flattenSelectionRecord(rowA)], ["env-A"])
    push({ records: [rowB] })
    await flush()
    expect(sent.map(({ actionId, input }) => ({ actionId, input }))).toEqual([{ actionId: "canon.order.act", input: { selected: [] } }])

    runtime.setSelection("orders", [flattenSelectionRecord(rowB)], ["env-B"])
    const envelopeLessImpostor = { values: { id: "env-B", name: "Impostor" } }
    expect(envelopeRowId(envelopeLessImpostor)).toBeNull()
    expect(reconcileSelection(["env-B"], [envelopeLessImpostor])).toEqual({ ids: [], records: [], changed: true })
    push({ records: [envelopeLessImpostor] })
    await flush()
    expect(sent.map(({ actionId, input }) => ({ actionId, input }))).toEqual([
      { actionId: "canon.order.act", input: { selected: [] } },
      { actionId: "canon.order.act", input: { selected: [] } },
    ])
    runtime.dispose()
  })

  test("mergeReadSecurity preserves mode:'refuse' against later 'redacted', and readSecurityLog resets cleanly on restored ok reads", async () => {
    const revRefused = { id: "ord-1", values: {}, masked: { secret: { mode: "refuse", markings: ["pii"], revealable: false } } }
    const revRedacted = { id: "ord-1", values: { secret: "***" }, masked: { secret: { mode: "redacted", markings: ["pii"], revealable: false } } }
    const revRestored = { id: "ord-1", values: { secret: "visible-again" } }

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
    expect(mergedPrefill).toMatchObject({ withheld: true, masked: { mode: "refuse" } })

    const liveMachine = makeSubMachine({
      bindingId: "order-detail",
      extraTransitions: [{ transitionId: "t-sub", from: "ready", on: "sub", target: "ready" }],
    })
    const ux = makeUx(
      rootNode(),
      { dataBindings: [{ bindingId: "order-detail", object: { id: "canon.Order.entity", version: "0.1.0" }, query: { kind: "object_runtime_detail", pageId: "main", parameterId: "orderId" } }] },
      liveMachine,
    )
    const { runtime, push } = startRuntime({
      ux,
      reader: { kind: "live", widget: () => ({ queryKey: ["order"], queryFn: async () => ({ outcome: "accepted", payload: {}, body: revRestored }) }) },
      calls: { call: async () => ({ outcome: "not_served", payload: {} }) },
      context: { orderId: "ord-1" },
      objectType: "canon.Order",
      subscribe: true,
    })
    await flush()
    const seqInitial = runtime.view().readSecurityLog?.get("order-detail")?.latestSeq ?? 0
    expect(seqInitial).toBeGreaterThan(0)

    push(revRefused)
    push(revRedacted)
    const sinceInitial = securitySince(runtime.view().readSecurityLog?.get("order-detail"), seqInitial)
    expect((sinceInitial?.masked.secret as { mode?: string })?.mode).toBe("refuse")
    expect(sinceInitial?.withheldProperties).toContain("secret")

    push(revRestored)
    const seqRestored = runtime.view().readSecurityLog?.get("order-detail")?.latestSeq ?? 0
    expect(seqRestored).toBeGreaterThan(seqInitial)
    expect(securitySince(runtime.view().readSecurityLog?.get("order-detail"), seqRestored)).toBeUndefined()

    for (let i = 0; i < 300; i += 1) {
      push({
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
      expect(readFileSync(file, "utf8")).not.toMatch(/@radix-ui\//)
    }
  })
})
