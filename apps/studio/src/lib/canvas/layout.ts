/**
 * "Tidy up": lay a whole graph out left to right, in columns by how far each node is from where the graph starts
 * and rows in the order its exits leave, then leave room for exit labels. Only on request (one undoable step);
 * a person's own placement is never moved otherwise.
 */
import { spaceForLabels } from "./exits.ts"
import type { Graph } from "./graph.ts"
import { crossings } from "./drawing.ts"

export interface Spacing { readonly width: number; readonly gap: number; readonly row: number }
/** Columns and rows for each canvas: a card's width, the gap after it, and the row pitch. */
export const STEP_SPACING: Spacing = { width: 208, gap: 64, row: 110 }
export const STATE_SPACING: Spacing = { width: 168, gap: 112, row: 150 }

export function tidy(graph: Graph, spacing: Spacing): Graph {
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
  const rows = new Map<number, number>()
  const place = new Map(order.map((id) => {
    const at = column.get(id)!
    const row = rows.get(at) ?? 0
    rows.set(at, row + 1)
    return [id, { x: at * (spacing.width + spacing.gap), y: row * spacing.row }] as const
  }))
  return spaceForLabels({ nodes: graph.nodes.map((node) => ({ ...node, position: place.get(node.id) ?? node.position })), edges: graph.edges })
}

/**
 * Tidy up for a status canvas: the main row is the states every way to the final state passes through; every other state
 * (a terminal or side state such as Cancelled or On hold) sits in a band above or below it, at the column of the
 * main-row states it connects to. Each side state's band is chosen to draw the fewest crossings (what the router
 * would draw, counted), and steps back along the row loop on the side away from the side states. Deterministic:
 * the same graph always tidies the same way.
 */
export function tidyLifecycle(graph: Graph, spacing: Spacing = STATE_SPACING): Graph {
  if (graph.nodes.length === 0) return graph
  const out = new Map(graph.nodes.map(({ id }) => [id, graph.edges.filter(({ source, target }) => source === id && target !== id).map(({ target }) => target)]))
  const incoming = new Set(graph.edges.map(({ target }) => target))
  const start = graph.nodes.find(({ config }) => config.initial === "yes")?.id ?? graph.nodes.find(({ id }) => !incoming.has(id))?.id ?? graph.nodes[0]!.id
  // The main row: the states every way from the start to its final state must pass through (that state's
  // dominators), in the order they are reached. A side state such as On hold, which the row can skip, stays off it.
  const ids = graph.nodes.map(({ id }) => id)
  const preds = new Map(ids.map((id) => [id, graph.edges.filter(({ target, source }) => target === id && source !== id).map(({ source }) => source)]))
  const distance = new Map<string, number>([[start, 0]])
  for (const queue = [start]; queue.length;) { const at = queue.shift()!; for (const next of out.get(at) ?? []) if (!distance.has(next)) { distance.set(next, distance.get(at)! + 1); queue.push(next) } }
  const reached = ids.filter((id) => distance.has(id))
  const dominators = new Map(reached.map((id) => [id, new Set(id === start ? [start] : reached)]))
  for (let changed = true; changed;) {
    changed = false
    for (const id of reached) {
      if (id === start) continue
      const inherited = (preds.get(id) ?? []).filter((p) => dominators.has(p)).map((p) => dominators.get(p)!)
      const next = new Set([id, ...inherited.length ? [...inherited[0]!].filter((d) => inherited.every((set) => set.has(d))) : []])
      if (next.size !== dominators.get(id)!.size) { dominators.set(id, next); changed = true }
    }
  }
  // The final state: a terminal whose dominator chain is longest (the happy path), else the farthest state reached.
  const terminals = reached.filter((id) => graph.nodes.find((node) => node.id === id)?.config.terminal === "yes")
  const end = [...terminals.length ? terminals : reached].sort((a, b) => dominators.get(b)!.size - dominators.get(a)!.size || distance.get(b)! - distance.get(a)! || ids.indexOf(a) - ids.indexOf(b))[0]!
  const best = [...dominators.get(end)!].sort((a, b) => distance.get(a)! - distance.get(b)!)
  const pitch = spacing.width + spacing.gap
  const column = new Map(best.map((id, index) => [id, index]))
  const side = graph.nodes.map(({ id }) => id).filter((id) => !column.has(id))
  // A side state sits one column right of the first main-row state that leads into it (so it is always entered
  // from the left), else under the first main-row state it leads to, else after the row.
  const wanted = new Map(side.map((id) => {
    const from = graph.edges.filter(({ source, target }) => target === id && column.has(source)).map(({ source }) => column.get(source)!)
    const to = graph.edges.filter(({ source, target }) => source === id && column.has(target)).map(({ target }) => column.get(target)!)
    return [id, from.length ? Math.min(...from) + 1 : to.length ? Math.min(...to) : best.length] as const
  }))
  const place = (bands: readonly number[]): Graph => {
    const taken = new Set<string>()
    const at = new Map<string, { x: number; y: number }>(best.map((id, index) => [id, { x: index * pitch, y: 0 }]))
    side.forEach((id, index) => {
      const band = bands[index]!
      let col = wanted.get(id)!
      while (taken.has(`${band}:${col}`)) col += 1
      taken.add(`${band}:${col}`)
      at.set(id, { x: col * pitch, y: band * spacing.row })
    })
    return { nodes: graph.nodes.map((node) => ({ ...node, position: at.get(node.id) ?? node.position })), edges: graph.edges }
  }
  // Every band choice for up to 12 side states (2^12 drawings at most), else all below; fewest crossings wins,
  // ties going to fewer states above (below first, the familiar reading).
  if (side.length > 12) return place(side.map(() => 1))
  let chosen = place(side.map(() => 1)), fewest = crossings(chosen), above = 0
  for (let mask = 1; mask < 1 << side.length; mask += 1) {
    const bands = side.map((_, index) => (mask >> index) & 1 ? -1 : 1)
    const candidate = place(bands)
    const count = crossings(candidate), up = bands.filter((band) => band < 0).length
    if (count < fewest || (count === fewest && up < above)) { chosen = candidate; fewest = count; above = up }
  }
  return chosen
}
