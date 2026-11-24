import { expect, test } from "bun:test"
import { QueryClient, QueryObserver } from "@tanstack/react-query"
import type { ActionReceipt, ActionSummary, CallResult, CatalogRead } from "../src/lib/actions.ts"
import { hasRecordOffers, inFlight, init, offeredFor, parkedOf, recordCatalog, resume, undoOf, update, type Cmd, type Msg, type State } from "../src/lib/screen/call.ts"

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
      // The cell serves the parked call back exactly as it was sent (APPROVAL-REQUESTED-1).
      (): Msg => ({ kind: "run-approved", decisionId: state.phase.kind === "pending" ? state.phase.decision.id : "elsewhere", actionId: "learning.triage_episode", input: state.sent?.input ?? "{}", key: state.sent?.key ?? "kx", next: `k${keys++}` }),
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

test("per-record offers only ever narrow the served catalog, in its order; missing or malformed offers fail closed", () => {
  const catalog = ["a", "b", "c", "d"].map((actionId) => ({ actionId }) as ActionSummary)
  for (const seed of SEEDS) {
    const r = rng(seed)
    const offers = Array.from({ length: Math.floor(r() * 6) }, () => pick(r, ["a", "b", "c", "d", "x", 7]))
    const shown = offeredFor(catalog, { id: "r", offers }).map((a) => a.actionId)
    expect(shown).toEqual(catalog.map((a) => a.actionId).filter((id) => offers.includes(id)))
  }
  expect(hasRecordOffers({ id: "r", offers: ["a"] })).toBe(true)
  expect(hasRecordOffers({ id: "r", offers: [] })).toBe(true)
  expect(hasRecordOffers({ id: "r" })).toBe(false)
  expect(hasRecordOffers({ id: "r", offers: "a" })).toBe(false)
  expect(offeredFor(catalog, { id: "r" })).toEqual([])
  expect(offeredFor(catalog, { id: "r", offers: undefined })).toEqual([])
  expect(offeredFor(catalog, { id: "r", offers: null })).toEqual([])
  expect(offeredFor(catalog, { id: "r", offers: "a" })).toEqual([])
  expect(offeredFor(catalog, { id: "r", offers: [] })).toEqual([])
})

test("an approval-gated call commits nothing: it waits for a decider, reads nothing again, and a new draft may follow", () => {
  const sending = update(init({ actionId: "ops.refund", risky: false }, "k0"), { kind: "submit", input: "{}" })[0]
  expect(sending.phase.kind).toBe("sending")
  const [pending, cmds] = update(sending, { kind: "answered", result: { kind: "pending", decision: { id: "d1", expiresAtMs: 5 } } })
  expect(pending.phase).toEqual({ kind: "pending", decision: { id: "d1", expiresAtMs: 5 } })
  expect(cmds).toEqual([])
  // The sent call stays the only one under its key: nothing is sent again from here but a new draft.
  expect(update(pending, { kind: "submit", input: '{"other":1}' })).toEqual([pending, []])
  const [again] = update(pending, { kind: "again", key: "k1" })
  expect(again.phase.kind).toBe("editing")
  expect(again.key).toBe("k1")
})

// ACTION-CATALOG-STATE-1: a live record offers only what the cell narrowed to its state; until
// that read succeeds it offers nothing (not the type's list, and not "no actions").
test("a live record waits for its narrowed catalog; a recorded one keeps the type's", () => {
  const typeLevel = [{ actionId: "submit" }, { actionId: "approve" }] as unknown as ActionSummary[]
  const narrowed = [{ actionId: "approve" }] as unknown as ActionSummary[]
  const read = (data: CatalogRead | undefined, isFetching = false, isError = false) => ({ data, isFetching, isError })
  expect(recordCatalog(true, read(undefined, true), typeLevel)).toEqual({ kind: "reading" })
  expect(recordCatalog(true, read(undefined), typeLevel)).toEqual({ kind: "reading" })
  expect(recordCatalog(true, read({ kind: "unavailable" }), typeLevel)).toEqual({ kind: "unreadable" })
  expect(recordCatalog(true, read({ kind: "catalog", items: narrowed }), typeLevel)).toEqual({ kind: "catalog", items: narrowed })
  expect(recordCatalog(false, read(undefined), typeLevel)).toEqual({ kind: "catalog", items: typeLevel })
})

