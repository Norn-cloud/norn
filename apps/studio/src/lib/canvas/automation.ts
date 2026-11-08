/**
 * An automation drawn on the canvas is a Norn flow: it compiles to the
 * closure-free flow definition of `tools/norn-ts` and lowers through the
 * same `lowerFlow` the typed authoring toolchain uses, into a Run entity
 * (whose lifecycle is the flow's state machine) and its decide rule.
 * Nothing here runs a flow or decides anything; the checker and then the
 * server judge what it lowers to.
 */
import type { Expr } from "../../../../../tools/norn-ts/src/builder/expr.ts"
import { X, lowerFlow, type BuildDiagnostic, type EnvelopeOptions, type FlowDef, type FnRef, type Step } from "../../../../../tools/norn-ts/src/builder/index.ts"
import type { CanvasEdge, CanvasNode, Graph, Kinds, Port } from "./graph.ts"

/** Control flow: every step hands on to the next. What a step reads is typed by its tool's schemas, which lowering checks. */
const FLOW = "flow"
const input = { id: "in", type: FLOW, side: "in" } as const satisfies Port
const next = { id: "next", type: FLOW, side: "out" } as const satisfies Port

/** One condition of a route branch: `left` compared with `right`. A branch holds when all its conditions do. */
export interface Condition {
  readonly left: string
  readonly op: "==" | "!=" | "<" | "<=" | ">" | ">="
  readonly right: string
}

export const branchesOf = (node: CanvasNode): Condition[][] => JSON.parse(node.config.branches ?? "[]") as Condition[][]
export const inputsOf = (node: CanvasNode): Record<string, string> => JSON.parse(node.config.input ?? "{}") as Record<string, string>

export const automationKinds: Kinds = {
  start: () => [next],
  perform: () => [input, next],
  sleep: () => [input, next],
  succeed: () => [input],
  route: (node) => [input, ...branchesOf(node).map((_, index) => ({ id: `b${index}`, type: FLOW, side: "out" as const })), { id: "otherwise", type: FLOW, side: "out" }, next],
}

/** A finding the canvas itself makes before anything is lowered. */
export interface Problem extends BuildDiagnostic {
  readonly node?: string
}

const REF = /^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)+$/

/** `invoice.status` reads a bound result; anything else is a literal (JSON when it parses, else text). */
export function value(text: string): Expr {
  const trimmed = text.trim()
  if (REF.test(trimmed)) {
    const [name = "", ...path] = trimmed.split(".")
    return X.ref(name, ...path)
  }
  try {
    const parsed: unknown = JSON.parse(trimmed)
    if (parsed === null || ["string", "number", "boolean"].includes(typeof parsed)) return X.lit(parsed as string | number | boolean | null)
  } catch { /* not JSON: text */ }
  return X.lit(trimmed)
}

/** Where a lowering finding came from: steps carry their node's id as their location. */
const at = (node: CanvasNode) => ({ file: `canvas/${node.id}`, line: 1 })
export const nodeOfLocation = (file: string | undefined) => file?.startsWith("canvas/") ? file.slice("canvas/".length) : undefined

