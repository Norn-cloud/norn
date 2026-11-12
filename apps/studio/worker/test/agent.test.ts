import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compile, proposalSchema, propose, systemPrompt, type Proposed } from "../agent.ts"
import { assistant, type AssistantDeps } from "../assistant.ts"
import type { Cell, CellAnswer } from "../cell.ts"
import { claudeCost, neuronCost, withFallback, type Answer, type Ask, type Model } from "../model.ts"
import type { CellCatalog } from "../questions.ts"

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
  const outcome = await propose(cell, model, "Yield by metal", { proposalId: "prop_1", targetId: "studio.dashboard.x", now: NOW })
  expect(outcome.verdict?.admitted).toBe(true)
  expect(outcome.answers).toHaveLength(2)
  expect(model.asks[1]?.turns.at(-1)?.content).toContain("tile 1: a pie shows the parts of one whole (definition.view_invalid at /tiles/0/view)")
  expect(outcome.definition?.tiles[0]?.view).toEqual({ kind: "chart", chart: "bar" })
  // Still refused after the retry: the last proposal and its refusal, never a guess.
  const stubborn = await propose(cell, scripted(proposed([tile({ view: "pie", groupBy: "metal", grain: "" })]), proposed([tile({ view: "pie", groupBy: "metal", grain: "" })])), "Yield by metal", { proposalId: "prop_2", targetId: "studio.dashboard.x", now: NOW })
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
function stage() {
  const db = new Database(":memory:")
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0008_dashboard_proposal.sql"), "utf8"))
  return d1(db)
}
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
  const deps: AssistantDeps = { db, cell, scope: { orgId: "org_demo", environmentId: "analytics", userId: "u1" }, model: scripted(pie, pie), capMicroUsd: 8000, now: NOW }
  const refusedProposal = await (await assistant(post("", { request: "Yield by metal as a pie" }), "", deps)).json() as { id: string; state: string; verdict: { findings: { code: string }[] } }
  expect(refusedProposal.state).toBe("refused")
  expect(refusedProposal.verdict.findings[0]?.code).toBe("definition.view_invalid")
  expect((await assistant(post(`/${refusedProposal.id}/accept`), `/${refusedProposal.id}/accept`, deps)).status).toBe(409)
  // Two calls at 4000 each reached the 8000 cap.
  const capped = await assistant(post("", { request: "Anything" }), "", { ...deps, model: scripted(proposed([tile()])) })
  expect(capped.status).toBe(429)
  expect((await assistant(post("", { request: "  " }), "", deps)).status).toBe(400)
  expect((await assistant(post("", { request: "x" }), "", { ...deps, model: undefined })).status).toBe(503)
})