// #1469: a cached narrowed catalog answers for the record's state when it was read, which a commit
// may have moved since. While it is read again the drawer offers nothing (a skeleton), and when the
// read again throws it offers nothing (an error), never the cached buttons. Driven by the query
// states TanStack Query itself reports for a refetch over cached data.
test("a live record's cached catalog offers nothing while it is read again, or after that read fails", async () => {
  const typeLevel = [{ actionId: "submit" }, { actionId: "approve" }] as unknown as ActionSummary[]
  const cached = [{ actionId: "submit" }] as unknown as ActionSummary[]
  const client = new QueryClient()
  const key = ["actions", "purchasing.PurchaseRequest", "PR-1"]
  const seeded: CatalogRead = { kind: "catalog", items: cached }
  client.setQueryData(key, seeded)
  const pending: { fail?: (error: Error) => void } = {}
  const observer = new QueryObserver<CatalogRead>(client, { queryKey: key, queryFn: () => new Promise<CatalogRead>((_, reject) => { pending.fail = reject }), retry: false, staleTime: 0, refetchOnMount: false })
  const offer = () => recordCatalog(true, observer.getCurrentResult(), typeLevel)
  expect(offer()).toEqual({ kind: "catalog", items: cached })
  const unsubscribe = observer.subscribe(() => {})
  const refetch = observer.refetch()
  expect(observer.getCurrentResult().data).toEqual({ kind: "catalog", items: cached })
  expect(offer()).toEqual({ kind: "reading" })
  while (pending.fail === undefined) await new Promise((resolve) => setTimeout(resolve, 0))
  pending.fail(new Error("offline"))
  await refetch
  expect(observer.getCurrentResult().data).toEqual({ kind: "catalog", items: cached })
  expect(offer()).toEqual({ kind: "unreadable" })
  unsubscribe()
})

// APPROVAL-REQUESTED-1: a parked call keeps its exact encoded input and key, and "Approved, run it"
// sends exactly the call the cell serves back, only from that decision's pending phase.
const TARGET = { actionId: "ops.refund", risky: false } as const
const PARKED = { decision: { id: "dec_1", expiresAtMs: 9 }, input: '{"amount":"1240.00","id":"r-1"}', key: "k-parked" } as const

test("a parked call keeps its exact input and key: the sheet reopens on it as it was sent", () => {
  const sending = update(init(TARGET, "k-parked"), { kind: "submit", input: PARKED.input })[0]
  const [pending] = update(sending, { kind: "answered", result: { kind: "pending", decision: PARKED.decision } })
  expect(parkedOf(pending)).toEqual(PARKED)
  const resumed = resume(TARGET, PARKED)
  expect(resumed.phase).toEqual({ kind: "pending", decision: PARKED.decision })
  expect(parkedOf(resumed)).toEqual(PARKED)
  // Nothing but a parked phase is a parked call.
  expect(parkedOf(init(TARGET, "k0"))).toBeNull()
  expect(parkedOf(sending)).toBeNull()
})

test("run-approved sends exactly the served call, under its key, once; a retry after an outage re-sends it", () => {
  const served = { kind: "run-approved", decisionId: "dec_1", actionId: "ops.refund", input: '{"amount":"1240.00","id":"r-1"}', key: "k-parked", next: "k-draft" } as const
  const [running, cmds] = update(resume(TARGET, PARKED), served)
  expect(running.phase.kind).toBe("sending")
  expect(cmds).toEqual([{ kind: "call", actionId: "ops.refund", input: served.input, key: served.key }])
  // A second click while it is out starts nothing.
  expect(update(running, served)).toEqual([running, []])
  const [failed] = update(running, { kind: "threw", message: "offline" })
  expect(update(failed, { kind: "retry" })[1]).toEqual(cmds)
  const [done, after] = update(running, { kind: "answered", result: { kind: "receipt", receipt: receipt(4) } })
  expect(done.phase.kind).toBe("done")
  expect(after).toEqual([{ kind: "refetch" }])
})

test("run-approved is only for the decision this form waits on and its own action", () => {
  const pending = resume(TARGET, PARKED)
  const call = { kind: "run-approved", decisionId: "dec_1", actionId: "ops.refund", input: PARKED.input, key: PARKED.key, next: "k-draft" } as const
  expect(update(pending, { ...call, decisionId: "dec_other" })).toEqual([pending, []])
  expect(update(pending, { ...call, actionId: "ops.other" })).toEqual([pending, []])
  const editing = init(TARGET, "k0")
  expect(update(editing, call)).toEqual([editing, []])
  // The served call wins over what this tab kept: the cell holds the call.
  const [, cmds] = update(pending, { ...call, input: '{"amount":"1240.000","id":"r-1"}' })
  expect(cmds).toEqual([{ kind: "call", actionId: "ops.refund", input: '{"amount":"1240.000","id":"r-1"}', key: PARKED.key }])
})

test("a refused approved run is final for that call: a draft sent after it is a new call under a new key", () => {
  const [running] = update(resume(TARGET, PARKED), { kind: "run-approved", decisionId: "dec_1", actionId: "ops.refund", input: PARKED.input, key: PARKED.key, next: "k-draft" })
  const [refused] = update(running, { kind: "answered", result: { kind: "refused", status: 403, code: "action.denied", message: "denied", diagnostics: [] } })
  expect(refused.phase.kind).toBe("refused")
  // Even without an edit, the parked key never carries another body.
  expect(update(refused, { kind: "submit", input: "{}" })[1]).toEqual([{ kind: "call", actionId: "ops.refund", input: "{}", key: "k-draft" }])
  const [edited] = update(refused, { kind: "edit", key: "k-new" })
  expect(edited.key).toBe("k-new")
  const [, cmds] = update(edited, { kind: "submit", input: "{}" })
  expect(cmds).toEqual([{ kind: "call", actionId: "ops.refund", input: "{}", key: "k-new" }])
})
