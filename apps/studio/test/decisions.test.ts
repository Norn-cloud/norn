import { expect, test } from "bun:test"
import { badgeText, cellNow, countDecisions, decide, flowTitle, listDecisions, needsConfirm, remaining, stepTitle, urgent } from "../src/lib/decisions.ts"
import { getRun, newRunId, phaseOf, refreshEvery, runTitle, startRun, type RunStep } from "../src/lib/runs.ts"

const ME = { user: { id: "u" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" }
function cell(host: (url: URL, init?: RequestInit) => Response, me: unknown = ME) {
  const calls: { url: URL; init?: RequestInit }[] = []
  const fetcher = async (input: string, init?: RequestInit) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json(me)
    calls.push({ url, init })
    return host(url, init)
  }
  return { calls, fetcher }
}
const refusal = (status: number, code: string, extra: Record<string, unknown> = {}) => () => Response.json({ code, message: `m:${code}`, diagnostics: [], ...extra }, { status })

test("the inbox reads pending decisions scoped; limit=0 asks only for the count; no cell, no call", async () => {
  const live = cell(() => Response.json({ items: [], total: 3, nextCursor: null, serverNowMs: 1 }))
  expect(await listDecisions({}, live.fetcher)).toMatchObject({ kind: "ok", value: { total: 3 } })
  expect(Object.fromEntries(live.calls[0]!.url.searchParams)).toEqual({ state: "pending", orgId: "org_demo", environmentId: "analytics" })
  await countDecisions(live.fetcher)
  expect(live.calls[1]?.url.searchParams.get("limit")).toBe("0")
  const none = cell(() => { throw new Error("no call") }, { ...ME, environmentId: null })
  expect(await listDecisions({}, none.fetcher)).toEqual({ kind: "no-cell" })
  expect(none.calls).toHaveLength(0)
})

test("a 404 with a Norn code is the cell's answer; a bare 404 or 503 means the route is not served", async () => {
  expect(await decide("dec_x", { choice: "a" }, "k", cell(refusal(404, "decision.not_found")).fetcher)).toEqual({ kind: "not-found", message: "m:decision.not_found" })
  expect(await decide("dec_x", { choice: "a" }, "k", cell(() => new Response("", { status: 404 })).fetcher)).toEqual({ kind: "unavailable" })
  expect(await listDecisions({}, cell(() => new Response(null, { status: 503 })).fetcher)).toEqual({ kind: "unavailable" })
})

test("the race: someone else resolved it first, by name, never id", async () => {
  const result = await decide("dec_x", { choice: "a" }, "k", cell(refusal(409, "decision.resolved", { resolution: { resolution: "decided", by: "Mona Ali", atMs: 7 } })).fetcher)
  expect(result).toEqual({ kind: "resolved", message: "m:decision.resolved", resolution: { resolution: "decided", by: "Mona Ali", atMs: 7 } })
  const withdrawn = await decide("dec_x", { choice: "a" }, "k", cell(refusal(409, "decision.resolved", { resolution: { resolution: "withdrawn", atMs: 9 } })).fetcher)
  expect(withdrawn).toMatchObject({ kind: "resolved", resolution: { resolution: "withdrawn", by: null } })
  expect(await decide("dec_x", { choice: "a" }, "k", cell(refusal(410, "decision.expired")).fetcher)).toMatchObject({ kind: "expired" })
})

test("field refusals keep their pointers from diagnostics[].span.pointer; rule refusals have none", async () => {
  const body = { code: "decision.unknown_choice", message: "Refused.", diagnostics: [
    { code: "decision.unknown_choice", message: "Choose one of the options.", span: { pointer: "/choice" } },
    { code: "decision.note_too_long", message: "At most 500.", span: { pointer: "/note" } },
  ] }
  const result = await decide("dec_x", { choice: "z", note: "n" }, "k", cell(() => Response.json(body, { status: 422 })).fetcher)
  expect(result.kind === "refused" && result.diagnostics.map((d) => d.pointer)).toEqual(["/choice", "/note"])
  const law = await decide("dec_x", { choice: "a" }, "k", cell(() => Response.json({ code: "decision.law_not_satisfied", message: "Needs finance.", diagnostics: [{ code: "decision.law_not_satisfied", message: "Needs finance.", span: null }] }, { status: 403 })).fetcher)
  expect(law).toMatchObject({ kind: "refused", status: 403, diagnostics: [{ pointer: null }] })
})

test("a decide call sends the choice, the note only when given, and its key; the opaque id is encoded, not read", async () => {
  const ok = { outcome: "replayed", decision: { id: "dec_A_b-c", choice: "a", decidedBy: "you", decidedAtMs: 1 }, receipt: { receiptHash: "h", commitSeq: 2, recordedAtMs: 1 }, run: { id: "r", state: "running" } }
  const live = cell(() => Response.json(ok))
  expect(await decide("dec_A_b-c", { choice: "a" }, "key-1", live.fetcher)).toEqual({ kind: "decided", decided: ok as never })
  expect(live.calls[0]?.url.pathname).toBe("/api/host/v1/decisions/dec_A_b-c:decide")
  expect(live.calls[0]?.init?.body).toBe('{"choice":"a","idempotencyKey":"key-1"}')
})

test("titles fall back to the ids in words; the badge says when the count is a lower bound", () => {
  expect(flowTitle({ flow: { ruleId: "canon.refund_review.flow.rule", version: 1, title: null } })).toBe("Refund review")
  expect(flowTitle({ flow: { ruleId: "x", version: 1, title: "Restock gold" } })).toBe("Restock gold")
  expect(stepTitle({ step: { id: "ask_manager", label: null } })).toBe("Ask manager")
  expect([badgeText({ total: 3 }), badgeText({ total: 500, totalCapped: true })]).toEqual(["3", "500+"])
})

test("high risk or an option that starts an action asks before deciding", () => {
  const binding = { id: "hold", label: "Hold", scope: "s", effect: { binding: "b" } }
  const action = { id: "go", label: "Go", scope: "s", effect: { action: "a" } }
  expect([needsConfirm({ risk: "low" }, binding), needsConfirm({ risk: "low" }, action), needsConfirm({ risk: "high" }, binding)]).toEqual([false, true, true])
})

test("countdowns run on the cell's clock, not the browser's", () => {
  const now = cellNow(1_000_000, 50, 1_050)
  expect(now).toBe(1_001_000)
  expect(remaining(now + 2 * 86_400_000 + 4 * 3_600_000, now)).toBe("2 d 4 h")
  expect(remaining(now + 3 * 3_600_000 + 5 * 60_000, now)).toBe("3 h 05 min")
  expect(remaining(now + 12 * 60_000, now)).toBe("12 min")
  expect(remaining(now + 45_000, now)).toBe("45 s")
  expect(remaining(now, now)).toBeNull()
  expect([urgent(now + 59 * 60_000, now), urgent(now + 2 * 3_600_000, now)]).toEqual([true, false])
})

const step = (status: RunStep["status"], kind: RunStep["kind"] = "perform"): RunStep => ({ id: "s", label: null, kind, status, startedAtMs: null, endedAtMs: null, receipts: [], error: null })

test("a run's phase comes from its steps, and sets how often it is read again", () => {
  expect(phaseOf({ steps: [step("done"), step("running")], decision: null, state: "x" })).toBe("running")
  expect(phaseOf({ steps: [step("done"), step("waiting", "decide")], decision: { id: "d", expiresAtMs: 1 }, state: "x" })).toBe("waiting")
  expect(phaseOf({ steps: [step("done"), step("done", "succeed")], decision: null, state: "x" })).toBe("succeeded")
  expect(phaseOf({ steps: [step("failed"), step("skipped", "succeed")], decision: null, state: "x" })).toBe("failed")
  expect([refreshEvery("running"), refreshEvery("waiting"), refreshEvery("succeeded")]).toEqual([2_000, 15_000, false])
  // RUNS's first cut: no steps, no decision. The state alone says whether it may still move.
  expect(phaseOf({ state: "awaiting_perform_ask", decision: undefined })).toBe("active")
  expect(phaseOf({ state: null })).toBe("ended")
  expect(refreshEvery("active")).toBe(5_000)
  expect(runTitle({ flow: { ruleId: "canon.cycle_count.flow.rule", version: 1, title: null } })).toBe("Cycle count")
  expect(newRunId()).toMatch(/^run_[0-9a-f-]{36}$/)
})

test("a run is read with its ETag; an unchanged run answers 304; starting one posts the flow and id", async () => {
  const live = cell((_, init) => (init?.headers as Record<string, string> | undefined)?.["if-none-match"] === '"1:3"' ? new Response(null, { status: 304 }) : Response.json({ id: "r" }, { headers: { etag: '"1:3"' } }))
  expect(await getRun("r", null, live.fetcher)).toMatchObject({ kind: "ok", etag: '"1:3"' })
  expect(await getRun("r", '"1:3"', live.fetcher)).toEqual({ kind: "not-modified" })
  const post = cell(() => Response.json({ id: "run_1" }, { status: 201 }))
  expect(await startRun("canon.daily_totals", "run_1", post.fetcher)).toMatchObject({ kind: "ok", status: 201 })
  expect(post.calls[0]?.init?.body).toBe('{"flow":"canon.daily_totals","runId":"run_1"}')
  expect(await getRun("nope", null, cell(refusal(404, "run.not_found")).fetcher)).toMatchObject({ kind: "refused", refusal: { code: "run.not_found" } })
})
