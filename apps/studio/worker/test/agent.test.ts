import type { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compile, editableDashboard, ISO_DATE, proposalSchema, propose, systemPrompt, unchangedByGuard, type Proposed } from "../agent.ts"
import { assistant, CALLS_PER_REQUEST, HOURLY_PROPOSALS, type AssistantDeps } from "../assistant.ts"
import type { Cell, CellAnswer } from "../cell.ts"
import { claudeCost, configuredModel, neuronCost, withFallback, workersAi, worstCallMicroUsd, type Answer, type Ask, type Model } from "../model.ts"
import { reserve, settle } from "../proposals.ts"
import type { CellCatalog } from "../questions.ts"
import { createMigratedDb } from "./harness.ts"

const fixture = (name: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../../fixtures/demo/answers", name), "utf8"))
const cellCatalog: CellCatalog = { objectTypes: [fixture("scrap-yield.object-type.json")], metrics: fixture("scrap-yield.metrics.json"), grains: ["week"], operators: ["eq", "ne"], maxGroupBy: 1, maxBuckets: 104 }
const catalog = cellCatalog
const NOW = Date.UTC(2026, 10, 12, 15)

const tile = (over: Partial<Proposed["tiles"][number]> = {}): Proposed["tiles"][number] =>
  ({ title: "Gold yield", metric: "demo.yield", groupBy: "occurred_at", grain: "week", filters: [{ property: "metal", operator: "eq", value: "gold" }], start: "2026-05-18", end: "2026-11-16", view: "line", width: "full", ...over })
const proposed = (tiles: Proposed["tiles"]): Proposed => ({ title: "Refinery weekly", note: "Yield and inputs, weekly.", tiles })

test("the proposal schema draws every name from the catalog, so a metric or property the cell does not declare cannot be proposed", () => {
  const schema = JSON.stringify(proposalSchema(catalog))
  for (const name of ["demo.input", "demo.recovered", "demo.yield", "occurred_at", "metal", "\"week\"", "\"pie\"", "\"number\""]) expect(schema).toContain(name)
  expect(schema).not.toContain("demo.purity")
  const prompt = systemPrompt(catalog, NOW)
  expect(prompt).toContain("Today is 2026-11-12")
  expect(prompt).toContain("demo.yield (\"Yield\"): ratio of Recovered to Input (not summable)")
})

test("the prompt names each readable link and how to say that grouping across it is not supported yet, so the note never denies a property a link reaches", () => {
  const [conversion] = cellCatalog.objectTypes
  if (conversion === undefined) throw new Error("fixture")
  const linked: CellCatalog = { ...cellCatalog, objectTypes: [{ ...conversion, links: [{ id: "customer", target: "canon.Party" }] }] }
  const prompt = systemPrompt(linked, NOW)
  expect(prompt).toContain("Links (not groupable or filterable yet):\n- customer → canon.Party")
  expect(prompt).toContain("needs grouping across the <type> → <target> link, which this cell's questions don't support yet")
  expect(prompt).toContain("Never say the catalog has no such property when a link reaches it.")
  expect(systemPrompt(cellCatalog, NOW)).not.toContain("Links (not groupable")
})

test("a proposal compiles to the cell's dashboard payload: the builder's question members, numeric filters as numbers, tiles in reading order, and its origin", () => {
  let n = 0
  const definition = compile(proposed([tile({ view: "number", groupBy: "", grain: "", width: "half" }), tile({ filters: [{ property: "input", operator: "eq", value: "50.5" }] }), tile({ metric: "demo.recovered", groupBy: "metal", grain: "", view: "pie", width: "half" })]), catalog, { proposal: "prop_1", model: "claude-sonnet-5-5", request: "Weekly gold yield" }, () => `t${++n}`)
  expect(definition.origin).toEqual({ by: "assistant", proposal: "prop_1", model: "claude-sonnet-5-5", request: "Weekly gold yield" })
  expect(definition.tiles.map(({ id, layout, view }) => [id, `${layout.x},${layout.y},${layout.w}`, view])).toEqual([
    ["t1", "0,0,1", { kind: "number" }], ["t2", "0,1,2", { kind: "chart", chart: "line" }], ["t3", "0,2,1", { kind: "chart", chart: "pie" }],
  ])
  expect(definition.tiles[0]?.question).toEqual({ irVersion: "0.1", name: "studio.gold_yield", objectSet: "demo.Conversion", aggregate: [{ metric: "demo.yield" }], groupBy: [], filter: [{ path: { property: "metal" }, operator: "eq", value: "gold" }], window: { start: "2026-05-18T00:00:00Z", end: "2026-11-16T00:00:00Z" }, calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 5000 } })
  expect(definition.tiles[1]?.question.filter).toEqual([{ path: { property: "input" }, operator: "eq", value: 50.5 }])
  expect(definition.tiles[2]?.question.groupBy).toEqual([{ path: { property: "metal" } }])
})

