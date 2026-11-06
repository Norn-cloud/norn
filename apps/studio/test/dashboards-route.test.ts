import { expect, test } from "bun:test"
import { createHash } from "node:crypto"
import { ANALYTICS } from "../worker/questions.ts"
import { restate } from "../worker/question-tree.ts"
import { dashboardsRoute, restateDashboard } from "../worker/dashboards.ts"
import { cellDefinitions, saveDashboard, sha256, type Dashboard } from "../src/lib/definitions.ts"

const question = ANALYTICS.example as never
const dashboard = { title: " Refinery weekly ", tiles: [{ title: "Gold yield", question }, { title: "  ", question }] }
const call = (body: string, method = "POST") => dashboardsRoute(new Request("https://studio.test/api/dashboards/check", method === "POST" ? { method, body } : { method }))

test("a dashboard reads as its title, then each tile by number with its question's words; an untitled tile says so", () => {
  expect(restateDashboard(dashboard)).toBe(`Refinery weekly\n1. Gold yield: ${restate(question)}\n2. Untitled question: ${restate(question)}`)
})

test("the check route states the words and their SHA-256, and refuses what is not a dashboard", async () => {
  const response = await call(JSON.stringify({ dashboard }))
  expect(response.status).toBe(200)
  const { restatement } = await response.json() as { restatement: { text: string; sha256: string } }
  expect(restatement.text).toBe(restateDashboard(dashboard))
  expect(restatement.sha256).toBe(createHash("sha256").update(restatement.text).digest("hex"))
  expect((await call("not json")).status).toBe(400)
  expect((await call(JSON.stringify({ dashboard: { title: "x", tiles: [{ title: "t" }] } }))).status).toBe(400)
  expect((await call("", "GET")).status).toBe(405)
  expect((await call(JSON.stringify({ dashboard: { title: "x".repeat(300_000), tiles: [] } }))).status).toBe(413)
})

const scope = async () => ({ orgId: "org_demo", environmentId: "analytics" })
const payload: Dashboard = { title: "Refinery weekly", tiles: [] }

test("save installs with the sign-off hash; the cell's mismatch comes back with its words only when they hash as it says", async () => {
  const bodies: unknown[] = []
  let answer = (_: unknown) => Response.json({ installed: true, id: "d", version: 1, receipt: { receiptHash: "r", commitSeq: 1, recordedAtMs: 0 } })
  const store = cellDefinitions(async (_input, init) => {
    const body = JSON.parse(String(init?.body ?? "null"))
    if (String(_input).includes(":admit")) return Response.json({ admitted: true, diagnostics: [] })
    bodies.push(body)
    return answer(body)
  }, "/api/host", scope)
  expect(await saveDashboard(store, "d", payload, undefined, "a".repeat(64))).toMatchObject({ saved: { id: "d", version: 1 } })
  expect(bodies[0]).toEqual({ kind: "dashboard", id: "d", payload, signoff: { restatementSha256: "a".repeat(64) } })
  const theirs = "Refinery weekly, as the cell reads it"
  answer = () => Response.json({ code: "definition.signoff_mismatch", restatement: theirs, restatementSha256: digest(theirs) }, { status: 422 })
  expect(await saveDashboard(store, "d", payload, 1, "a".repeat(64))).toEqual({ signoff: "mismatch", restatement: { text: theirs, sha256: await sha256(theirs) } })
  answer = () => Response.json({ code: "definition.signoff_mismatch", restatement: theirs, restatementSha256: "b".repeat(64) }, { status: 422 })
  expect(await saveDashboard(store, "d", payload, 1, "a".repeat(64))).toEqual({ signoff: "mismatch" })
  answer = () => Response.json({ code: "definition.signoff_unavailable" }, { status: 422 })
  expect(await saveDashboard(store, "d", payload, 1, "a".repeat(64))).toEqual({ signoff: "unavailable" })
})

test("reopening reads the sign-off the cell recorded", async () => {
  const store = cellDefinitions(async () => Response.json({ id: "d", kind: "dashboard", version: 3, payload, signoff: { restatementSha256: "f".repeat(64) } }), "/api/host", scope)
  expect(await store.get("d")).toEqual({ id: "d", version: 3, dashboard: payload, signoff: { restatementSha256: "f".repeat(64) } })
})

// The digest the stand-in cell sends for its own words.
function digest(text: string) { return createHash("sha256").update(text).digest("hex") }
