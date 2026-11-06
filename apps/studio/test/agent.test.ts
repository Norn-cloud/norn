import { expect, test } from "bun:test"
import { AGENT_NODE, agentGraph, agentOf, emptyAgent, lawNodeId, toolNodeId, type AgentDefinition } from "../src/lib/canvas/agent.ts"
import * as g from "../src/lib/canvas/graph.ts"
import { agentKinds } from "../src/lib/canvas/agent.ts"
import { kindStore } from "../src/lib/canvas/store.ts"
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
  const store = kindStore<AgentDefinition>("agent", async (input, init) => {
    calls.push({ path: input, body: init?.body ? JSON.parse(String(init.body)) : undefined })
    return new Response(JSON.stringify({ code: "definition.refused", diagnostics: [{ code: "agent.validation.unknown_law", message: "no such law", law: "support.x", span: { pointer: "/laws/0" } }] }), { status: 422 })
  })
  const result = await store.install("studio.agent.support_triage", agent, 3)
  expect(calls[0]).toEqual({ path: "/api/host/v1/definitions:install", body: { kind: "agent", id: "studio.agent.support_triage", payload: agent, expectedVersion: 3 } })
  expect(result).toEqual({ installed: false, findings: [{ code: "agent.validation.unknown_law", message: "no such law", law: "support.x", pointer: "/laws/0" }] })
})

test("a binding pinned by the gateway becomes the agent check's binding entry; an unpinned one does not", () => {
  const base = { id: "bind_x", app: "chat", tool: "post", label: "Post", inputMapping: {}, runRole: "member" as const, updatedBy: "u", updatedAt: 0 }
  expect(pinOf({ ...base, manifestDigest: "sha256:ab", connector: { gateway: "executor", origin: "https://g", app: "chat", tool: "post", version: "v3" } } as never))
    .toEqual({ id: "bind_x", connector: "executor:chat/post@v3", manifestDigest: "sha256:ab" })
  expect(pinOf(base)).toBeNull()
})