/** A model that answers from a script, recording each ask. */
function scripted(...documents: unknown[]): Model & { readonly asks: Ask[] } {
  const asks: Ask[] = []
  return { name: "scripted", asks, answer: async (ask) => { asks.push(ask); const json = documents.shift() ?? null; return { json, model: "scripted", inputTokens: 1000, outputTokens: 200, costMicroUsd: 4000, ...json === null ? { failure: "no answer" } : {} } } }
}
/** The cell as the assistant reaches it: its catalog, and a checker that refuses a pie over a ratio. */
function cellDouble() {
  const calls: { method: string; path: string; body?: unknown }[] = []
  const ok = (body: unknown): CellAnswer => ({ kind: "answer", status: 200, body })
  const cell: Cell = {
    get: async (path) => { calls.push({ method: "GET", path }); return path === "/v1/questions:catalog" ? ok(cellCatalog) : { kind: "answer", status: 404, body: null } },
    post: async (path, body) => {
      calls.push({ method: "POST", path, body })
      // The cell's guard, as a stand-in: one email address becomes [EMAIL_1]; a restore gives it back.
      if (path === "/v1/guard:prompt") {
        const text = (body as { text: string }).text
        const email = /[\w.+-]+@[\w-]+\.[\w.]+/.exec(text)?.[0]
        return ok({ guardId: 7, receiptHash: "g", safePrompt: email ? text.replace(/[\w.+-]+@[\w-]+\.[\w.]+/g, "[EMAIL_1]") : text, tokens: email ? [{ token: "[EMAIL_1]", kind: "EMAIL", label: "Email", source: "detected", revealable: true }] : [] })
      }
      if (path === "/v1/guard/7:restore") return ok({ guardId: 7, segments: [{ text: (body as { text: string }).text }], reveals: [], complete: true })
      const payload = (body as { payload: ReturnType<typeof compile> }).payload
      const bad = payload.tiles.findIndex((t) => t.view.kind === "chart" && "chart" in t.view && t.view.chart === "pie" && t.question.aggregate[0]?.metric === "demo.yield")
      if (path === "/v1/definitions:admit") return ok(bad < 0 ? { admitted: true, diagnostics: [], restatement: `${payload.title}\nwords`, restatementSha256: "c".repeat(64) } : { admitted: false, diagnostics: [{ code: "definition.view_invalid", message: "a pie shows the parts of one whole", span: { pointer: `/tiles/${bad}/view` } }], restatement: null, restatementSha256: null })
      if (path === "/v1/definitions:install") return ok({ installed: true, id: (body as { id: string }).id, version: 1, receipt: {} })
      return { kind: "answer", status: 404, body: null }
    },
  }
  return { cell, calls }
}

test("a refused proposal goes back to the model once with the checker's own findings; the corrected one is admitted", async () => {
  const { cell } = cellDouble()
  const model = scripted(proposed([tile({ view: "pie", groupBy: "metal", grain: "" })]), proposed([tile({ view: "bar", groupBy: "metal", grain: "" })]))
  const outcome = await propose(cell, model, "Yield by metal", { proposalId: "prop_1", targetId: "studio.dashboard.x", now: NOW, guard: async (text) => text, maxCalls: 4 })
  expect(outcome.verdict?.admitted).toBe(true)
  expect(outcome.answers).toHaveLength(2)
  expect(model.asks[1]?.turns.at(-1)?.content).toContain("tile 1: a pie shows the parts of one whole (definition.view_invalid at /tiles/0/view)")
  expect(outcome.definition?.tiles[0]?.view).toEqual({ kind: "chart", chart: "bar" })
  // Still refused after the retry: the last proposal and its refusal, never a guess.
  const stubborn = await propose(cell, scripted(proposed([tile({ view: "pie", groupBy: "metal", grain: "" })]), proposed([tile({ view: "pie", groupBy: "metal", grain: "" })])), "Yield by metal", { proposalId: "prop_2", targetId: "studio.dashboard.x", now: NOW, guard: async (text) => text, maxCalls: 4 })
  expect(stubborn.verdict?.admitted).toBe(false)
  expect(stubborn.verdict?.findings[0]?.code).toBe("definition.view_invalid")
})

test("the fallback answers only when the primary gives no document, and both calls are reported", async () => {
  const failing: Model = { name: "primary", answer: async () => { throw new Error("gateway down") } }
  const both = await withFallback(failing, scripted({ any: 1 })).answerAll({ system: "", turns: [], schema: {}, maxTokens: 1 })
  expect(both.map(({ model, failure }) => [model, failure ?? null])).toEqual([["primary", "gateway down"], ["scripted", null]])
  expect(claudeCost("claude-sonnet-5-5", 1_000_000, 100_000)).toBe(3_000_000)
  expect(neuronCost(1000)).toBe(11_000)
})

function d1(db: Database): D1Database {
  const statement = (sql: string, args: unknown[] = []) => ({
    bind: (...next: unknown[]) => statement(sql, next),
    all: async () => ({ results: db.query(sql).all(...(args as never[])) }),
    first: async () => db.query(sql).get(...(args as never[])) ?? null,
    run: async () => ({ success: true, meta: { changes: db.query(sql).run(...(args as never[])).changes } }),
  })
  return { prepare: (sql: string) => statement(sql), batch: async (list: { run(): Promise<unknown> }[]) => { for (const s of list) await s.run(); return [] } } as unknown as D1Database
}
const stage = () => d1(createMigratedDb(["0008_dashboard_proposal.sql", "0016_agent_reservation.sql"]))
const post = (route: string, body: unknown = {}) => new Request(`https://studio.test/api/agent/dashboards${route}`, { method: "POST", body: JSON.stringify(body) })
const get = (route = "") => new Request(`https://studio.test/api/agent/dashboards${route}`)

