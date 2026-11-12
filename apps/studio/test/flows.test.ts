import { afterEach, expect, test } from "bun:test"
import { readRunTypeTitles, runTypeTitles } from "../src/lib/flows.ts"

const original = globalThis.fetch
afterEach(() => { globalThis.fetch = original })

const ME = { user: { id: "u", name: "U", email: "" }, organization: { id: "org", name: "Org", role: "admin" }, environmentId: "dev" }

const PAYLOAD = {
  flow: { id: "ops.approve_refund", version: "1.0.0", title: "Approve a refund" },
  artifacts: [
    { kind: "entity", payload: { objectType: { name: "ops.ApproveRefundRun", description: "Run of flow ops.approve_refund@1.0.0" } } },
    { kind: "rule", payload: { ruleId: "ops.approve_refund.ask_review" } },
  ],
}

test("a flow's run type is named by the title the flow was installed under", () => {
  expect([...runTypeTitles([PAYLOAD])]).toEqual([["ops.ApproveRefundRun", "Approve a refund"]])
})

test("a flow without a title, or without an entity, names nothing", () => {
  expect(runTypeTitles([{ ...PAYLOAD, flow: { title: null } }]).size).toBe(0)
  expect(runTypeTitles([{ flow: { title: "Approve a refund" }, artifacts: [] }]).size).toBe(0)
})

test("the titles come from each listed flow's installed definition", async () => {
  const urls: string[] = []
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(ME)
    urls.push(url)
    if (url.includes("/v1/definitions:list")) return Response.json({ items: [{ id: "ops.approve_refund", kind: "flow", title: "Approve a refund", version: 1, lifecycle: "active", updatedAtMs: 0 }], cursor: null })
    return Response.json({ id: "ops.approve_refund", kind: "flow", version: 1, payload: PAYLOAD })
  }) as typeof fetch
  expect((await readRunTypeTitles()).get("ops.ApproveRefundRun")).toBe("Approve a refund")
  expect(urls.some((url) => url.includes("/v1/definitions/ops.approve_refund?"))).toBe(true)
})
