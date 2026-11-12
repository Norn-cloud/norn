import { expect, test } from "bun:test"
import type { ActionReceipt, ActionSummary, CallResult } from "../src/lib/actions.ts"
import { inFlight, init, offeredFor, undoOf, update, type Cmd, type Msg, type State } from "../src/lib/screen/call.ts"

// Properties of the renderer core over seeded random message sequences (D79: properties, not cases).

function rng(seed: number) {
  let s = seed >>> 0
  return () => { s = (s + 0x6d2b79f5) >>> 0; let t = s; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 2 ** 32 }
}
const pick = <T,>(r: () => number, xs: readonly T[]): T => xs[Math.floor(r() * xs.length)]!

const receipt = (seq: number, undo?: unknown): ActionReceipt => ({
  outcome: "committed", objectType: "learning.Episode", objectId: `ep_${seq}`, version: seq,
  receipt: { receiptHash: `h${seq}`, commitSeq: seq, recordedAtMs: seq }, ...(undo === undefined ? {} : { undo }),
} as ActionReceipt)

interface Step { readonly before: State; readonly msg: Msg; readonly after: State; readonly cmds: readonly Cmd[] }

/** One random session. The draft's input changes only on `edit`, as the view guarantees. */
function session(seed: number, length = 60): { readonly risky: boolean; readonly steps: readonly Step[] } {
  const r = rng(seed)
  const risky = r() < 0.5
  let keys = 0
  let seq = 0
  let draft = 0
  let state = init({ actionId: "learning.triage_episode", risky }, `k${keys++}`)
  const steps: Step[] = []
  for (let i = 0; i < length; i++) {
    const result = (): CallResult => pick(r, [
      () => ({ kind: "receipt", receipt: receipt(++seq, r() < 0.5 ? { actionId: "learning.retract_triage", input: { episodeId: `ep_${seq}` } } : undefined) }) as const,
      () => ({ kind: "refused", status: 422, code: "action.precondition_failed", message: "no", diagnostics: [] }) as const,
      () => ({ kind: "unavailable" }) as const,
      () => ({ kind: "no-cell" }) as const,
      () => ({ kind: "pending", decision: { id: `d${seq}`, expiresAtMs: 9 } }) as const,
    ])()
    const msg: Msg = pick(r, [
      // The view holds the fields still while a call is out, so it sends no edit then.
      (): Msg => { if (!inFlight(state)) draft++; return { kind: "edit", key: `k${keys++}` } },
      (): Msg => ({ kind: "submit", input: `{"verdict":"v${draft}"}` }),
      (): Msg => ({ kind: "back" }),
      (): Msg => ({ kind: "answered", result: result() }),
      (): Msg => ({ kind: "answered", result: result() }),
      (): Msg => ({ kind: "threw", message: r() < 0.5 ? null : "offline" }),
      (): Msg => ({ kind: "retry" }),
      (): Msg => ({ kind: "again", key: `k${keys++}` }),
      (): Msg => ({ kind: "undo", key: `k${keys++}` }),
    ])()
    const [after, cmds] = update(state, msg)
    steps.push({ before: state, msg, after, cmds })
    state = after
  }
  return { risky, steps }
}

const SEEDS = Array.from({ length: 400 }, (_, i) => i + 1)
const calls = (cmds: readonly Cmd[]) => cmds.filter((c): c is Extract<Cmd, { kind: "call" }> => c.kind === "call")

test("update is pure: the same state and message give the same answer", () => {
  for (const seed of SEEDS) for (const { before, msg, after, cmds } of session(seed).steps) expect(update(before, msg)).toEqual([after, cmds])
})

test("one call at a time: no call starts while another is on its way", () => {
  for (const seed of SEEDS) for (const { before, cmds } of session(seed).steps) {
    expect(calls(cmds).length).toBeLessThanOrEqual(1)
    if (calls(cmds).length > 0) expect(inFlight(before)).toBe(false)
  }
})

test("a risky action never calls straight from editing: a confirm comes first", () => {
  for (const seed of SEEDS) {
    const { risky, steps } = session(seed)
    for (const { before, msg, cmds } of steps) if (risky && msg.kind === "submit" && calls(cmds).length > 0) expect(before.phase.kind).toBe("confirm")
  }
})