test("a proposal is a commitment: pending once admitted, accepted once (installed as the person, signing the checker's words), and only by its author", async () => {
  const db = stage()
  const { cell, calls } = cellDouble()
  let n = 0
  const deps = (over: Partial<AssistantDeps> = {}): AssistantDeps => ({ db, cell, scope: { orgId: "org_demo", environmentId: "analytics", userId: "u1" }, model: scripted(proposed([tile()])), capMicroUsd: 20_000_000, now: NOW, newId: () => `id${++n}`, ...over })
  const made = await (await assistant(post("", { request: "Weekly gold yield" }), "", deps())).json() as { id: string; state: string; targetId: string; definition: { origin: { request: string } }; costMicroUsd: number }
  expect(made).toMatchObject({ id: "prop_id1", state: "pending", targetId: "studio.dashboard.refinery_weekly_id2", costMicroUsd: 4000 })
  expect(made.definition.origin.request).toBe("Weekly gold yield")
  // Another person cannot see, accept or decline it.
  const stranger = deps({ scope: { orgId: "org_demo", environmentId: "analytics", userId: "u2" } })
  expect((await assistant(post(`/${made.id}/accept`), `/${made.id}/accept`, stranger)).status).toBe(404)
  expect((await (await assistant(get(), "", stranger)).json() as { proposals: unknown[] }).proposals).toEqual([])
  const accepted = await (await assistant(post(`/${made.id}/accept`), `/${made.id}/accept`, deps())).json() as { state: string; installed: unknown }
  expect(accepted).toMatchObject({ state: "accepted", installed: { id: "studio.dashboard.refinery_weekly_id2", version: 1 } })
  expect(calls.at(-1)).toMatchObject({ path: "/v1/definitions:install", body: { kind: "dashboard", id: "studio.dashboard.refinery_weekly_id2", signoff: { restatementSha256: "c".repeat(64) } } })
  expect((await assistant(post(`/${made.id}/accept`), `/${made.id}/accept`, deps())).status).toBe(409)
  expect((await assistant(post(`/${made.id}/decline`), `/${made.id}/decline`, deps())).status).toBe(409)
})

test("a refused proposal is kept with its diagnostics and cannot be accepted; the spend cap and an empty request refuse before any call", async () => {
  const db = stage()
  const { cell } = cellDouble()
  const pie = proposed([tile({ view: "pie", groupBy: "metal", grain: "" })])
  const deps: AssistantDeps = { db, cell, scope: { orgId: "org_demo", environmentId: "analytics", userId: "u1" }, model: scripted(pie, pie), capMicroUsd: 16_000, perCallMicroUsd: 4000, now: NOW }
  const refusedProposal = await (await assistant(post("", { request: "Yield by metal as a pie" }), "", deps)).json() as { id: string; state: string; verdict: { findings: { code: string }[] } }
  expect(refusedProposal.state).toBe("refused")
  expect(refusedProposal.verdict.findings[0]?.code).toBe("definition.view_invalid")
  expect((await assistant(post(`/${refusedProposal.id}/accept`), `/${refusedProposal.id}/accept`, deps)).status).toBe(409)
  // Two calls at 4000 each were spent; the next request's reservation (four calls at 4000) no longer fits the 16000 cap.
  const capped = await assistant(post("", { request: "Anything" }), "", { ...deps, model: scripted(proposed([tile()])) })
  expect(capped.status).toBe(429)
  expect((await assistant(post("", { request: "  " }), "", deps)).status).toBe(400)
  expect((await assistant(post("", { request: "x" }), "", { ...deps, model: undefined })).status).toBe(503)
})

test("every model call goes through the cell's guard: the model reads tokens, never the value; no guard, no call", async () => {
  const db = stage()
  const { cell, calls } = cellDouble()
  const model = scripted(proposed([tile()]))
  const deps: AssistantDeps = { db, cell, scope: { orgId: "org_demo", environmentId: "analytics", userId: "u1" }, model, capMicroUsd: 20_000_000, now: NOW, newId: () => "g1" }
  const made = await (await assistant(post("", { request: "Weekly yield for refunds to amira@nilegold.com" }), "", deps)).json() as { verdict: { guard: { safePrompt: string; guardId: number } }; definition: { origin: { request: string } } }
  expect(JSON.stringify(model.asks)).not.toContain("amira@nilegold.com")
  expect(model.asks[0]?.turns[0]?.content).toContain("[EMAIL_1]")
  expect(made.verdict.guard).toMatchObject({ guardId: 7, safePrompt: "Weekly yield for refunds to [EMAIL_1]" })
  expect(made.definition.origin.request).not.toContain("amira@")
  expect(calls[0]).toMatchObject({ path: "/v1/guard:prompt" })
  // A cell without the guard: nothing reaches the model.
  const unguarded: Cell = { ...cell, post: async (path, body) => path === "/v1/guard:prompt" ? { kind: "answer", status: 404, body: null } : cell.post(path, body) }
  const silent = scripted(proposed([tile()]))
  const refusedCall = await assistant(post("", { request: "Weekly yield" }), "", { ...deps, cell: unguarded, model: silent, newId: () => "g2" })
  expect(refusedCall.status).toBe(503)
  expect(silent.asks).toHaveLength(0)
})

