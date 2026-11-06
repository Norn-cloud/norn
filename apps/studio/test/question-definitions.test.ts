import { expect, test } from "bun:test"
import { questionDefinitions, QuestionDefinitionError } from "../src/lib/question-definitions.ts"

const testQuestions = (fetcher: Parameters<typeof questionDefinitions>[0]) => questionDefinitions(fetcher, async () => ({ orgId: "org_demo", environmentId: "analytics" }))

test("question definitions use the analytics scope and send the signed version", async () => {
  const calls: { url: string; body?: unknown }[] = []
  const text = "Cell words"
  const hash = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))), (b) => b.toString(16).padStart(2, "0")).join("")
  const store = testQuestions(async (input, init) => {
    const url = String(input)
    calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : undefined })
    if (url.includes(":list")) return Response.json({ items: [{ id: "studio.yield", title: "Yield", version: 2, lifecycle: "active" }], cursor: null })
    if (url.includes(":install") || url.includes(":retire")) return Response.json({ installed: true, id: "studio.yield", version: 3 })
    return Response.json({ id: "studio.yield", kind: "question", version: 2, payload: { name: "studio.yield" }, restatement: text, restatementSha256: hash, signoff: { signer: "analyst@demo.norn.cloud", signedAtMs: 1794000000000, restatementSha256: hash } })
  })
  expect((await store.list()).items[0]?.version).toBe(2)
  const reopened = await store.get("studio.yield")
  expect(reopened.payload).toEqual({ name: "studio.yield" })
  expect(reopened.signoff).toEqual({ signer: "analyst@demo.norn.cloud", signedAtMs: 1794000000000, restatementSha256: hash })
  expect((await store.install("studio.yield", { name: "studio.yield" }, "abc", 2)).version).toBe(3)
  expect((await store.retire("studio.yield", 2)).version).toBe(3)
  expect(calls.every(({ url }) => url.includes("orgId=org_demo") && url.includes("environmentId=analytics"))).toBe(true)
  expect(calls[2]?.body).toEqual({ kind: "question", id: "studio.yield", payload: { name: "studio.yield" }, expectedVersion: 2, signoff: { restatementSha256: "abc" } })
  expect(calls[3]?.url).toContain("/studio.yield:retire?")
})

test("a sign-off refusal remains typed and never reports a save", async () => {
  for (const code of ["definition.signoff_unavailable", "definition.signoff_mismatch"]) {
    const store = testQuestions(async () => Response.json({ code, message: "Refused", detail: "Not saved" }, { status: 422 }))
    try { await store.install("studio.yield", {}, "abc"); throw new Error("unexpected success") }
    catch (error) { expect(error).toBeInstanceOf(QuestionDefinitionError); expect((error as QuestionDefinitionError).code).toBe(code) }
  }
})


test("mismatch returns only words whose digest matches the server hash", async () => {
  const text = "The cell's words."
  const digest = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))), (b) => b.toString(16).padStart(2, "0")).join("")
  for (const [sha, expected] of [[digest, { text, sha256: digest }], ["0".repeat(64), undefined]] as const) {
    const store = testQuestions(async () => Response.json({ code: "definition.signoff_mismatch", restatement: text, restatementSha256: sha }, { status: 422 }))
    try { await store.install("studio.yield", {}, "abc"); throw new Error("unexpected success") }
    catch (error) { expect(error).toBeInstanceOf(QuestionDefinitionError); expect((error as QuestionDefinitionError).restatement).toEqual(expected) }
  }
})

test("admission exposes only cell words whose digest matches", async () => {
  const text = "The cell's current words."
  const hash = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))), (b) => b.toString(16).padStart(2, "0")).join("")
  const valid = testQuestions(async () => Response.json({ admitted: true, diagnostics: [], restatement: text, restatementSha256: hash }))
  expect(await valid.admit("studio.yield", { name: "studio.yield" })).toEqual({ text, sha256: hash })
  const invalid = testQuestions(async () => Response.json({ admitted: true, diagnostics: [], restatement: text, restatementSha256: "0".repeat(64) }))
  await expect(invalid.admit("studio.yield", {})).rejects.toMatchObject({ code: "definition.shape" })
  const missing = testQuestions(async () => Response.json({ admitted: true, diagnostics: [] }))
  await expect(missing.admit("studio.yield", {})).rejects.toMatchObject({ code: "definition.signoff_unavailable" })
})
