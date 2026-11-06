/**
 * "Ask a person" (D58 §2): a decision is a step of a flow, not a new kind of
 * IR. It lowers to a `perform` of the host Function `norn.decision` (the run
 * waits in `awaiting_perform_ask_<step>`) and a `route` on the person's
 * choice. Only the cell resolves that perform, with a receipt it issues for
 * the deciding human; a client or an agent can only recommend.
 */
import { S, X, fn, taggedError, type FnRef, type Step } from "../../../../../tools/norn-ts/src/builder/index.ts"
import type { CanvasEdge, CanvasNode, Graph, Port } from "./graph.ts"

/** One option a person may choose: what it does, and how far it reaches. */
export interface DecisionOption {
  /** Stable within its step: `o1`, `o2`, …; ports and the chosen value use it. */
  readonly id: string
  readonly label: string
  /** What choosing it starts, in words the decider reads ("Public · updates every 15 min"). */
  readonly scope: string
}

export const RISKS = ["low", "medium", "high"] as const
export type Risk = (typeof RISKS)[number]

const OPTION = S.record({ id: S.text(), label: S.text(), scope: S.text() })

/** The decision Function contract (`norn.decision` v1): question and options in, the person's choice out. */
export const DECISION_FN: FnRef = fn({
  id: "norn.decision",
  version: "1.0.0",
  input: S.record({ question: S.text(), risk: S.text([...RISKS]), options: S.array(OPTION), agentPick: S.text(), deciderRoles: S.array(S.text()) }),
  output: S.record({ choice: S.text() }),
  errors: [taggedError("Expired", S.record({})), taggedError("Withdrawn", S.record({}))],
  at: { file: "apps/studio/src/lib/canvas/decision.ts", line: 1 },
})

export const optionsOf = (node: CanvasNode): DecisionOption[] => JSON.parse(node.config.options ?? "[]") as DecisionOption[]
export const rolesOf = (node: CanvasNode): string[] => (node.config.deciderRoles ?? "").split(",").map((role) => role.trim()).filter(Boolean)
export const optionPort = (option: Pick<DecisionOption, "id">) => `opt:${option.id}`

/** An ask step: one exit per option, then next (continue after the decision). */
export const decisionPorts = (node: CanvasNode): Port[] => [
  { id: "in", type: "flow", side: "in" },
  ...optionsOf(node).map((option) => ({ id: optionPort(option), type: "flow", side: "out" as const })),
  { id: "next", type: "flow", side: "out" },
]

/** The perform that waits for the person and the route on what they chose. */
export function decisionSteps(node: CanvasNode, sequence: (port: string) => Step[], at: { file: string; line: number }): Step[] {
  const options = optionsOf(node)
  const pick = node.config.agentPick ?? ""
  const input = X.record({
    question: X.lit(node.config.question ?? ""),
    risk: X.lit(node.config.risk ?? "medium"),
    options: X.list(...options.map((option) => X.record({ id: X.lit(option.id), label: X.lit(option.label), scope: X.lit(option.scope) }))),
    agentPick: X.lit(options.some(({ id }) => id === pick) ? pick : ""),
    deciderRoles: X.list(...rolesOf(node).map((role) => X.lit(role))),
  })
  const ask: Step = { _tag: "Perform", step: `ask_${node.id}`, fn: DECISION_FN, input, bind: node.id, at, sourceMap: { id: node.id, kind: "perform" } }
  const route: Step = {
    _tag: "Route",
    branches: options.map((option) => ({ when: X.eq(X.ref(node.id, "choice"), X.lit(option.id)), then: sequence(optionPort(option)) })),
    // The cell accepts only a listed choice, so nothing falls through here.
    otherwise: [],
    at,
    sourceMap: { id: `${node.id}_route`, kind: "switch", branchLabels: options.map(optionPort), otherwiseLabel: "otherwise" },
  }
  return [ask, route]
}

/** Set an ask step's options; an exit follows its option by id, and a removed option's exit goes with it. */
export function setOptions(graph: Graph, id: string, options: readonly DecisionOption[]): Graph {
  const kept = new Set(options.map(optionPort))
  return {
    nodes: graph.nodes.map((node) => node.id === id ? { ...node, config: { ...node.config, options: JSON.stringify(options), agentPick: options.some(({ id: option }) => option === node.config.agentPick) ? node.config.agentPick ?? "" : "" } } : node),
    edges: graph.edges.filter((edge) => edge.source !== id || !edge.sourcePort.startsWith("opt:") || kept.has(edge.sourcePort)),
  }
}

/** A fresh option id for a step: `o1`, `o2`, …, never reusing a live one. */
export function nextOptionId(options: readonly DecisionOption[]): string {
  const used = new Set(options.map(({ id }) => id))
  for (let n = 1; ; n += 1) if (!used.has(`o${n}`)) return `o${n}`
}

/** What a decider sees: the question, each option with what it starts, and the decisions that follow it. */
export interface DecisionView {
  readonly id: string
  readonly question: string
  readonly risk: Risk
  readonly deciderRoles: readonly string[]
  readonly options: readonly {
    readonly option: DecisionOption
    readonly agentPick: boolean
    /** The tools the option leads to before the next decision, in order. */
    readonly tools: readonly string[]
    readonly followUp: DecisionView | null
  }[]
}

/** The decision tree an ask step draws, from the graph: each option's path up to the next ask step. */
export function decisionView(graph: Graph, id: string, seen: ReadonlySet<string> = new Set()): DecisionView | null {
  const node = graph.nodes.find((one) => one.id === id && one.kind === "decide")
  if (!node || seen.has(id)) return null
  const visited = new Set([...seen, id])
  const after = (source: string, port: string): CanvasNode | undefined => {
    const edge: CanvasEdge | undefined = graph.edges.find((one) => one.source === source && one.sourcePort === port)
    return edge && graph.nodes.find((one) => one.id === edge.target)
  }
  return {
    id,
    question: node.config.question ?? "",
    risk: (RISKS as readonly string[]).includes(node.config.risk ?? "") ? node.config.risk as Risk : "medium",
    deciderRoles: rolesOf(node),
    options: optionsOf(node).map((option) => {
      const tools: string[] = []
      let followUp: DecisionView | null = null
      for (let step = after(id, optionPort(option)), guard = 0; step && guard < 64; step = after(step.id, "next"), guard += 1) {
        if (step.kind === "decide") { followUp = decisionView(graph, step.id, visited); break }
        if (step.kind === "perform") tools.push(step.config.tool ?? "")
        if (step.kind === "succeed" || step.kind === "route") break
      }
      return { option, agentPick: node.config.agentPick === option.id, tools, followUp }
    }),
  }
}