const scope = { orgId: "org_demo", environmentId: "analytics", userId: "u1" }
const totals = async (db: D1Database) => ({
  spent: (await db.prepare("SELECT COALESCE(SUM(cost_micro_usd), 0) AS n FROM agent_spend").first<{ n: number }>())?.n ?? 0,
  open: (await db.prepare("SELECT COALESCE(SUM(reserved_micro_usd), 0) AS n FROM agent_reservation WHERE state = 'reserved'").first<{ n: number }>())?.n ?? 0,
})

test("parallel requests reserve before any provider call: only as many as the cap covers reach the model, and spend never passes the cap", async () => {
  const db = stage()
  const { cell } = cellDouble()
  // Every model call waits on one gate, so all requests are in flight together before any settles.
  let open: () => void = () => undefined
  const gate = new Promise<void>((resolve) => { open = resolve })
  let calls = 0
  const model: Model = { name: "slow", answer: async () => { calls += 1; await gate; return { json: proposed([tile()]), model: "slow", inputTokens: 1, outputTokens: 1, costMicroUsd: 4000 } } }
  // Each request reserves 4 calls at 4000 = 16000: a 40000 cap covers two in flight, never three.
  const deps: AssistantDeps = { db, cell, scope, model, capMicroUsd: 40_000, perCallMicroUsd: 4000, now: NOW }
  let n = 0
  const pending = Array.from({ length: 6 }, () => assistant(post("", { request: "Weekly gold yield" }), "", { ...deps, newId: () => `c${++n}` }))
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(calls).toBe(2)
  open()
  const statuses = (await Promise.all(pending)).map((response) => response.status).sort()
  expect(statuses).toEqual([200, 200, 429, 429, 429, 429])
  const after = await totals(db)
  expect(after).toEqual({ spent: 8000, open: 0 })
  expect(after.spent + after.open).toBeLessThanOrEqual(40_000)
})

test("a paid failure takes an hourly slot: the quota counts every admitted request, not only proposals", async () => {
  const db = stage()
  const { cell } = cellDouble()
  let n = 0
  const deps: AssistantDeps = { db, cell, scope, model: scripted(), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => `q${++n}` }
  for (let request = 0; request < HOURLY_PROPOSALS; request += 1) expect((await assistant(post("", { request: "Anything" }), "", deps)).status).toBe(502)
  const quota = await assistant(post("", { request: "Anything" }), "", deps)
  expect(quota.status).toBe(429)
  expect(quota.headers.get("x-norn-limit")).toBe("proposals_hourly")
  // The failed calls were paid for, each once.
  expect((await totals(db)).spent).toBe(HOURLY_PROPOSALS * 4000)
})

test("uncertain usage keeps the whole reservation: a primary that threw after dispatch is paid for at the bound", async () => {
  const db = stage()
  const { cell } = cellDouble()
  const thrown: Model = { name: "primary", answer: async () => { throw new Error("connection reset") } }
  const deps: AssistantDeps = { db, cell, scope, model: withFallback(thrown, scripted(proposed([tile()]))), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "u1" }
  const made = await assistant(post("", { request: "Weekly gold yield" }), "", deps)
  expect(made.status).toBe(200)
  expect(await totals(db)).toEqual({ spent: 4000 * CALLS_PER_REQUEST, open: 0 })
})

test("a crash after dispatch keeps the reservation counted; settlement happens once per request id", async () => {
  const db = stage()
  const { cell } = cellDouble()
  const crashing: Model = { name: "crash", answer: async () => { throw new Error("isolate died") } }
  const deps: AssistantDeps = { db, cell, scope, model: crashing, capMicroUsd: 32_000, perCallMicroUsd: 4000, now: NOW, newId: () => "x1" }
  await expect(assistant(post("", { request: "Weekly gold yield" }), "", deps)).rejects.toThrow("isolate died")
  expect(await totals(db)).toEqual({ spent: 16_000, open: 0 })
  // A request whose settlement never ran (the isolate died) keeps its reservation open, and it counts.
  expect(await reserve(db, { requestId: "prop_lost", userId: "u1", amountMicroUsd: 16_000, capMicroUsd: 32_000, hourly: HOURLY_PROPOSALS, now: NOW })).toEqual({ ok: true })
  expect(await totals(db)).toEqual({ spent: 16_000, open: 16_000 })
  const capped = await assistant(post("", { request: "Weekly gold yield" }), "", { ...deps, model: scripted(proposed([tile()])), newId: () => "x2" })
  expect(capped.status).toBe(429)
  expect(capped.headers.get("x-norn-limit")).toBe("spend_cap")
  // Settling twice records once.
  const answer: Answer = { json: null, model: "m", inputTokens: 1, outputTokens: 1, costMicroUsd: 100 }
  await settle(db, { requestId: "prop_lost", userId: "u1", proposalId: null, answers: [answer], now: NOW })
  await settle(db, { requestId: "prop_lost", userId: "u1", proposalId: null, answers: [answer, answer], now: NOW })
  expect(await totals(db)).toEqual({ spent: 16_100, open: 0 })
})

