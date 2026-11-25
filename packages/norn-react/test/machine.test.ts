// The runtime against the checker's own engine: guards and assignments evaluate in the WASM build
// of norn-checker (NORN_CHECKER_WASM_PKG), the same engine that type-checked them at admission.

import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

import { NO_HOST_ERROR } from "../../norn-sdk/src/client.ts"
import { type Classified, type Clock, type Evaluate, type Machine, type Msg, type Transport, classifyResponse, classifyText, createDriver, EngineNotReady, init, lazyEngine, update, walks, wasmEvaluate } from "../src/machine/index.ts"

// The engine is built by CI's integration phase (NORN_CHECKER_WASM_PKG); without it the
// engine-backed rows are skipped and `classifyResponse` still runs.
const pkg = process.env.NORN_CHECKER_WASM_PKG
const withEngine = pkg ? describe : describe.skip
const checker = pkg
  ? ((await import(pathToFileURL(join(pkg, "norn_checker_wasm.js")).href)) as {
      initSync(options: { module: Uint8Array }): unknown
      evaluateExpression(expression: string, inputJson: string, profile?: string): string
    })
  : null
checker?.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
if (!checker && process.env.NORN_CHECKER_WASM_REQUIRED) throw new Error("NORN_CHECKER_WASM_PKG is required")
const evaluate: Evaluate = checker ? wasmEvaluate(checker) : () => {
  throw new Error("no checker engine")
}

const machine = JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/ui/orders-approve.machine.json"), "utf8")) as Machine

withEngine("update", () => {
  test("a user event starts the awaited call under the state's deadline", () => {
    const opened = init(machine)
    expect(opened.snapshot).toEqual({ stateId: "browsing", fields: { lastCode: null }, entry: 0 })
    const step = update(machine, evaluate, opened.snapshot, { kind: "user", eventId: "approve" })
    expect(step.snapshot.stateId).toBe("approving")
    expect(step.cmds).toEqual([{ kind: "start", entry: 1, call: { kind: "action", bindingId: "approve" }, input: {}, deadlineMs: 15000 }])
  })

  test("a refusal assigns through the checker's engine; a stale outcome is dropped", () => {
    const approving = update(machine, evaluate, init(machine).snapshot, { kind: "user", eventId: "approve" }).snapshot
    const stale = update(machine, evaluate, approving, { kind: "outcome", entry: 0, outcome: "accepted" })
    expect(stale.fired).toBeNull()
    const refused = update(machine, evaluate, approving, {
      kind: "outcome",
      entry: 1,
      outcome: "refused",
      category: "permission",
      payload: { code: "authz.denied", retryable: false },
    })
    expect(refused.snapshot).toEqual({ stateId: "refused", fields: { lastCode: "authz.denied" }, entry: 2 })
  })

  test("an event the current state does not take is ignored", () => {
    const step = update(machine, evaluate, init(machine).snapshot, { kind: "user", eventId: "cancel" })
    expect(step.fired).toBeNull()
    expect(step.snapshot).toEqual(init(machine).snapshot)
  })
})

withEngine("walks", () => {
  test("every transition has a walk, and replaying it fires exactly that path", () => {
    const all = walks(machine, evaluate, (_, event) => (event.source.kind === "outcome" && event.source.outcome === "refused" ? [{ code: "law.refused", retryable: false }] : undefined))
    expect(all.map((walk) => walk.transitionId)).toEqual(machine.transitions.map((transition) => transition.transitionId))
    for (const walk of all) {
      let snapshot = init(machine).snapshot
      for (const step of walk.steps) {
        const next = update(machine, evaluate, snapshot, step.msg)
        expect(next.fired?.transitionId).toBe(step.transition.transitionId)
        snapshot = next.snapshot
      }
    }
  })
})

/** A clock the test advances by hand. */
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

