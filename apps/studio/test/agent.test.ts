import { expect, test } from "bun:test"
import { AGENT_NODE, agentGraph, agentOf, emptyAgent, lawNodeId, toolNodeId, type AgentDefinition } from "../src/lib/canvas/agent.ts"
import * as g from "../src/lib/canvas/graph.ts"
import { agentKinds } from "../src/lib/canvas/agent.ts"
import { kindStore } from "../src/lib/canvas/store.ts"
import { scopeOf } from "../src/lib/scope.ts"

// A signed-in account as /api/me reports it; the server resolved its data environment.
const account = { user: { id: "u", name: "U", email: "u@example.com" }, organization: { id: "org_demo", name: "Demo", role: "admin" }, environmentId: "analytics" }
const SCOPE = scopeOf(account)!
import { pinOf } from "../src/agent-canvas.tsx"

const agent: AgentDefinition = {
  name: "support.triage", title: "Support triage", model: "claude-sonnet-5-5", prompt: "Triage new tickets.",
  tools: ["bind_zendesk_read", "bind_slack_post"], budgets: { maxRuns: 50, maxToolCalls: 400, maxCostUsd: "25.00" },
  laws: ["support.no_refund_over_limit", "support.public_reply_review"],
  decisionPolicy: { approve: "laws", laws: ["support.public_reply_review"], deciderRoles: ["admin", "lead"] },
}

test("an agent's graph compiles back to exactly its definition", () => {
  expect(agentOf(agentGraph(agent))).toEqual(agent)
  expect(agentOf(agentGraph(emptyAgent("fresh")))).toEqual(emptyAgent("fresh"))
})

test("only wired tools and laws count; a law cannot plug into the tools port", () => {
  const graph = agentGraph(agent)
  const cut = g.remove(graph, [], [`${toolNodeId("bind_slack_post")}->agent`])
  expect(agentOf(cut)?.tools).toEqual(["bind_zendesk_read"])
  const wrong = g.connect(cut, agentKinds, { id: "x", source: lawNodeId("support.no_refund_over_limit"), sourcePort: "out", target: AGENT_NODE, targetPort: "tools", config: {} })
  expect(wrong).toMatchObject({ code: "canvas.port.type_mismatch" })
})

test("a budget that is not a whole number stays invalid for the checker to name, never 0", () => {
  const graph = g.configureNode(agentGraph(agent), AGENT_NODE, { maxRuns: "ten" })
  expect(Number.isNaN(agentOf(graph)?.budgets.maxRuns)).toBe(true)
})

test("the agent store speaks the cell's definition routes and reads a refusal's law", async () => {
  const calls: { path: string; body: unknown }[] = []
  const store = kindStore<AgentDefinition>("agent", SCOPE, async (input, init) => {
    calls.push({ path: input, body: init?.body ? JSON.parse(String(init.body)) : undefined })
    return new Response(JSON.stringify({ code: "definition.refused", diagnostics: [{ code: "agent.validation.unknown_law", message: "no such law", law: "support.x", span: { pointer: "/laws/0" } }] }), { status: 422 })
  })
  const result = await store.install("studio.agent.support_triage", agent, 3)
  expect(calls[0]).toEqual({ path: "/api/host/v1/definitions:install?orgId=org_demo&environmentId=analytics", body: { kind: "agent", id: "studio.agent.support_triage", payload: agent, expectedVersion: 3 } })
  expect(result).toEqual({ installed: false, findings: [{ code: "agent.validation.unknown_law", message: "no such law", law: "support.x", pointer: "/laws/0" }] })
})

test("a binding pinned by the gateway becomes the agent check's binding entry; an unpinned one does not", () => {
  const base = { id: "bind_x", app: "chat", tool: "post", label: "Post", inputMapping: {}, runRole: "member" as const, updatedBy: "u", updatedAt: 0 }
  expect(pinOf({ ...base, manifestDigest: "sha256:ab", connector: { gateway: "executor", origin: "https://g", app: "chat", tool: "post", version: "v3" } } as never))
    .toEqual({ id: "bind_x", connector: "executor:chat/post@v3", manifestDigest: "sha256:ab" })
  expect(pinOf(base)).toBeNull()
})

test("the scope is the server's: /api/me's organization and environment, and none without a data cell", () => {
  expect(scopeOf(account)).toEqual({ orgId: "org_demo", environmentId: "analytics" })
  expect(scopeOf({ ...account, environmentId: null })).toBeNull()
  expect(scopeOf({ ...account, environmentId: undefined })).toBeNull()
  expect(scopeOf({ ...account, organization: null })).toBeNull()
  expect(scopeOf(null)).toBeNull()
})

test("every definitions call carries the account's scope", async () => {
  const paths: string[] = []
  const store = kindStore<AgentDefinition>("agent", SCOPE, async (input) => {
    paths.push(input)
    return new Response(JSON.stringify({ items: [], cursor: null, admitted: true, installed: true, id: "a", version: 1, kind: "agent", payload: agent, receipt: { receiptHash: "r", commitSeq: 1, recordedAtMs: 0 } }), { status: 200 })
  })
  await store.list("next")
  await store.get("studio.agent/one")
  await store.admit("a", agent)
  await store.install("a", agent)
  await store.retire("a", 1)
  expect(paths.length).toBe(5)
  for (const path of paths) {
    const query = new URL(path, "https://studio.test").searchParams
    expect([query.get("orgId"), query.get("environmentId")]).toEqual(["org_demo", "analytics"])
  }
  expect(new URL(paths[0]!, "https://studio.test").searchParams.get("cursor")).toBe("next")
  expect(new URL(paths[1]!, "https://studio.test").pathname).toBe("/api/host/v1/definitions/studio.agent%2Fone")
})