/** A dashboard as Studio stores it, with members the assistant must never forward. */
const stored = (filterValue: string) => ({
  title: "Refinery weekly",
  internalNote: "board pack for amira@nilegold.com",
  origin: { by: "person", signer: "amira@nilegold.com" },
  tiles: [{ id: "tile-1", title: "Gold yield", question: { irVersion: "0.1", name: "studio.gold_yield", objectSet: "demo.Conversion", aggregate: [{ metric: "demo.yield" }], groupBy: [{ path: { property: "occurred_at" }, grain: "week" }], filter: [{ path: { property: "metal" }, operator: "eq", value: filterValue }], window: { start: "2026-05-18T00:00:00Z", end: "2026-11-16T00:00:00Z" }, calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 5000 } }, view: { kind: "chart", chart: "line" }, layout: { x: 0, y: 0, w: 2, h: 1 }, annotations: ["call amira@nilegold.com"] }],
})
function withDefinitions(cell: Cell, definitions: Record<string, unknown>): Cell {
  return { ...cell, get: async (path) => { const id = decodeURIComponent(path.replace("/v1/definitions/", "")); return id in definitions ? { kind: "answer", status: 200, body: definitions[id] } : cell.get(path) } }
}

/**
 * The cell's guard as privacy_guard.rs behaves: each call numbers its own tokens from 1, an 8-15 digit
 * run with a dash (so every YYYY-MM-DD date) is a [PHONE_n], an address is an [EMAIL_n], more than
 * 20,000 characters is refused, and a restore gives back that call's values.
 */
function realGuard(cell: Cell) {
  const calls: { text: string; key: string }[] = []
  const maps: Map<string, string>[] = []
  const post: Cell["post"] = async (path, body) => {
    if (path === "/v1/guard:prompt") {
      const { text, idempotencyKey } = body as { text: string; idempotencyKey: string }
      calls.push({ text, key: idempotencyKey })
      if (text.length > 20_000) return { kind: "answer", status: 400, body: { code: "guard.invalid", message: "text is at most 20000 characters" } }
      const map = new Map<string, string>()
      const counts: Record<string, number> = {}
      const token = (kind: string) => (value: string) => { const name = `[${kind}_${(counts[kind] = (counts[kind] ?? 0) + 1)}]`; map.set(name, value); return name }
      const safePrompt = text.replace(/[\w.+-]+@[\w-]+\.[\w.]+/g, token("EMAIL")).replace(/(?<![0-9A-Za-z])\d{4}-\d{2}-\d{2}(?![0-9])/g, token("PHONE"))
      maps.push(map)
      return { kind: "answer", status: 200, body: { guardId: maps.length, receiptHash: "g", safePrompt, tokens: [...map.keys()].map((t) => ({ token: t, kind: "x", label: "x", source: "detected", revealable: true })) } }
    }
    const restore = /^\/v1\/guard\/(\d+):restore$/.exec(path)
    if (restore !== null) {
      const map = maps[Number(restore[1]) - 1] ?? new Map<string, string>()
      const text = (body as { text: string }).text.replace(/\[[A-Z]+_\d+\]/g, (t) => map.get(t) ?? t)
      return { kind: "answer", status: 200, body: { segments: [{ text }] } }
    }
    return cell.post(path, body)
  }
  return { cell: { ...cell, post }, calls }
}