/** A transport whose calls the test answers by hand. */
function manualTransport() {
  const calls: { key: string; signal: AbortSignal; resolve(value: Classified): void; reject(error: unknown): void }[] = []
  const transport: Transport = {
    call: (_call, _input, options) =>
      new Promise<Classified>((resolve, reject) => {
        calls.push({ key: options.idempotencyKey, signal: options.signal, resolve, reject })
        options.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")))
      }),
  }
  return { transport, calls }
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

withEngine("driver", () => {
  test("the deadline aborts the call and raises timeout; a retry replays the same key", async () => {
    const clock = manualClock()
    const { transport, calls } = manualTransport()
    let keys = 0
    const driver = createDriver({ machine, evaluate, transport, clock, newKey: () => `k${++keys}` })
    driver.dispatch({ kind: "user", eventId: "approve" })
    clock.advance(15000)
    await settle()
    expect(calls[0]?.signal.aborted).toBe(true)
    expect(driver.snapshot().stateId).toBe("failed")
    driver.dispatch({ kind: "user", eventId: "approve" })
    expect(calls[1]?.key).toBe("k1")
    calls[1]?.resolve({ outcome: "accepted", payload: {} })
    await settle()
    expect(driver.snapshot().stateId).toBe("browsing")
    driver.dispatch({ kind: "user", eventId: "approve" })
    expect(calls[2]?.key).toBe("k2")
    driver.dispose()
  })

  test("a cancel is a request: the outcome that follows decides", async () => {
    const { transport, calls } = manualTransport()
    const driver = createDriver({ machine, evaluate, transport, clock: manualClock() })
    driver.dispatch({ kind: "user", eventId: "approve" })
    driver.dispatch({ kind: "user", eventId: "cancel" })
    expect(driver.snapshot().stateId).toBe("approving")
    await settle()
    expect(calls[0]?.signal.aborted).toBe(true)
    expect(driver.snapshot().stateId).toBe("browsing")
    driver.dispose()
  })

  test("a 202 parks the page on the approval the server opened", async () => {
    const seen: Msg[] = []
    const { transport, calls } = manualTransport()
    const driver = createDriver({ machine, evaluate, transport, clock: manualClock(), onStep: (_, msg) => msg && seen.push(msg) })
    driver.dispatch({ kind: "user", eventId: "approve" })
    calls[0]?.resolve(classifyResponse(202, { outcome: "pending", decision: { id: "dec_1", expiresAtMs: 1 } }))
    await settle()
    expect(driver.snapshot().stateId).toBe("awaiting-approval")
    expect(seen.at(-1)).toEqual({ kind: "outcome", entry: 1, outcome: "pending_approval", payload: { decisionId: "dec_1" } })
    driver.dispose()
  })
})

describe("classifyResponse", () => {
  test("reads only what the server said", () => {
    expect(classifyResponse(200, { outcome: "committed" }).outcome).toBe("accepted")
    expect(classifyResponse(409, { code: "core.projection.answer_moved" })).toMatchObject({ outcome: "answer_moved", payload: { code: "core.projection.answer_moved" } })
    expect(classifyResponse(409, { code: "decision.resolved", category: "conflict" })).toMatchObject({ outcome: "refused", category: "conflict" })
    expect(classifyResponse(403, { code: "authz.denied" })).toMatchObject({ outcome: "refused", category: "permission" })
    expect(classifyResponse(422, { code: "law.refused", retryable: true })).toMatchObject({ outcome: "refused", category: "invalid", payload: { retryable: true } })
    expect(classifyResponse(503, undefined).outcome).toBe("network_failure")
    expect(classifyResponse(502, { message: "Bad gateway" }).outcome).toBe("network_failure")
  })

  // A route the host does not serve is `not_served`, never a network failure (#1762).
  test("a route the host does not serve is not_served", () => {
    expect(classifyResponse(404, undefined).outcome).toBe("not_served")
    expect(classifyResponse(404, { code: "route.not_found", message: "No such route." }).outcome).toBe("not_served")
    expect(classifyResponse(503, { error: NO_HOST_ERROR }).outcome).toBe("not_served")
    // A coded 404 is the cell's refusal (a record this caller cannot see), and a coded 503 is that refusal.
    expect(classifyResponse(404, { format: "norn/refusal@2", code: "object.not_found", category: "visibility" })).toMatchObject({ outcome: "refused", category: "visibility" })
    expect(classifyResponse(503, { format: "norn/refusal@2", code: "action.receipts_unconfigured", category: "internal" })).toMatchObject({ outcome: "refused", category: "internal" })
    // A 503 from a gateway while the host restarts is a cell that did not answer.
    expect(classifyResponse(503, { error: "Service Unavailable" }).outcome).toBe("network_failure")
  })
  test("a success whose body is no JSON (a host's HTML fallback page) is no answer, never accepted", () => {
    expect(classifyText(200, "<!doctype html><html><body>Studio</body></html>")).toEqual({ outcome: "network_failure", payload: {} })
    expect(classifyText(200, '{"outcome":"committed"}').outcome).toBe("accepted")
    expect(classifyText(204, "").outcome).toBe("accepted")
    expect(classifyText(404, "<html>Not found</html>").outcome).toBe("not_served")
  })
})

// Refusal codes (#1762, Fable should 3): a handler with a `code` fires only for that exact code, and
// outranks its category's handler for that code alone; it never swallows the category.
describe("refusal codes", () => {
  const refused = (eventId: string, refine: { readonly category?: "precondition" | "conflict"; readonly code?: string }) => ({ eventId, source: { kind: "outcome" as const, outcome: "refused" as const, ...refine } })
  const coded: Machine = {
    machineVersion: "0.1",
    initialStateId: "idle",
    fields: [],
    events: [
      { eventId: "send", source: { kind: "user", nodeId: "send-node" } },
      // Declared before the coded handlers, so only the ranking, not the order, lets a code win.
      refused("any-precondition", { category: "precondition" }),
      refused("stock-short", { category: "precondition", code: "stock.short" }),
      refused("locked", { code: "record.locked" }),
      refused("refused", {}),
      { eventId: "accepted", source: { kind: "outcome", outcome: "accepted" } },
    ],
    states: [
      { stateId: "idle", role: "idle" },
      { stateId: "sending", role: "pending", awaits: { kind: "action", bindingId: "send" }, deadlineMs: 1000 },
      { stateId: "short", role: "error" },
      { stateId: "precondition", role: "error" },
      { stateId: "locked", role: "error" },
      { stateId: "other", role: "error" },
    ],
    transitions: [
      { transitionId: "send", from: "idle", on: "send", target: "sending", effects: [{ kind: "start", call: { kind: "action", bindingId: "send" } }] },
      { transitionId: "on-precondition", from: "sending", on: "any-precondition", target: "precondition" },
      { transitionId: "on-short", from: "sending", on: "stock-short", target: "short" },
      { transitionId: "on-locked", from: "sending", on: "locked", target: "locked" },
      { transitionId: "on-refused", from: "sending", on: "refused", target: "other" },
      { transitionId: "on-accepted", from: "sending", on: "accepted", target: "idle" },
    ],
  }
  const none: Evaluate = () => {
    throw new Error("this machine evaluates no expressions")
  }
  const sending = update(coded, none, init(coded).snapshot, { kind: "user", eventId: "send" }).snapshot
  const answer = (category: "precondition" | "conflict" | "limit", code?: string) =>
    update(coded, none, sending, { kind: "outcome", entry: sending.entry, outcome: "refused", category, ...(code === undefined ? {} : { code }) }).snapshot.stateId

  test("a coded handler fires for its code and outranks its category's handler", () => {
    expect(answer("precondition", "stock.short")).toBe("short")
  })
  test("a coded handler never swallows its category: another code takes the category handler", () => {
    expect(answer("precondition", "stock.reserved")).toBe("precondition")
    expect(answer("precondition")).toBe("precondition")
  })
  test("a coded handler with a category matches only that category", () => {
    expect(answer("conflict", "stock.short")).toBe("other")
  })
  test("a coded handler without a category matches its code in any category, and nothing else", () => {
    expect(answer("conflict", "record.locked")).toBe("locked")
    expect(answer("limit", "record.locked")).toBe("locked")
    expect(answer("limit", "record.busy")).toBe("other")
  })
  test("the driver carries the server's code, and every refusal handler gets a walk that fires it", async () => {
    const seen: Msg[] = []
    const driver = createDriver({
      machine: coded,
      evaluate: none,
      transport: { call: async () => classifyResponse(422, { format: "norn/refusal@2", code: "stock.short", category: "precondition" }) },
      onStep: (_step, msg) => {
        if (msg) seen.push(msg)
      },
    })
    driver.dispatch({ kind: "user", eventId: "send" })
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(driver.snapshot().stateId).toBe("short")
    expect(seen.at(-1)).toMatchObject({ kind: "outcome", outcome: "refused", category: "precondition", code: "stock.short" })
    driver.dispose()
    expect(walks(coded, none).map((walk) => walk.transitionId)).toEqual(coded.transitions.map((transition) => transition.transitionId))
  })
})

// The lazy engine (#1762 milestone C): guard-free transitions run in plain TypeScript at once; a
// message whose transition needs the engine waits, with every message after it, and replays in
// order once the engine is here. A constant option (a Boolean or text literal) needs no engine.
describe("lazy engine", () => {
  const guarded: Machine = {
    machineVersion: "0.1",
    initialStateId: "idle",
    fields: [{ fieldId: "ready", type: "boolean", required: true, initial: true }],
    events: [
      { eventId: "open", source: { kind: "user", nodeId: "open-node" } },
      { eventId: "go", source: { kind: "user", nodeId: "go-node" } },
      { eventId: "back", source: { kind: "user", nodeId: "back-node" } },
    ],
    states: [{ stateId: "idle", role: "idle" }, { stateId: "open", role: "idle" }, { stateId: "gone", role: "idle" }],
    transitions: [
      { transitionId: "open", from: "idle", on: "open", target: "open" },
      { transitionId: "go", from: "open", on: "go", target: "gone", guard: "input.fields.ready" },
      { transitionId: "back", from: "gone", on: "back", target: "idle" },
    ],
  }
  const fake = { evaluateExpression: (expression: string, inputJson: string) => JSON.stringify({ ok: true, value: (JSON.parse(inputJson) as { fields: Record<string, unknown> }).fields[expression.replace("input.fields.", "")] }) }

  test("guard-free work runs before the engine; guarded work waits and replays in order", async () => {
    let release: (checker: typeof fake) => void = () => {}
    const engine = lazyEngine(() => new Promise((resolve) => { release = resolve }))
    let deferred = 0
    const driver = createDriver({ machine: guarded, evaluate: engine.evaluate, transport: { call: () => Promise.reject(new Error("no calls")) }, onDefer: () => { deferred += 1 } })
    expect(engine.state()).toBe("idle")
    driver.dispatch({ kind: "user", eventId: "open" })
    expect(driver.snapshot().stateId).toBe("open")
    driver.dispatch({ kind: "user", eventId: "go" })
    driver.dispatch({ kind: "user", eventId: "back" })
    expect(driver.snapshot().stateId).toBe("open")
    expect(driver.deferred()).toBe(2)
    expect(deferred).toBe(2)
    const loaded = engine.load()
    expect(engine.state()).toBe("loading")
    release(fake)
    await loaded
    expect(engine.state()).toBe("ready")
    driver.resume()
    expect(driver.deferred()).toBe(0)
    expect(driver.snapshot().stateId).toBe("idle")
    driver.dispose()
  })

  // A guard or assignment the engine refuses fires nothing, whether the message ran at once or waited
  // for the engine: replay ends where eager evaluation ends, and the messages after it still run.
  const refusing: Machine = {
    ...guarded,
    fields: [...guarded.fields, { fieldId: "note", type: "text", required: true, initial: "" }],
    events: [...guarded.events, { eventId: "edit", source: { kind: "user", nodeId: "edit-node" } }, { eventId: "close", source: { kind: "user", nodeId: "close-node" } }],
    transitions: [
      { transitionId: "open", from: "idle", on: "open", target: "open" },
      { transitionId: "go", from: "open", on: "go", target: "gone", guard: "input.fields.missing" },
      { transitionId: "edit", from: "open", on: "edit", target: "open", assign: { note: "input.fields.missing" } },
      { transitionId: "close", from: "open", on: "close", target: "idle" },
    ],
  }
  const strict = {
    evaluateExpression: (expression: string, inputJson: string) => {
      const value = (JSON.parse(inputJson) as { fields: Record<string, unknown> }).fields[expression.replace("input.fields.", "")]
      return JSON.stringify(value === undefined ? { ok: false, error: { code: "unknown_field", message: `${expression} names no field` } } : { ok: true, value })
    },
  }
  const messages: Msg[] = [{ kind: "user", eventId: "open" }, { kind: "user", eventId: "go" }, { kind: "user", eventId: "edit" }, { kind: "user", eventId: "close" }]

  test("a replayed guard or assignment the engine refuses is dropped and reported, and replay ends where eager evaluation does", async () => {
    let release: (checker: typeof strict) => void = () => {}
    const engine = lazyEngine(() => new Promise((resolve) => { release = resolve }))
    const refused: Msg[] = []
    const lazy = createDriver({ machine: refusing, evaluate: engine.evaluate, transport: { call: () => Promise.reject(new Error("no calls")) }, onError: (_error, msg) => { refused.push(msg) } })
    for (const msg of messages) lazy.dispatch(msg)
    expect(lazy.snapshot().stateId).toBe("open")
    expect(lazy.deferred()).toBe(3)
    const loaded = engine.load()
    release(strict)
    await loaded
    lazy.resume()
    expect(lazy.deferred()).toBe(0)
    expect(refused).toEqual([messages[1]!, messages[2]!])
    // The same messages with the engine already here.
    const eagerRefused: Msg[] = []
    const eager = createDriver({ machine: refusing, evaluate: engine.evaluate, transport: { call: () => Promise.reject(new Error("no calls")) }, onError: (_error, msg) => { eagerRefused.push(msg) } })
    for (const msg of messages) eager.dispatch(msg)
    expect(eagerRefused).toEqual(refused)
    expect(lazy.snapshot()).toEqual(eager.snapshot())
    expect(lazy.snapshot()).toEqual({ stateId: "idle", fields: { ready: true, note: "" }, entry: 2 })
    // The queue is not stuck: the page keeps taking messages.
    lazy.dispatch(messages[0]!)
    expect(lazy.snapshot().stateId).toBe("open")
    lazy.dispose()
    eager.dispose()
  })

  test("after a failed load, a retry reports loading until it settles", async () => {
    let attempts = 0
    const engine = lazyEngine(async () => {
      attempts += 1
      if (attempts === 1) throw new Error("chunk failed")
      return fake
    })
    await engine.load().catch(() => {})
    expect(engine.state()).toBe("failed")
    const retried = engine.load()
    expect(engine.state()).toBe("loading")
    await retried
    expect(engine.state()).toBe("ready")
    expect(attempts).toBe(2)
  })

  test("a constant literal needs no engine; anything else does", () => {
    const engine = lazyEngine(() => Promise.reject(new Error("never loaded")))
    const input = { fields: {}, event: {} }
    expect(engine.evaluate("true", input)).toBe(true)
    expect(engine.evaluate(" false ", input)).toBe(false)
    expect(engine.evaluate('"Sending"', input)).toBe("Sending")
    expect(() => engine.evaluate("1.50", input)).toThrow(EngineNotReady)
    expect(() => engine.evaluate('"a\\"b"', input)).toThrow(EngineNotReady)
    expect(() => engine.evaluate("input.fields.ready", input)).toThrow(EngineNotReady)
  })
})
