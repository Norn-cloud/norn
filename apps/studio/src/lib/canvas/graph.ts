/**
 * The canvas's own model: nodes with typed ports, edges between ports, and an
 * undo history. A graph is only an editing surface; what it means comes from
 * the definition it compiles to, which the checker and then the server judge.
 */

/** A port's type, derived from the IR it stands for (`state:commerce.SalesOrder.status`). */
export type PortType = string

export interface Port {
  readonly id: string
  readonly type: PortType
  readonly side: "in" | "out"
}

export interface CanvasNode {
  readonly id: string
  readonly kind: string
  readonly position: { readonly x: number; readonly y: number }
  readonly config: Readonly<Record<string, string>>
}

export interface CanvasEdge {
  readonly id: string
  readonly source: string
  readonly sourcePort: string
  readonly target: string
  readonly targetPort: string
  readonly config: Readonly<Record<string, string>>
}

export interface Graph {
  readonly nodes: readonly CanvasNode[]
  readonly edges: readonly CanvasEdge[]
}

/** The ports a node of each kind offers. */
export type Kinds = Readonly<Record<string, (node: CanvasNode) => readonly Port[]>>

/** Why the canvas would not draw an edge. */
export interface Refusal {
  readonly code: "canvas.port.unknown" | "canvas.port.type_mismatch" | "canvas.edge.duplicate"
  readonly message: string
}

export const portOf = (kinds: Kinds, node: CanvasNode | undefined, id: string, side: Port["side"]): Port | undefined =>
  node && kinds[node.kind]?.(node).find((port) => port.id === id && port.side === side)

/** Draw an edge when both ports exist and carry the same type; otherwise say why not. */
export function connect(graph: Graph, kinds: Kinds, edge: CanvasEdge): Graph | Refusal {
  const find = (id: string) => graph.nodes.find((node) => node.id === id)
  const from = portOf(kinds, find(edge.source), edge.sourcePort, "out")
  const to = portOf(kinds, find(edge.target), edge.targetPort, "in")
  if (!from || !to) return { code: "canvas.port.unknown", message: `No ${from ? "input" : "output"} port ${from ? edge.targetPort : edge.sourcePort} on ${from ? edge.target : edge.source}` }
  if (from.type !== to.type) return { code: "canvas.port.type_mismatch", message: `${edge.source} gives ${from.type}, but ${edge.target} takes ${to.type}` }
  if (graph.edges.some(({ id }) => id === edge.id)) return { code: "canvas.edge.duplicate", message: `${edge.id} is already drawn` }
  return { ...graph, edges: [...graph.edges, edge] }
}

export const isRefusal = (value: Graph | Refusal): value is Refusal => "code" in value

export const addNode = (graph: Graph, node: CanvasNode): Graph => ({ ...graph, nodes: [...graph.nodes, node] })

/** Remove nodes and edges; an edge touching a removed node goes with it. */
export function remove(graph: Graph, nodes: readonly string[], edges: readonly string[] = []): Graph {
  const gone = new Set(nodes)
  return {
    nodes: graph.nodes.filter(({ id }) => !gone.has(id)),
    edges: graph.edges.filter(({ id, source, target }) => !edges.includes(id) && !gone.has(source) && !gone.has(target)),
  }
}

const changes = (current: Readonly<Record<string, string>>, config: Record<string, string>) =>
  Object.entries(config).some(([key, value]) => current[key] !== value)

/** Configure a node; an unchanged value returns the same graph, so no empty undo step is recorded. */
export const configureNode = (graph: Graph, id: string, config: Record<string, string>): Graph =>
  graph.nodes.some((node) => node.id === id && changes(node.config, config))
    ? { ...graph, nodes: graph.nodes.map((node) => node.id === id ? { ...node, config: { ...node.config, ...config } } : node) }
    : graph

/** Configure edges; an unchanged value returns the same graph. */
export const configureEdges = (graph: Graph, ids: readonly string[], config: Record<string, string>): Graph =>
  graph.edges.some((edge) => ids.includes(edge.id) && changes(edge.config, config))
    ? { ...graph, edges: graph.edges.map((edge) => ids.includes(edge.id) ? { ...edge, config: { ...edge.config, ...config } } : edge) }
    : graph

export const moveNode = (graph: Graph, id: string, position: CanvasNode["position"]): Graph =>
  ({ ...graph, nodes: graph.nodes.map((node) => node.id === id ? { ...node, position } : node) })

/** Give a node a new id, carrying its edges along. */
export function renameNode(graph: Graph, from: string, to: string): Graph {
  if (from === to || graph.nodes.some(({ id }) => id === to)) return graph
  const swap = (id: string) => id === from ? to : id
  return {
    nodes: graph.nodes.map((node) => node.id === from ? { ...node, id: to } : node),
    edges: graph.edges.map((edge) => ({ ...edge, source: swap(edge.source), target: swap(edge.target) })),
  }
}

/** `stem`, or `stem_2`, `stem_3`…: the first not in `used`. */
export function freshName(used: Iterable<string>, stem: string): string {
  const taken = new Set(used)
  for (let n = 1; ; n += 1) if (!taken.has(n === 1 ? stem : `${stem}_${n}`)) return n === 1 ? stem : `${stem}_${n}`
}

export const freshId = (graph: Graph, stem: string): string => freshName(graph.nodes.map(({ id }) => id), stem)

/** Every edit is one step back; a drag is one step however many moves it makes. */
export interface History {
  readonly past: readonly Graph[]
  readonly present: Graph
  readonly future: readonly Graph[]
}

export const history = (present: Graph): History => ({ past: [], present, future: [] })

export const commit = (state: History, next: Graph): History =>
  next === state.present ? state : { past: [...state.past, state.present], present: next, future: [] }

/** Change the present without a new step (the moves inside one drag). */
export const replace = (state: History, next: Graph): History => ({ ...state, present: next })

/** Mark the present as a step (a drag's start), so the drag undoes as one. */
export const checkpoint = (state: History): History => ({ past: [...state.past, state.present], present: state.present, future: [] })

export function undo(state: History): History {
  const previous = state.past.at(-1)
  return previous ? { past: state.past.slice(0, -1), present: previous, future: [state.present, ...state.future] } : state
}

export function redo(state: History): History {
  const [next, ...rest] = state.future
  return next ? { past: [...state.past, state.present], present: next, future: rest } : state
}
