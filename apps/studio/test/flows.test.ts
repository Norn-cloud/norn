import { afterEach, expect, test } from "bun:test"
import { flowName, readRunTypeFlows, runTypeFlows } from "../src/lib/flows.ts"

const original = globalThis.fetch
afterEach(() => { globalThis.fetch = original })

const ME = { user: { id: "u", name: "U", email: "" }, organization: { id: "org", name: "Org", role: "admin" }, environmentId: "dev" }

const PAYLOAD = {
  flow: { id: "ops.approve_refund", version: "1.0.0", title: "Approve a refund", steps: [{ id: "ask_review", label: "Review the refund" }] },
  artifacts: [
    { kind: "entity", payload: { objectType: { name: "ops.ApproveRefundRun", description: "Run of flow ops.approve_refund@1.0.0" } } },
    { kind: "rule", payload: { ruleId: "ops.approve_refund.ask_review" } },
  ],
}

test("a flow's run type is named by the title the flow was installed under", () => {
  expect([...runTypeFlows([PAYLOAD])]).toEqual([["ops.ApproveRefundRun", { title: "Approve a refund", steps: new Map([["ask_review", "Review the refund"]]) }]])
})

test("a flow without a title, or without an entity, names nothing", () => {
  expect(runTypeFlows([{ ...PAYLOAD, flow: { title: null } }]).size).toBe(0)
  expect(runTypeFlows([{ flow: { title: "Approve a refund" }, artifacts: [] }]).size).toBe(0)
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
  expect((await readRunTypeFlows()).get("ops.ApproveRefundRun")?.title).toBe("Approve a refund")
  expect(urls.some((url) => url.includes("/v1/definitions/ops.approve_refund?"))).toBe(true)
})

test("a generated run's states and steps read in the flow's own words; a name outside the builder's grammar is left to the caller", () => {
  const flow = { title: "Approve a refund", steps: new Map([["ask_review", "Review the refund"], ["notify", "Tell the customer"]]) }
  // The lowered fixture's own lifecycle (tools/norn-ts/fixtures/flows/ask.ir.json).
  expect(flowName(flow, "started")).toBe("Started")
  expect(flowName(flow, "to_awaiting_perform_ask_review")).toBe("Review the refund")
  expect(flowName(flow, "awaiting_perform_ask_review")).toBe("Waiting: Review the refund")
  expect(flowName(flow, "to_completed")).toBe("Completed")
  expect(flowName(flow, "failed_withdrawn")).toBe("Withdrawn")
  expect(flowName(flow, "to_failed_expired")).toBe("Expired")
  // Retries and sleeps, as lower-flow.ts names them.
  expect(flowName(flow, "awaiting_retry_notify_attempt_2")).toBe("Waiting to retry: Tell the customer (attempt 2)")
  expect(flowName(flow, "retrying_notify_attempt_2")).toBe("Retrying: Tell the customer (attempt 2)")
  expect(flowName(flow, "sleeping_notify")).toBe("Paused: Tell the customer")
  // A step the flow does not label reads as its id in words; anything else is not decoded.
  expect(flowName(flow, "awaiting_perform_ship_it")).toBe("Waiting: Ship it")
  expect(flowName(flow, "approved")).toBeUndefined()
  expect(flowName(flow, "to_approved")).toBeUndefined()
})
