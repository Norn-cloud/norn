import { expect, test } from "bun:test"
import { createHash } from "node:crypto"
import { cellDefinitions, saveDashboard, sha256, type Dashboard } from "../src/lib/definitions.ts"

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

test("the cell's check states the dashboard in its own words; words that do not hash as it says are dropped", async () => {
  const text = "Refinery weekly\n1. Gold yield: the question"
  let reply: Record<string, unknown> = { admitted: true, diagnostics: [], restatement: text, restatementSha256: digest(text) }
  const store = cellDefinitions(async () => Response.json(reply), "/api/host", scope)
  expect(await store.admit("d", payload)).toEqual({ admitted: true, restatement: { text, sha256: digest(text) } })
  reply = { admitted: true, diagnostics: [], restatement: text, restatementSha256: "e".repeat(64) }
  expect(await store.admit("d", payload)).toEqual({ admitted: true })
  // A cell without the restater yet: no words, and the editor asks for them on save instead.
  reply = { admitted: false, diagnostics: [{ code: "definition.tile_title_invalid", message: "too long", span: { pointer: "/title" } }] }
  expect(await store.admit("d", payload)).toEqual({ admitted: false, findings: [{ code: "definition.tile_title_invalid", message: "too long", law: null, pointer: "/title" }] })
})
