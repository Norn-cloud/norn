import { expect, test } from "bun:test"
import { questionDefinitions, QuestionDefinitionError } from "../src/lib/question-definitions.ts"

test("question definitions use the analytics scope and send the signed version", async () => {
  const calls: { url: string; body?: unknown }[] = []
  const store = questionDefinitions(async (input, init) => {
    const url = String(input)
    calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : undefined })
    if (url.includes(":list")) return Response.json({ items: [{ id: "studio.yield", title: "Yield", version: 2, lifecycle: "active" }], cursor: null })
    if (url.includes(":install")) return Response.json({ installed: true, id: "studio.yield", version: 3 })
    return Response.json({ id: "studio.yield", kind: "question", version: 2, payload: { name: "studio.yield" } })
  })
  expect((await store.list()).items[0]?.version).toBe(2)
  expect((await store.get("studio.yield")).payload).toEqual({ name: "studio.yield" })
  expect((await store.install("studio.yield", { name: "studio.yield" }, "abc", 2)).version).toBe(3)
  expect(calls.every(({ url }) => url.includes("orgId=org_demo") && url.includes("environmentId=analytics"))).toBe(true)
  expect(calls[2]?.body).toEqual({ kind: "question", id: "studio.yield", payload: { name: "studio.yield" }, expectedVersion: 2, signoff: { restatementSha256: "abc" } })
})

test("a sign-off refusal remains typed and never reports a save", async () => {
  for (const code of ["definition.signoff_unavailable", "definition.signoff_mismatch"]) {
    const store = questionDefinitions(async () => Response.json({ code, message: "Refused", detail: "Not saved" }, { status: 422 }))
    try { await store.install("studio.yield", {}, "abc"); throw new Error("unexpected success") }
    catch (error) { expect(error).toBeInstanceOf(QuestionDefinitionError); expect((error as QuestionDefinitionError).code).toBe(code) }
  }
})


test("mismatch returns only words whose digest matches the server hash", async () => {
  const text = "The cell's words."
  const digest = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))), (b) => b.toString(16).padStart(2, "0")).join("")
  for (const [sha, expected] of [[digest, { text, sha256: digest }], ["0".repeat(64), undefined]] as const) {
    const store = questionDefinitions(async () => Response.json({ code: "definition.signoff_mismatch", restatement: text, restatementSha256: sha }, { status: 422 }))
    try { await store.install("studio.yield", {}, "abc"); throw new Error("unexpected success") }
    catch (error) { expect(error).toBeInstanceOf(QuestionDefinitionError); expect((error as QuestionDefinitionError).restatement).toEqual(expected) }
  }
})
