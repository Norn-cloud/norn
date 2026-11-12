import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { Cell, CellAnswer } from "../cell.ts"
import { questions, studioCatalog, windows, type CellCatalog } from "../questions.ts"

const fixture = (name: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../../fixtures/demo/answers", name), "utf8"))
const objectType = fixture("scrap-yield.object-type.json")
/** The cell's catalog document for the demo analytics cell, as `GET /v1/questions:catalog` serves it. */
const catalog: CellCatalog = { objectTypes: [objectType], metrics: fixture("scrap-yield.metrics.json"), grains: ["week"], operators: ["eq", "ne"], maxGroupBy: 1, maxBuckets: 104 }
const question = { ...fixture("scrap-yield.question.json"), name: "studio.gold_yield" }

/** A cell that answers each route with what the test gives it, recording every call. */
function cellOf(answers: Record<string, CellAnswer>) {
  const calls: { method: string; path: string; body?: unknown }[] = []
  const answer = (path: string) => answers[path] ?? { kind: "answer", status: 404, body: null }
  const cell: Cell = {
    get: async (path) => { calls.push({ method: "GET", path }); return answer(path) },
    post: async (path, body) => { calls.push({ method: "POST", path, body }); return answer(path) },
  }
  return { cell, calls }
}
const ok = (body: unknown): CellAnswer => ({ kind: "answer", status: 200, body })
const get = (route: string) => new Request(`https://studio.test/api/questions${route}`)
const post = (route: string, body: unknown) => new Request(`https://studio.test/api/questions${route}`, { method: "POST", body: JSON.stringify(body) })

test("the builder's catalogue is the cell's: one object type, its properties and units, the metrics over it and the engine's grains", async () => {
  const { cell, calls } = cellOf({ "/v1/questions:catalog": ok(catalog) })
  const served = await (await questions(get("/catalog"), cell, "/catalog")).json() as ReturnType<typeof studioCatalog>
  expect(calls).toEqual([{ method: "GET", path: "/v1/questions:catalog" }])
  expect(served.objectSet).toBe("demo.Conversion")
  expect(served.metrics.map(({ name, type }) => `${name}:${type}`)).toEqual(["demo.input:simple", "demo.recovered:simple", "demo.yield:ratio"])
  expect(served.properties).toContainEqual({ name: "recovered", kind: "decimal", unit: "g" })
  expect(served.properties).toContainEqual({ name: "metal", kind: "text", unit: null })
  expect(served.grains).toEqual(["week"])
  expect(served.declarations.objectType).toEqual(objectType)
  // Nothing readable is an empty catalogue, not an error.
  expect(studioCatalog({ ...catalog, objectTypes: [], metrics: [] })).toMatchObject({ objectSet: null, metrics: [], properties: [] })
})

test("a check is the cell admitting a question definition: admitted with its plan and the checker's words, or refused with each finding's pointer", async () => {
  const restatement = "Yield of conversion records, by week of occurred at"
  const admitted = cellOf({ "/v1/questions:catalog": ok(catalog), "/v1/definitions:admit": ok({ admitted: true, diagnostics: [], restatement, restatementSha256: "ab".repeat(32) }) })
  const verdict = await (await questions(post("/check", { question }), admitted.cell, "/check")).json() as Record<string, unknown>
  expect(admitted.calls.at(-1)).toEqual({ method: "POST", path: "/v1/definitions:admit", body: { kind: "question", id: "studio.gold_yield", payload: question } })
  expect(verdict).toMatchObject({ kind: "question", admitted: true, findings: [], restatement: { text: restatement, sha256: "ab".repeat(32) } })
  expect(verdict.plan).toMatchObject({ grain: "demo.Conversion", sums: [{ metric: "demo.recovered", property: "recovered" }, { metric: "demo.input", property: "input" }], metrics: [{ name: "demo.yield", label: "Yield" }] })
  const finding = { code: "core.compute.unsupported_engine", severity: "error", message: "Question is outside the bounded keyed sum/ratio profile", span: { pointer: "/groupBy/0" } }
  const refused = cellOf({ "/v1/questions:catalog": ok(catalog), "/v1/definitions:admit": ok({ admitted: false, diagnostics: [finding], restatement: null, restatementSha256: null }) })
  expect(await (await questions(post("/check", { question }), refused.cell, "/check")).json()).toMatchObject({ admitted: false, plan: null, restatement: null, findings: [{ code: finding.code, message: finding.message, pointer: "/groupBy/0" }] })
})

test("the cell's denial is passed through as it wrote it; no cell, no catalog route and a malformed check say so", async () => {
  const denial = { code: "authz.denied", message: "the read request is not authorized" }
  const denied = cellOf({ "/v1/questions:catalog": { kind: "answer", status: 403, body: denial } })
  const response = await questions(get("/catalog"), denied.cell, "/catalog")
  expect([response.status, await response.json()]).toEqual([403, denial])
  expect((await questions(get("/catalog"), cellOf({ "/v1/questions:catalog": { kind: "no-cell" } }).cell, "/catalog")).status).toBe(409)
  expect((await questions(get("/catalog"), cellOf({}).cell, "/catalog")).status).toBe(503)
  expect((await questions(post("/check", { nope: 1 }), cellOf({}).cell, "/check")).status).toBe(400)
  expect((await questions(get("/elsewhere"), cellOf({}).cell, "/elsewhere")).status).toBe(404)
})

test("recommended windows are whole weeks ending on the coming Monday", () => {
  // Thursday 12 November 2026: the next Monday is the 16th.
  const [twelve, twentySix] = windows(Date.UTC(2026, 10, 12, 15, 30))
  expect(twelve?.value.window).toEqual({ start: "2026-08-24T00:00:00Z", end: "2026-11-16T00:00:00Z" })
  expect(twentySix?.recommended).toBe(true)
  // On a Monday the window ends a week later, never today.
  expect(windows(Date.UTC(2026, 10, 16))[0]?.value.window.end).toBe("2026-11-23T00:00:00Z")
})
