/**
 * An agent is a definition (D58): a model, a prompt, the tool bindings it may
 * call, its budgets, the laws it works under and when a person decides. On
 * the canvas it is one agent node with tool bindings and laws wired into it;
 * the wiring compiles to exactly the definition's payload and back. The
 * checker (in the browser for feedback, in the cell on admit and install)
 * decides whether it is admissible.
 */
import type { CanvasEdge, CanvasNode, Graph, Kinds } from "./graph.ts"

export interface AgentDefinition {
  readonly name: string
  readonly title: string
  readonly model: string
  readonly prompt: string
  readonly tools: readonly string[]
  readonly budgets: { readonly maxRuns: number; readonly maxToolCalls: number; readonly maxCostUsd: string }
  readonly laws: readonly string[]
  readonly decisionPolicy: { readonly approve: "always" | "never" | "laws"; readonly laws?: readonly string[]; readonly deciderRoles: readonly string[] }
}

/**
 * The models Studio offers until the cell serves its declared allowlist.
 * Offering is not admitting: the cell refuses a model outside its own list.
 */
export const OFFERED_MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] as const

/** A tool binding plugs only into an agent's tools, a law only into its laws. */
export const agentKinds: Kinds = {
  agent: () => [{ id: "tools", type: "tool-binding", side: "in" }, { id: "laws", type: "law", side: "in" }],
  tool: () => [{ id: "out", type: "tool-binding", side: "out" }],
  law: () => [{ id: "out", type: "law", side: "out" }],
}

export const AGENT_NODE = "agent"
const toolNode = (binding: string) => `tool:${binding}`
const lawNode = (law: string) => `law:${law}`

export function emptyAgent(name: string): AgentDefinition {
  return { name, title: "", model: OFFERED_MODELS[1], prompt: "", tools: [], budgets: { maxRuns: 100, maxToolCalls: 1000, maxCostUsd: "10.00" }, laws: [], decisionPolicy: { approve: "always", deciderRoles: ["admin"] } }
}

/** The agent's graph: the agent in the middle, its tools to the left, its laws to the right. */
export function agentGraph(agent: AgentDefinition): Graph {
  const policyLaws = new Set(agent.decisionPolicy.laws ?? [])
  const nodes: CanvasNode[] = [
    { id: AGENT_NODE, kind: "agent", position: { x: 360, y: Math.max(agent.tools.length, agent.laws.length) * 45 }, config: {
      name: agent.name, title: agent.title, model: agent.model, prompt: agent.prompt,
      maxRuns: String(agent.budgets.maxRuns), maxToolCalls: String(agent.budgets.maxToolCalls), maxCostUsd: agent.budgets.maxCostUsd,
      approve: agent.decisionPolicy.approve, deciderRoles: agent.decisionPolicy.deciderRoles.join(","),
    } },
    ...agent.tools.map((binding, row): CanvasNode => ({ id: toolNode(binding), kind: "tool", position: { x: 0, y: row * 90 }, config: { binding } })),
    ...agent.laws.map((law, row): CanvasNode => ({ id: lawNode(law), kind: "law", position: { x: 720, y: row * 90 }, config: { law, decides: policyLaws.has(law) ? "yes" : "" } })),
  ]
  const edges: CanvasEdge[] = [
    ...agent.tools.map((binding): CanvasEdge => ({ id: `${toolNode(binding)}->agent`, source: toolNode(binding), sourcePort: "out", target: AGENT_NODE, targetPort: "tools", config: {} })),
    ...agent.laws.map((law): CanvasEdge => ({ id: `${lawNode(law)}->agent`, source: lawNode(law), sourcePort: "out", target: AGENT_NODE, targetPort: "laws", config: {} })),
  ]
  return { nodes, edges }
}

const wholeNumber = (text: string | undefined) => /^\d+$/.test(text ?? "") ? Number(text) : Number.NaN

/**
 * The definition the graph draws: only tools and laws wired into the agent
 * count, in the order they sit on the canvas (top to bottom). Numbers that are
 * not whole stay invalid (NaN) so the checker names them, never silently 0.
 */
/** The tool and law nodes wired into the agent, in payload order (top to bottom): a finding's `/tools/i` or `/laws/i` is the i-th. */
export function wiredParts(graph: Graph): { readonly tools: readonly CanvasNode[]; readonly laws: readonly CanvasNode[] } {
  const agent = graph.nodes.find(({ kind }) => kind === "agent")
  const wired = (kind: "tool" | "law") => graph.nodes
    .filter((node) => node.kind === kind && graph.edges.some(({ source, target }) => source === node.id && target === agent?.id))
    .sort((a, b) => a.position.y - b.position.y || a.position.x - b.position.x)
  return { tools: wired("tool"), laws: wired("law") }
}

export function agentOf(graph: Graph): AgentDefinition | undefined {
  const agent = graph.nodes.find(({ kind }) => kind === "agent")
  if (!agent) return undefined
  const parts = wiredParts(graph)
  const wired = (kind: "tool" | "law") => kind === "tool" ? parts.tools : parts.laws
  const laws = wired("law")
  const approve = (["always", "never", "laws"] as const).find((value) => value === agent.config.approve) ?? "always"
  const decides = laws.filter(({ config }) => config.decides === "yes").map(({ config }) => config.law ?? "")
  return {
    name: agent.config.name ?? "", title: agent.config.title ?? "", model: agent.config.model ?? "", prompt: agent.config.prompt ?? "",
    tools: wired("tool").map(({ config }) => config.binding ?? ""),
    budgets: { maxRuns: wholeNumber(agent.config.maxRuns), maxToolCalls: wholeNumber(agent.config.maxToolCalls), maxCostUsd: agent.config.maxCostUsd ?? "" },
    laws: laws.map(({ config }) => config.law ?? ""),
    decisionPolicy: { approve, ...approve === "laws" ? { laws: decides } : {}, deciderRoles: (agent.config.deciderRoles ?? "").split(",").map((role) => role.trim()).filter(Boolean) },
  }
}

export const toolNodeId = toolNode
export const lawNodeId = lawNode