test("dates pass the boundary as dates; workspace content the guard would change is refused (422) before any call; the person's own words tokenize and restore in one numbering", async () => {
  const db = stage()
  const base = cellDouble().cell
  const { cell, calls } = realGuard(withDefinitions(base, {
    "studio.dashboard.weekly": { id: "studio.dashboard.weekly", kind: "dashboard", version: 3, payload: stored("gold") },
    "studio.dashboard.pii": { id: "studio.dashboard.pii", kind: "dashboard", version: 1, payload: stored("amira@nilegold.com") },
    "studio.dashboard.pii_title": { id: "studio.dashboard.pii_title", kind: "dashboard", version: 1, payload: { ...stored("gold"), title: "Refunds for amira@nilegold.com" } },
  }))
  const model = scripted({ ...proposed([tile()]), note: "Monthly gold yield, filtered for [EMAIL_1]." })
  let n = 0
  const deps: AssistantDeps = { db, cell, scope, model, capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => `d${++n}` }
  // (a) + (c): an edit with dated tiles, asked in words that carry an address and a date.
  const made = await assistant(post("", { request: "Make it monthly from 2026-06-01 for amira@nilegold.com", dashboardId: "studio.dashboard.weekly" }), "", deps)
  expect(made.status).toBe(200)
  const [ask] = model.asks
  expect(ask?.system).toContain("Today is 2026-11-12.")
  expect(ask?.turns[0]?.content).toContain("Make it monthly from 2026-06-01 for [EMAIL_1]")
  expect(ask?.turns[0]?.content).toContain(`"start": "2026-05-18"`)
  expect(ask?.turns[0]?.content).toContain(`"end": "2026-11-16"`)
  const sent = JSON.stringify(model.asks)
  expect(sent).not.toContain("amira@nilegold.com")
  expect(sent).not.toContain("[PHONE_")
  for (const member of ["internalNote", "origin", "annotations", "tile-1", "freshness", "irVersion"]) expect(sent).not.toContain(member)
  // The note's token is restored from the request's own guard call: the right value, never another call's.
  const proposal = await made.json() as { targetVersion: number; verdict: { guard: { reply: { text: string }[] } } }
  expect(proposal.targetVersion).toBe(3)
  expect(proposal.verdict.guard.reply.map(({ text }) => text).join("")).toBe("Monthly gold yield, filtered for amira@nilegold.com.")
  // Every boundary check left its text unchanged; only the request's own call tokenized.
  expect(calls.filter(({ key }) => key === "prop_d1")).toHaveLength(1)
  // (b): an address in a dashboard's filter value or title is refused before any call, and nothing is spent.
  for (const dashboardId of ["studio.dashboard.pii", "studio.dashboard.pii_title"]) {
    const silent = scripted(proposed([tile()]))
    const refusedEdit = await assistant(post("", { request: "Make it monthly", dashboardId }), "", { ...deps, model: silent })
    expect(refusedEdit.status).toBe(422)
    expect(silent.asks).toHaveLength(0)
  }
  expect(await totals(db)).toEqual({ spent: 4000, open: 0 })
  // Not a dashboard: refused, nothing sent, nothing reserved.
  const flowCell = withDefinitions(cell, { "ops.flow.refunds": { id: "ops.flow.refunds", kind: "flow", version: 1, payload: { title: "Refunds", tiles: stored("gold").tiles } } })
  const silent = scripted(proposed([tile()]))
  expect((await assistant(post("", { request: "Change it", dashboardId: "ops.flow.refunds" }), "", { ...deps, cell: flowCell, model: silent })).status).toBe(422)
  expect(silent.asks).toHaveLength(0)
  // A dashboard with parts this builder does not write is refused too, not forwarded raw.
  expect(editableDashboard({ title: "x", tiles: [{ title: "t", question: { aggregate: [{ metric: "m" }, { metric: "n" }] }, view: { kind: "table" }, layout: { w: 1 } }] })).toBeNull()
  expect(editableDashboard(stored("gold"))?.tiles[0]).toEqual({ title: "Gold yield", metric: "demo.yield", groupBy: "occurred_at", grain: "week", filters: [{ property: "metal", operator: "eq", value: "gold" }], start: "2026-05-18", end: "2026-11-16", view: "line", width: "full" })
})

test("catalog text and retry feedback are checked at the boundary: a catalog the guard would change is refused, feedback it would change is not sent", async () => {
  const { cell } = cellDouble()
  const guard = async (text: string) => text.replace(/[\w.+-]+@[\w-]+\.[\w.]+/g, "[EMAIL_1]").replace(/(?<![0-9A-Za-z])\d{4}-\d{2}-\d{2}(?![0-9])/g, "[PHONE_1]")
  const leaky: CellCatalog = { ...cellCatalog, metrics: cellCatalog.metrics.map((metric) => metric.name === "demo.input" ? { ...metric, label: "Input (owner amira@nilegold.com)" } : metric) }
  const leakyCatalog: Cell = { ...cell, get: async (path) => path === "/v1/questions:catalog" ? { kind: "answer", status: 200, body: leaky } : cell.get(path) }
  const silent = scripted(proposed([tile()]))
  const refusedCatalog = await propose(leakyCatalog, silent, "Weekly yield", { proposalId: "prop_a", targetId: "studio.dashboard.x", now: NOW, guard, maxCalls: 4 })
  expect(refusedCatalog.failureKind).toBe("private")
  expect(silent.asks).toHaveLength(0)
  // The checker's findings quote a value: the repair is not sent; the refused proposal stands.
  const leakyFindings: Cell = { ...cell, post: async (path, body) => path === "/v1/definitions:admit" ? { kind: "answer", status: 200, body: { admitted: false, diagnostics: [{ code: "definition.filter_value", message: "no rows for owner amira@nilegold.com", span: { pointer: "/tiles/0/question" } }] } } : cell.post(path, body) }
  const model = scripted(proposed([tile()]), proposed([tile()]))
  const outcome = await propose(leakyFindings, model, "Weekly yield", { proposalId: "prop_b", targetId: "studio.dashboard.x", now: NOW, guard, maxCalls: 4 })
  expect(model.asks).toHaveLength(1)
  expect(outcome.verdict?.admitted).toBe(false)
  // Findings with only dates are sent as written.
  const datedFindings: Cell = { ...cell, post: async (path, body) => path === "/v1/definitions:admit" ? { kind: "answer", status: 200, body: { admitted: false, diagnostics: [{ code: "definition.window", message: "the window ends 2026-11-16, after today", span: { pointer: "/tiles/0/question" } }] } } : cell.post(path, body) }
  const dated = scripted(proposed([tile()]), proposed([tile()]))
  await propose(datedFindings, dated, "Weekly yield", { proposalId: "prop_e", targetId: "studio.dashboard.x", now: NOW, guard, maxCalls: 4 })
  expect(dated.asks[1]?.turns.at(-1)?.content).toContain("the window ends 2026-11-16, after today")
  // The guard unavailable at the boundary: nothing is sent.
  const none = scripted(proposed([tile()]))
  expect((await propose(cell, none, "Weekly yield", { proposalId: "prop_c", targetId: "studio.dashboard.x", now: NOW, guard: async () => null, maxCalls: 4 })).failureKind).toBe("guard")
  expect(none.asks).toHaveLength(0)
  // The reservation's call budget bounds the calls made.
  const bounded = scripted(proposed([tile({ view: "pie", groupBy: "metal", grain: "" })]), proposed([tile()]))
  await propose(cell, bounded, "Yield by metal", { proposalId: "prop_d", targetId: "studio.dashboard.x", now: NOW, guard: async (text) => text, maxCalls: 1 })
  expect(bounded.asks).toHaveLength(1)
})

