// The runtime against the checker's own engine: guards and assignments evaluate in the WASM build
// of norn-checker (NORN_CHECKER_WASM_PKG), the same engine that type-checked them at admission.

import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

import { type Classified, type Clock, type Machine, type Msg, type Transport, classifyResponse, createDriver, init, update, walks, wasmEvaluate } from "../src/machine/index.ts"

const pkg = process.env.NORN_CHECKER_WASM_PKG
if (!pkg) throw new Error("NORN_CHECKER_WASM_PKG must name the built checker WASM package")
const checker = (await import(pathToFileURL(join(pkg, "norn_checker_wasm.js")).href)) as {
  initSync(options: { module: Uint8Array }): unknown
  evaluateExpression(expression: string, inputJson: string, profile?: string): string
}
checker.initSync({ module: readFileSync(join(pkg, "norn_checker_wasm_bg.wasm")) })
const evaluate = wasmEvaluate(checker)

const machine = JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/ui/orders-approve.machine.json"), "utf8")) as Machine

describe("update", () => {
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

describe("walks", () => {
  test("every transition has a walk, and replaying it fires exactly that path", () => {
    const all = walks(machine, (_, event) => (event.source.kind === "outcome" && event.source.outcome === "refused" ? { code: "law.refused", retryable: false } : undefined))
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

describe("driver", () => {
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
    expect(classifyResponse(404, undefined).outcome).toBe("network_failure")
  })
})
