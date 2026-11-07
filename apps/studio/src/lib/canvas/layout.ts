/**
 * "Tidy up": lay a whole graph out left to right, in columns by how far each node is from where the graph starts
 * and rows in the order its exits leave, then leave room for exit labels. Only on request (one undoable step);
 * a person's own placement is never moved otherwise.
 */
import { spaceForLabels } from "./exits.ts"
import type { Graph } from "./graph.ts"

export interface Spacing { readonly width: number; readonly gap: number; readonly row: number }
/** Columns and rows for each canvas: a card's width, the gap after it, and the row pitch. */
export const STEP_SPACING: Spacing = { width: 208, gap: 64, row: 110 }
export const STATE_SPACING: Spacing = { width: 168, gap: 112, row: 150 }

/** The clear space under a card in a column; the row pitch is a card's height plus this, at least `spacing.row`. */
const ROW_GAP = 36

/** `height` gives each card's drawn height, so a tall card (a step with many exits) pushes the next row down instead of overlapping it. */
export function tidy(graph: Graph, spacing: Spacing, height?: (node: Graph["nodes"][number]) => number): Graph {
  if (graph.nodes.length === 0) return graph
  const incoming = new Set(graph.edges.map(({ target }) => target))
  // Where the graph starts: an automation's start, a lifecycle's initial states, else nodes nothing leads to.
  const starts = graph.nodes.filter(({ kind }) => kind === "start").map(({ id }) => id)
  const initial = graph.nodes.filter(({ config }) => config.initial === "yes").map(({ id }) => id)
  const roots = starts.length ? starts : initial.length ? initial : graph.nodes.filter(({ id }) => !incoming.has(id)).map(({ id }) => id)
  const column = new Map<string, number>()
  const order: string[] = []
  const queue = (roots.length ? roots : [graph.nodes[0]!.id]).map((id) => ({ id, depth: 0 }))
  for (const { id } of queue) column.set(id, 0)
  while (queue.length) {
    const { id, depth } = queue.shift()!
    order.push(id)
    for (const edge of graph.edges) if (edge.source === id && !column.has(edge.target)) { column.set(edge.target, depth + 1); queue.push({ id: edge.target, depth: depth + 1 }) }
  }
  // Anything the starts do not reach goes after, in a column of its own, in drawing order.
  const last = Math.max(0, ...column.values())
  for (const { id } of graph.nodes) if (!column.has(id)) { column.set(id, last + 1); order.push(id) }
  const byId = new Map(graph.nodes.map((node) => [node.id, node]))
  const next = new Map<number, number>()
  const place = new Map(order.map((id) => {
    const at = column.get(id)!
    const y = next.get(at) ?? 0
    next.set(at, y + Math.max(spacing.row, (height?.(byId.get(id)!) ?? 0) + ROW_GAP))
    return [id, { x: at * (spacing.width + spacing.gap), y }] as const
  }))
  return spaceForLabels({ nodes: graph.nodes.map((node) => ({ ...node, position: place.get(node.id) ?? node.position })), edges: graph.edges })
}