test("a catalog and a schema over the guard's 20,000-character limit are checked in pieces and still answered", async () => {
  const db = stage()
  const { cell: base } = cellDouble()
  const big: CellCatalog = { ...cellCatalog, metrics: [...cellCatalog.metrics, ...Array.from({ length: 500 }, (_, index) => ({ ...cellCatalog.metrics[0], name: `demo.extra_measure_for_the_refinery_ledger_${index}`, label: `Extra measure number ${index} of the refinery ledger` }))] as CellCatalog["metrics"] }
  // The schema alone is longer than one guard call takes; checked one member per line, it passes in pieces.
  expect(JSON.stringify(proposalSchema(big)).length).toBeGreaterThan(20_000)
  const bigCell: Cell = { ...base, get: async (path) => path === "/v1/questions:catalog" ? { kind: "answer", status: 200, body: big } : base.get(path) }
  const { cell, calls } = realGuard(bigCell)
  const model = scripted(proposed([tile()]))
  const made = await assistant(post("", { request: "Weekly gold yield" }), "", { db, cell, scope, model, capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "big" })
  expect(made.status).toBe(200)
  expect(model.asks[0]?.system.length).toBeGreaterThan(25_000)
  expect(Math.max(...calls.map(({ text }) => text.length))).toBeLessThanOrEqual(18_000)
  expect(calls.length).toBeGreaterThan(2)
})

test("nothing dispatched or a definite unbilled answer releases the reservation; an uncertain failure keeps it", async () => {
  const { cell } = cellDouble()
  const httpError = (status: number) => Object.assign(new Error(`status ${status}`), { status })
  // Before dispatch: the cell's catalog read throws.
  {
    const db = stage()
    const broken: Cell = { ...cell, get: async () => { throw new Error("cell unreachable") } }
    await expect(assistant(post("", { request: "Weekly yield" }), "", { db, cell: broken, scope, model: scripted(), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "p1" })).rejects.toThrow("cell unreachable")
    expect(await totals(db)).toEqual({ spent: 0, open: 0 })
  }
  // A definite 500 from the model (no fallback): nothing charged, the request answers 502.
  for (const status of [429, 500]) {
    const db = stage()
    const failing: Model = { name: "m", answer: async () => { throw httpError(status) } }
    const response = await assistant(post("", { request: "Weekly yield" }), "", { db, cell, scope, model: failing, capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "p2" })
    expect(response.status).toBe(502)
    expect(await totals(db)).toEqual({ spent: 0, open: 0 })
  }
  // A definite 429 from the primary, then the fallback answers: only the fallback's call is charged.
  {
    const db = stage()
    const limited: Model = { name: "primary", answer: async () => { throw httpError(429) } }
    expect((await assistant(post("", { request: "Weekly yield" }), "", { db, cell, scope, model: withFallback(limited, scripted(proposed([tile()]))), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "p3" })).status).toBe(200)
    expect(await totals(db)).toEqual({ spent: 4000, open: 0 })
  }
  // A timeout after dispatch: usage unknown, the whole reservation is kept.
  {
    const db = stage()
    const timedOut: Model = { name: "m", answer: async () => { throw new DOMException("The operation timed out.", "TimeoutError") } }
    await expect(assistant(post("", { request: "Weekly yield" }), "", { db, cell, scope, model: timedOut, capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "p4" })).rejects.toThrow("timed out")
    expect(await totals(db)).toEqual({ spent: 4000 * CALLS_PER_REQUEST, open: 0 })
  }
  // The work is handed to waitUntil, so a disconnect cannot stop it before settlement.
  {
    const db = stage()
    const kept: Promise<unknown>[] = []
    await assistant(post("", { request: "Weekly yield" }), "", { db, cell, scope, model: scripted(proposed([tile()])), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "p5", waitUntil: (work) => kept.push(work) })
    expect(kept).toHaveLength(1)
  }
})

test("an unpriced model is never configured, and a Workers AI answer without its neurons is charged at the bound", async () => {
  const gateway = { AI_GATEWAY_URL: `https://gateway.ai.cloudflare.com/v1/${"a".repeat(32)}/studio`, AI_GATEWAY_TOKEN: "t" }
  expect(configuredModel(gateway)?.name).toBe("claude-sonnet-5-5")
  expect(configuredModel({ ...gateway, AGENT_MODEL: "claude-unlisted-9" })).toBeUndefined()
  const ai = workersAi({ accountId: "a", token: "t", model: "@cf/x", fetch: async () => Response.json({ success: true, result: { response: "{\"a\":1}", usage: { prompt_tokens: 10, completion_tokens: 5 } } }) })
  const ask = { system: "s", turns: [{ role: "user" as const, content: "hello" }], schema: {}, maxTokens: 100 }
  const answer = await ai.answer(ask)
  expect(answer.costMicroUsd).toBe(worstCallMicroUsd(new TextEncoder().encode("s{}hello").byteLength, 100))
  expect(answer.costMicroUsd).toBeGreaterThan(0)
  const reported = workersAi({ accountId: "a", token: "t", model: "@cf/x", fetch: async () => Response.json({ success: true, result: { response: "{}", usage: { neurons: 1000 } } }) })
  expect((await reported.answer(ask)).costMicroUsd).toBe(11_000)
})