/** The flow definition the graph draws, or the canvas's own findings when it draws none. */
export function toFlow(graph: Graph, tools: readonly FnRef[], meta: { readonly id: string; readonly version: string }): { readonly flow?: FlowDef; readonly problems: readonly Problem[] } {
  const problems: Problem[] = []
  const byId = new Map(graph.nodes.map((node) => [node.id, node]))
  const starts = graph.nodes.filter(({ kind }) => kind === "start")
  if (starts.length !== 1) problems.push({ code: "canvas.flow.start", message: `An automation has exactly one start; this one has ${starts.length}` })
  for (const node of graph.nodes) {
    const incoming = graph.edges.filter(({ target }) => target === node.id)
    if (incoming.length > 1) problems.push({ code: "canvas.flow.join", message: "Two paths join here; end each branch, or continue after the route", node: node.id })
  }
  const reached = new Set<string>()
  const out = (from: string, port: string): CanvasNode | undefined => {
    const edge = graph.edges.find(({ source, sourcePort }) => source === from && sourcePort === port)
    return edge && byId.get(edge.target)
  }
  const sequence = (from: string, port: string): Step[] => {
    const steps: Step[] = []
    for (let node = out(from, port); node; node = out(node.id, "next")) {
      if (reached.has(node.id)) { problems.push({ code: "canvas.flow.cycle", message: "This step leads back to itself; a flow only moves forward", node: node.id }); break }
      reached.add(node.id)
      const step = stepOf(node)
      if (step) steps.push(step)
      if (node.kind === "succeed") break
    }
    return steps
  }
  const stepOf = (node: CanvasNode): Step | undefined => {
    const sourceMap = { id: node.id, kind: node.kind === "route" ? "switch" as const : node.kind as "perform" | "sleep" | "succeed" }
    switch (node.kind) {
      case "perform": {
        const tool = tools.find(({ id }) => id === node.config.tool)
        if (!tool) { problems.push({ code: "canvas.tool.unknown", message: `No tool ${node.config.tool ?? ""} is connected`, node: node.id }); return undefined }
        const fields = Object.entries(inputsOf(node)).map(([field, text]) => [field, value(text)] as const)
        const bind = node.config.bind?.trim()
        return { _tag: "Perform", step: node.id, fn: tool, input: X.record(Object.fromEntries(fields)), ...(bind ? { bind } : {}), at: at(node), sourceMap }
      }
      case "sleep": return { _tag: "Sleep", step: node.id, millis: Number(node.config.millis ?? "0"), at: at(node), sourceMap }
      case "succeed": return { _tag: "Succeed", value: X.record({ outcome: X.lit(node.config.outcome ?? "") }), at: at(node), sourceMap }
      case "route": {
        const conditions = branchesOf(node)
        const compare = ({ op, left, right }: Condition): Expr => ({ _tag: "Compare", op, left: value(left), right: value(right) })
        const branches = conditions.map((all, index) => ({ when: all.length > 0 ? all.map(compare).reduce((both, one) => X.and(both, one)) : X.lit(true), then: sequence(node.id, `b${index}`) }))
        const route: Step = { _tag: "Route", branches, otherwise: sequence(node.id, "otherwise"), at: at(node), sourceMap: { ...sourceMap, branchLabels: conditions.map((_, index) => `b${index}`), otherwiseLabel: "otherwise" } }
        return route
      }
      default: return undefined
    }
  }
  const [start] = starts
  const body = start ? sequence(start.id, "next") : []
  for (const node of graph.nodes) if (node.kind !== "start" && !reached.has(node.id)) problems.push({ code: "canvas.flow.unreachable", message: "Nothing leads to this step, so it never runs", node: node.id })
  return problems.length > 0 ? { problems } : { flow: { _tag: "Flow", meta, body }, problems }
}

/** The canvas layout rides with the rule as an extension, so the drawing reopens exactly as it was left. */
export const CANVAS_EXTENSION = "norn.cloud/canvas/1.0.0"

type Artifact = { readonly kind: string; readonly payload: Record<string, unknown> } & Record<string, unknown>

export interface Lowered {
  readonly artifacts: readonly Artifact[]
  /** Each canvas step's generated states, by node id. */
  readonly states: ReadonlyMap<string, readonly string[]>
}

/** Lower the drawn flow to its artifacts; lowering findings point back at the node they came from. */
export function lower(graph: Graph, tools: readonly FnRef[], meta: { readonly id: string; readonly version: string }, options: EnvelopeOptions = { scope: {}, sourcePath: "apps/studio/canvas" }): { readonly lowered?: Lowered; readonly problems: readonly Problem[] } {
  const drawn = toFlow(graph, tools, meta)
  if (!drawn.flow) return { problems: drawn.problems }
  const built = lowerFlow(drawn.flow, options)
  if (!built.ok) return { problems: built.diagnostics.map((finding) => ({ ...finding, ...(nodeOfLocation(finding.at?.file) ? { node: nodeOfLocation(finding.at?.file) } : {}) })) }
  const artifacts = (built.artifacts as unknown as Artifact[]).map((artifact) => artifact.kind === "rule"
    ? { ...artifact, payload: { ...artifact.payload, extensions: { [CANVAS_EXTENSION]: { nodes: graph.nodes, edges: graph.edges } } } }
    : artifact)
  const states = new Map((built.flowSourceMap?.flows ?? []).flatMap(({ steps }) => steps.map(({ id, stateIds }) => [id, stateIds] as const)))
  return { lowered: { artifacts, states }, problems: [] }
}

/**
 * The drawing a lowered rule carries, only when it still lowers to exactly
 * these artifacts; a rule changed outside the canvas is not redrawn as if it
 * were the canvas's.
 */
export function redraw(artifacts: readonly Artifact[], tools: readonly FnRef[], meta: { readonly id: string; readonly version: string }, options?: EnvelopeOptions): Graph | undefined {
  const rule = artifacts.find(({ kind }) => kind === "rule")
  const saved = (rule?.payload.extensions as Record<string, { nodes: CanvasNode[]; edges: CanvasEdge[] }> | undefined)?.[CANVAS_EXTENSION]
  if (!saved) return undefined
  const graph: Graph = { nodes: saved.nodes, edges: saved.edges }
  const again = lower(graph, tools, meta, options).lowered?.artifacts
  return again && JSON.stringify(again) === JSON.stringify(artifacts) ? graph : undefined
}