test("keys: one key never carries two bodies, a retry re-sends exactly the last call, and an edit always moves the key", () => {
  for (const seed of SEEDS) {
    const bodies = new Map<string, string>()
    let last: Extract<Cmd, { kind: "call" }> | null = null
    for (const { before, msg, after, cmds } of session(seed).steps) {
      if (msg.kind === "edit" && !inFlight(before) && before.phase.kind !== "done" && before.phase.kind !== "pending" && before.phase.kind !== "undo-failed" && before.phase.kind !== "undone") expect(after.key).toBe(msg.key)
      if (msg.kind === "edit" && inFlight(before)) expect(after).toBe(before)
      for (const c of calls(cmds)) {
        if (c.actionId !== "learning.triage_episode") continue
        if (bodies.has(c.key)) expect(c.input).toBe(bodies.get(c.key)!)
        bodies.set(c.key, c.input)
        if (msg.kind === "retry") expect(c).toEqual(last!)
        last = c
      }
    }
  }
})

test("undo calls only the compensation the receipt named, with its input, and nothing else", () => {
  for (const seed of SEEDS) for (const { before, cmds } of session(seed).steps) {
    for (const c of calls(cmds)) {
      if (c.actionId === "learning.triage_episode") continue
      const offered = before.phase.kind === "done" ? before.phase.undo : before.phase.kind === "undo-failed" ? before.phase.undo : null
      expect(offered).not.toBeNull()
      expect({ actionId: c.actionId, input: c.input }).toEqual(offered!)
    }
  }
})

test("the documents are read again exactly when the cell commits something, and a late answer changes nothing", () => {
  for (const seed of SEEDS) for (const { before, msg, after, cmds } of session(seed).steps) {
    const refetches = cmds.filter((c) => c.kind === "refetch").length
    const commits = msg.kind === "answered" && msg.result.kind === "receipt" && inFlight(before) ? 1 : 0
    expect(refetches).toBe(commits)
    if ((msg.kind === "answered" || msg.kind === "threw") && !inFlight(before)) { expect(after).toBe(before); expect(cmds).toEqual([]) }
  }
})

test("an undo that did not come back says whether it is known not to have happened: only the cell's own answer makes it known", () => {
  for (const seed of SEEDS) for (const { before, msg, after } of session(seed).steps) {
    if (before.phase.kind !== "undoing" || after.phase.kind !== "undo-failed") continue
    expect(after.phase.known).toBe(msg.kind === "answered")
  }
})

test("undo offers are decoded fail-closed: a missing, malformed or inexact offer is no offer", () => {
  expect(undoOf(receipt(1))).toBeNull()
  expect(undoOf(receipt(1, { actionId: "", input: {} }))).toBeNull()
  expect(undoOf(receipt(1, { actionId: "a.undo", input: "x" }))).toBeNull()
  expect(undoOf(receipt(1, { actionId: "a.undo", input: { qty: 0.1 } }))).toBeNull()
  expect(undoOf(receipt(1, { actionId: "a.undo", input: { n: [2 ** 60] } }))).toBeNull()
  expect(undoOf(receipt(1, { actionId: "a.undo", input: { id: "ep_1", n: 3 } }))).toEqual({ actionId: "a.undo", input: '{"id":"ep_1","n":3}' })
})

test("per-record offers only ever narrow the served catalog, in its order; without them the catalog stands", () => {
  const catalog = ["a", "b", "c", "d"].map((actionId) => ({ actionId }) as ActionSummary)
  for (const seed of SEEDS) {
    const r = rng(seed)
    const offers = Array.from({ length: Math.floor(r() * 6) }, () => pick(r, ["a", "b", "c", "d", "x", 7]))
    const shown = offeredFor(catalog, { id: "r", offers }).map((a) => a.actionId)
    expect(shown).toEqual(catalog.map((a) => a.actionId).filter((id) => offers.includes(id)))
  }
  expect(offeredFor(catalog, { id: "r" })).toBe(catalog)
  expect(offeredFor(catalog, { id: "r", offers: "a" })).toBe(catalog)
})

test("an approval-gated call commits nothing: it waits for a decider, reads nothing again, and a new draft may follow", () => {
  const sending = update(init({ actionId: "ops.refund", risky: false }, "k0"), { kind: "submit", input: "{}" })[0]
  expect(sending.phase.kind).toBe("sending")
  const [pending, cmds] = update(sending, { kind: "answered", result: { kind: "pending", decision: { id: "d1", expiresAtMs: 5 } } })
  expect(pending.phase).toEqual({ kind: "pending", decision: { id: "d1", expiresAtMs: 5 } })
  expect(cmds).toEqual([])
  const [again] = update(pending, { kind: "again", key: "k1" })
  expect(again.phase.kind).toBe("editing")
  expect(again.key).toBe("k1")
})