test("a line too long for one guard call is too large, not private; a date inside a phone number stays visible to the detector", async () => {
  expect(await unchangedByGuard(async (text) => text, "x".repeat(18_001))).toBe("too_large")
  expect(await unchangedByGuard(async (text) => text, `${"x".repeat(17_000)}\n${"y".repeat(17_000)}`)).toBe("clean")
  expect("ends 2026-05-22.".replace(ISO_DATE, "DATE")).toBe("ends DATE.")
  expect(`"start": "2026-05-18T00:00:00Z"`.replace(ISO_DATE, "DATE")).toBe(`"start": "DATE"`)
  for (const phone of ["010-2012-05-22", "2012-05-22-77", "+20 2012-05-22"]) expect(phone.replace(ISO_DATE, "DATE")).toBe(phone.startsWith("+") ? "+20 DATE" : phone)
})

test("concurrent proposal accept/decline is fenced to one cell install, and a non-terminal error releases the claim", async () => {
  const db = stage()
  const base = cellDouble()
  let releaseInstall!: () => void
  let installStarted!: () => void
  let installs = 0
  let failNextInstall = false
  const started = new Promise<void>((resolve) => { installStarted = resolve })
  const gatedCell: Cell = {
    ...base.cell,
    post: async (path, body) => {
      if (path === "/v1/definitions:install") {
        installs += 1
        if (failNextInstall) {
          failNextInstall = false
          return { kind: "answer", status: 502, body: { error: "cell busy" } }
        }
        installStarted()
        await new Promise<void>((resolve) => { releaseInstall = resolve })
      }
      return base.cell.post(path, body)
    },
  }
  const deps: AssistantDeps = { db, cell: gatedCell, scope, model: scripted(proposed([tile()])), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "race1" }
  const created = await assistant(post("", { request: "Weekly gold yield" }), "", deps)
  expect(created.status).toBe(200)
  const proposal = await created.json() as { id: string; decidedAtMs: number | null }
  expect(proposal.decidedAtMs).toBeNull()

  // A transient 502 releases the accept claim so a retry can proceed.
  failNextInstall = true
  expect((await assistant(post(`/${proposal.id}/accept`, {}), `/${proposal.id}/accept`, deps)).status).toBe(502)

  // Start one accept and pause mid-install: a concurrent accept AND a concurrent decline both refuse 409.
  const firstAccept = assistant(post(`/${proposal.id}/accept`, {}), `/${proposal.id}/accept`, deps)
  await started
  const secondAccept = await assistant(post(`/${proposal.id}/accept`, {}), `/${proposal.id}/accept`, deps)
  const racedDecline = await assistant(post(`/${proposal.id}/decline`, {}), `/${proposal.id}/decline`, deps)
  expect(secondAccept.status).toBe(409)
  expect(racedDecline.status).toBe(409)
  releaseInstall()
  const settled = await firstAccept
  expect(settled.status).toBe(200)
  expect((await settled.json()) as { state: string }).toMatchObject({ state: "accepted" })
  // Only the failed 502 attempt + the one winning install reached the cell.
  expect(installs).toBe(2)
})

test("withFallback preserves primary billed or uncertain spend when the fallback throws a definite unbilled status", async () => {
  const { cell } = cellDouble()
  const httpError = (status: number) => Object.assign(new Error(`status ${status}`), { status })
  const unbilledFallback: Model = { name: "fallback", answer: async () => { throw httpError(429) } }

  // 1. Primary returns a billed json:null refusal, then fallback throws definite 429: primary spend is recorded.
  {
    const db = stage()
    const billedPrimary: Model = {
      name: "primary",
      answer: async () => ({ json: null, model: "primary", inputTokens: 100, outputTokens: 20, costMicroUsd: 4000, failure: "The model declined this request." }),
    }
    const response = await assistant(
      post("", { request: "Weekly yield" }),
      "",
      { db, cell, scope, model: withFallback(billedPrimary, unbilledFallback), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "fb1" },
    )
    expect(response.status).toBe(502)
    expect(await totals(db)).toEqual({ spent: 4000, open: 0 })
  }

  // 2. Primary throws an uncertain timeout, then fallback throws definite 429: full reservation is retained.
  {
    const db = stage()
    const uncertainPrimary: Model = {
      name: "primary",
      answer: async () => { throw new DOMException("The operation timed out.", "TimeoutError") },
    }
    const response = await assistant(
      post("", { request: "Weekly yield" }),
      "",
      { db, cell, scope, model: withFallback(uncertainPrimary, unbilledFallback), capMicroUsd: 20_000_000, perCallMicroUsd: 4000, now: NOW, newId: () => "fb2" },
    )
    expect(response.status).toBe(502)
    expect(await totals(db)).toEqual({ spent: 4000 * CALLS_PER_REQUEST, open: 0 })
  }
})
