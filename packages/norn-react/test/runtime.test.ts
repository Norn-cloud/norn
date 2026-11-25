// The page runtime without React (#1762 milestone C): a failed or late engine is said, with a
// person's Retry that replays what waited; a step the engine refuses is the page's fault, not a
// frozen page; and everything a runtime read or sent stays in the workspace it was given.
import { describe, expect, test } from "bun:test"
import type { Classified, Clock } from "../src/machine/index.ts"
import { lazyEngine, type CheckerEvaluator } from "../src/machine/evaluate.ts"
import type { Fields, Machine } from "../src/machine/machine.ts"
import type { BindingRead, WidgetCalls, WidgetReader } from "../src/reads.ts"
import { pageKey } from "../src/renderer.tsx"
import { createPageRuntime, ENGINE_WAIT_MS, type RuntimeView } from "../src/runtime.ts"
import type { WidgetUx } from "../src/ux.ts"
import { ACTION, matrixArtifact } from "./fixtures/matrix-page.ts"

const ux = matrixArtifact().payload.ux as WidgetUx
const machine = ux.pages[0]!.machine as Machine

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

/** An engine that reads `input.<path>` and JSON literals, and refuses anything else (as the checker's engine refuses an unknown field). */
const pathEngine: CheckerEvaluator = {
  evaluateExpression(expression, inputJson) {
    const source = expression.trim()
    if (/^input(\.[A-Za-z_][A-Za-z0-9_]*)+$/.test(source)) {
      let value: unknown = JSON.parse(inputJson)
      for (const part of source.split(".").slice(1)) value = typeof value === "object" && value !== null ? (value as Record<string, unknown>)[part] : undefined
      return JSON.stringify(value === undefined ? { ok: false, error: { code: "unknown_field", message: `${source} names nothing.` } } : { ok: true, value })
    }
    try {
      return JSON.stringify({ ok: true, value: JSON.parse(source) as unknown })
    } catch {
      return JSON.stringify({ ok: false, error: { code: "parse", message: `${source} is not ZEN this stand-in reads.` } })
    }
  },
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

/** A reader that answers each read with the next queued document (or holds it). */
function reader(documents: unknown[]): WidgetReader & { readonly asked: BindingRead[] } {
  const asked: BindingRead[] = []
  return {
    kind: "live",
    asked,
    widget: (read) => ({
      queryKey: ["widget", read],
      queryFn: () => {
        asked.push(read)
        const document = documents.shift()
        return document === undefined ? new Promise<Classified>(() => {}) : Promise.resolve({ outcome: "accepted", payload: {}, body: document })
      },
    }),
  }
}

function calls(answer: Classified = { outcome: "accepted", payload: {}, body: { outcome: "committed" } }): WidgetCalls & { readonly sent: { readonly actionId: string; readonly input: Fields }[] } {
  const sent: { actionId: string; input: Fields }[] = []
  return { sent, call: async (actionId, input) => { sent.push({ actionId, input }); return answer } }
}

const host = () => ({ context: {}, objectTypeOf: () => "canon.ValuationPolicy", actionIdOf: (artifactId: string) => (artifactId === ACTION.id ? "canon.valuation_policy.set" : undefined) })

function run(options: { readonly engine: () => Promise<CheckerEvaluator>; readonly reader?: WidgetReader; readonly calls?: WidgetCalls; readonly ux?: WidgetUx; readonly machine?: Machine; readonly clock?: Clock }) {
  const views: RuntimeView[] = []
  const runtime = createPageRuntime({
    ux: options.ux ?? ux,
    machine: options.machine ?? machine,
    reader: options.reader ?? reader([{ records: [] }]),
    calls: options.calls ?? calls(),
    engine: lazyEngine(options.engine),
    host,
    clock: options.clock ?? manualClock(),
  }, (view) => views.push(view))
  return { runtime, views }
}

const edit = { kind: "user", eventId: "edit-note", payload: { value: "Checked twice" } } as const
const save = { kind: "user", eventId: "save" } as const

describe("the engine", () => {
  test("a failed load says so; the waiting steps wait in order, and a person's Retry replays them", async () => {
    let attempts = 0
    const { runtime } = run({ engine: async () => {
      attempts += 1
      if (attempts === 1) throw new Error("the engine's chunk did not load")
      return pathEngine
    } })
    await flush()
    expect(runtime.view().snapshot.stateId).toBe("browsing")
    // The note's assignment needs the engine: it waits, and asks for it.
    runtime.send(edit)
    await flush()
    expect(runtime.view().engine).toBe("failed")
    // Save needs no engine, yet it waits behind the note: nothing is reordered.
    runtime.send(save)
    expect(runtime.view().waiting).toBe(2)
    expect(runtime.view().snapshot.stateId).toBe("browsing")
    runtime.retryEngine()
    expect(runtime.view().engine).toBe("loading")
    await flush()
    expect(attempts).toBe(2)
    expect(runtime.view()).toMatchObject({ engine: "ready", waiting: 0, snapshot: { stateId: "confirming", fields: { note: "Checked twice" } } })
    runtime.dispose()
  })

  test("a step in an idle state waits ENGINE_WAIT_MS for the engine, then the page says it is unavailable; a late engine still replays", async () => {
    const clock = manualClock()
    let release: (engine: CheckerEvaluator) => void = () => {}
    const { runtime } = run({ clock, engine: () => new Promise((resolve) => { release = resolve }) })
    await flush()
    runtime.send(edit)
    expect(runtime.view().engine).toBe("loading")
    clock.advance(ENGINE_WAIT_MS - 1)
    expect(runtime.view().engine).toBe("loading")
    clock.advance(1)
    expect(runtime.view()).toMatchObject({ engine: "failed", waiting: 1, snapshot: { stateId: "browsing" } })
    release(pathEngine)
    await flush()
    expect(runtime.view()).toMatchObject({ engine: "ready", waiting: 0, snapshot: { fields: { note: "Checked twice" } } })
    runtime.dispose()
  })

  test("an outcome that waits in a pending state waits no longer than that state's deadline", async () => {
    const clock = manualClock()
    // Saving awaits the call under a 5s deadline; its answer's transition assigns through the engine.
    const pending: Machine = {
      machineVersion: "0.1",
      initialStateId: "idle",
      fields: [{ fieldId: "said", type: "text", required: true, initial: "" }],
      events: [
        { eventId: "save", source: { kind: "user", nodeId: "save-node" } },
        { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
        { eventId: "timeout", source: { kind: "outcome", outcome: "timeout" } },
      ],
      states: [{ stateId: "idle", role: "idle" }, { stateId: "saving", role: "pending", awaits: { kind: "action", bindingId: "save" }, deadlineMs: 5000 }, { stateId: "saved", role: "idle" }],
      transitions: [
        { transitionId: "save", from: "idle", on: "save", target: "saving", effects: [{ kind: "start", call: { kind: "action", bindingId: "save" } }] },
        { transitionId: "saved", from: "saving", on: "accepted", target: "saved", assign: { said: "input.event.code" } },
        { transitionId: "late", from: "saving", on: "timeout", target: "idle" },
      ],
    }
    const { runtime } = run({ clock, machine: pending, engine: () => new Promise(() => {}) })
    runtime.send(save)
    await flush()
    // The call answered in time; its outcome waits for the engine.
    expect(runtime.view()).toMatchObject({ waiting: 1, snapshot: { stateId: "saving" } })
    expect(runtime.view().calls.get("save")?.kind).toBe("committed")
    clock.advance(4999)
    expect(runtime.view().engine).toBe("loading")
    clock.advance(1)
    expect(runtime.view().engine).toBe("failed")
    runtime.dispose()
  })

  test("a step the engine refuses is the page's fault: it stays where it was, and the person's next step clears it", async () => {
    const refusing: Machine = { ...machine, transitions: machine.transitions.map((transition) => (transition.transitionId === "edit-note" ? { ...transition, assign: { note: "input.event.missing" } } : transition)) }
    const { runtime } = run({ machine: refusing, engine: async () => pathEngine })
    await flush()
    runtime.send(edit)
    await flush()
    expect(runtime.view().fault?.message).toBe("input.event.missing names nothing.")
    expect(runtime.view()).toMatchObject({ waiting: 0, engine: "ready", snapshot: { stateId: "browsing", fields: { note: "" } } })
    runtime.send(save)
    expect(runtime.view()).toMatchObject({ fault: null, snapshot: { stateId: "confirming" } })
    runtime.dispose()
  })
})

describe("one workspace's reads and calls stay in it", () => {
  // The save call maps the record its binding names (`input.record`), read in the runtime's workspace.
  const mapped: WidgetUx = { ...ux, actionBindings: ux.actionBindings.map((binding) => ({ ...binding, inputs: { recordBindingId: "records" }, inputMapping: { id: "input.record.id" } })) }
  const confirm = { kind: "user", eventId: "confirm" } as const

  test("a runtime for workspace B starts with nothing read or called, and maps only B's record into B's call", async () => {
    const callsA = calls()
    const a = run({ ux: mapped, reader: reader([{ values: { id: "a-1" } }]), calls: callsA, engine: async () => pathEngine })
    await flush()
    expect(a.runtime.view().reads.get("records")).toMatchObject({ kind: "ok", document: { values: { id: "a-1" } } })
    a.runtime.send(save)
    a.runtime.send(confirm)
    await flush()
    expect(callsA.sent).toEqual([{ actionId: "canon.valuation_policy.set", input: { id: "a-1" } }])
    expect(a.runtime.view().calls.get("save")?.kind).toBe("committed")
    // The workspace moves to B: the page gets a new runtime, given B's reader and calls.
    a.runtime.dispose()
    const readerB = reader([{ values: { id: "b-1" } }])
    const callsB = calls()
    const b = run({ ux: mapped, reader: readerB, calls: callsB, engine: async () => pathEngine })
    expect([b.views[0]!.reads.size, b.views[0]!.calls.size]).toEqual([0, 0])
    await flush()
    b.runtime.send(save)
    b.runtime.send(confirm)
    await flush()
    expect(callsB.sent).toEqual([{ actionId: "canon.valuation_policy.set", input: { id: "b-1" } }])
    expect(callsA.sent).toHaveLength(1)
    // No view B drew ever held A's record or A's call.
    expect(b.views.some((view) => JSON.stringify([...view.reads.values(), ...view.calls.values()]).includes("a-1"))).toBe(false)
    expect(readerB.asked).toHaveLength(1)
    b.runtime.dispose()
  })

  test("a disposed runtime's late answers change nothing", async () => {
    let answer: (classified: Classified) => void = () => {}
    const late: WidgetReader = { kind: "live", widget: () => ({ queryKey: ["late"], queryFn: () => new Promise<Classified>((resolve) => { answer = resolve }) }) }
    const { runtime, views } = run({ reader: late, engine: async () => pathEngine })
    const seen = views.length
    runtime.dispose()
    answer({ outcome: "accepted", payload: {}, body: { values: { id: "a-1" } } })
    await flush()
    expect(views).toHaveLength(seen)
  })

  test("a live page is keyed by its decided scope", () => {
    const scopeA = { orgId: "org_demo", environmentId: "analytics" }
    expect(pageKey({ kind: "live", scope: scopeA })).toBe(pageKey({ kind: "live", scope: { ...scopeA } }))
    expect(pageKey({ kind: "live", scope: scopeA })).not.toBe(pageKey({ kind: "live", scope: { ...scopeA, environmentId: "ops" } }))
    expect(pageKey({ kind: "recorded" })).not.toBe(pageKey({ kind: "live", scope: scopeA }))
  })
})
