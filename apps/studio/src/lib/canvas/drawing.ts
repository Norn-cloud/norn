/**
 * How a status canvas's steps are drawn, worked out once per drawing from where the states sit: steps of one
 * lifecycle step into one state through one lane draw as a bundle (one rail, one pill, a tick per source); every other
 * lane step gets its own rail; a step back along its row loops on the side away from the side states; labels are
 * placed clear of each other and of the states. Pure, so layout can score a placement by what it would draw.
 */
import type { Bundle } from "./focus.ts"
import * as g from "./graph.ts"
import { labelWidth } from "./exits.ts"
import { stepKey } from "./lifecycle.ts"
import { LANE, bundleRoute, laneRoute, laneYOf, placeLabels, railsOf, type Point, type Rail } from "./routes.ts"
import { CARD, STATE_CARD_HEIGHT } from "./viewport.ts"
import { words } from "../format.ts"

export interface Drawing {
  readonly rails: ReadonlyMap<string, Rail>
  readonly bundles: readonly (Bundle & { readonly label: string; readonly offset: number; readonly side: number })[]
}

const middle = STATE_CARD_HEIGHT / 2
/** Each step between its handles: a state's right edge out, its left edge in, at the card's middle. */
export function handlesOf(graph: g.Graph): ReadonlyMap<string, { readonly source: Point; readonly target: Point }> {
  const at = new Map(graph.nodes.map(({ id, position }) => [id, position]))
  return new Map(graph.edges.flatMap((edge) => {
    const source = at.get(edge.source), target = at.get(edge.target)
    return source && target ? [[edge.id, { source: { x: source.x + CARD.state, y: source.y + middle }, target: { x: target.x, y: target.y + middle } }] as const] : []
  }))
}

/**
 * Which side of its row a step back along the row loops. On the main row (the row with the most states), away from
 * the side states (above when level); on a side row, toward the main row, so it shares the lane between them.
 */
export function backSide(graph: g.Graph, rowY: number): number {
  const centres = graph.nodes.map(({ position }) => position.y + middle)
  const rows = new Map<number, number>()
  for (const y of centres) rows.set(Math.round(y), (rows.get(Math.round(y)) ?? 0) + 1)
  const mainY = [...rows].sort((a, b) => b[1] - a[1] || a[0] - b[0])[0]?.[0] ?? rowY
  if (Math.abs(rowY - mainY) > 2) return mainY < rowY ? -1 : 1
  const above = centres.filter((y) => y < rowY - 2).length, below = centres.filter((y) => y > rowY + 2).length
  return above > below ? 1 : -1
}

const drawings = new WeakMap<g.Graph, Drawing>()
export function drawingOf(graph: g.Graph): Drawing {
  const cached = drawings.get(graph)
  if (cached) return cached
  const handles = handlesOf(graph)
  const straight = (id: string) => { const h = handles.get(id)!; return Math.abs(h.source.y - h.target.y) < 2 && h.target.x > h.source.x }
  const sideOf = (id: string) => { const h = handles.get(id)!; return Math.abs(h.source.y - h.target.y) < 2 ? backSide(graph, h.source.y) : undefined }
  const groups = new Map<string, g.CanvasEdge[]>()
  for (const edge of graph.edges) {
    const h = handles.get(edge.id)
    if (!h || straight(edge.id)) continue
    const key = `${stepKey(edge)}|${edge.target}|${laneYOf(h.source, h.target, LANE, sideOf(edge.id))}`
    groups.set(key, [...groups.get(key) ?? [], edge])
  }
  // A bundle's id is its step and target; two bundles that share both (different guards or lanes) keep distinct ids.
  const ids = new Set<string>()
  const bundles = [...groups.values()].filter((members) => members.length > 1).map((members) => {
    const first = members[0]!
    const sorted = [...members].sort((a, b) => handles.get(a.id)!.source.x - handles.get(b.id)!.source.x)
    const base = `bundle:${first.config.transition || first.id}:${first.target}`
    const id = ids.has(base) ? `${base}:${sorted[0]!.id}` : base
    ids.add(id)
    return { id, members: sorted.map(({ id }) => id), sources: sorted.map(({ source }) => source), target: first.target, label: `${words(first.config.transition || first.id)} · from ${members.length} states`, side: sideOf(first.id) ?? -1 }
  })
  const bundled = new Set(bundles.flatMap(({ members }) => members))
  // A bundle holds the middle of its lane; the other steps take rails around it.
  const steps = [
    ...graph.edges.filter(({ id }) => handles.has(id) && !bundled.has(id)).map((edge) => ({ id: edge.id, ...handles.get(edge.id)!, width: labelWidth(words(edge.config.transition || edge.id)), ...sideOf(edge.id) === undefined ? {} : { side: sideOf(edge.id)! } })),
    ...bundles.map((bundle) => ({ id: bundle.id, ...handles.get(bundle.members[0]!)!, width: 0, side: bundle.side })),
  ]
  const assigned = railsOf(steps)
  const placedBundles = bundles.map((bundle) => ({ ...bundle, offset: assigned.get(bundle.id)?.offset ?? 0 }))
  const taken = placedBundles.map((bundle) => {
    const width = labelWidth(bundle.label)
    const { label } = bundleRoute(bundle.members.map((id) => handles.get(id)!.source), handles.get(bundle.members[0]!)!.target, width, LANE, bundle.offset, bundle.side)
    return { x1: label.x - width / 2, x2: label.x + width / 2, y: label.y }
  })
  const rails = placeLabels(steps.filter(({ id }) => !ids.has(id)), assigned, LANE, graph.nodes.map(({ position }) => ({ ...position, width: CARD.state, height: STATE_CARD_HEIGHT })), taken)
  const drawing = { rails, bundles: placedBundles }
  drawings.set(graph, drawing)
  return drawing
}

/**
 * How many times the drawn steps cross one another: each step (a bundle counts as one) as its orthogonal polyline,
 * counting a horizontal and a vertical segment of two different steps that cross strictly inside both. Shared ends
 * (two steps meeting at a state) are not crossings.
 */
export function crossings(graph: g.Graph): number {
  const handles = handlesOf(graph)
  const { rails, bundles } = drawingOf(graph)
  const bundled = new Set(bundles.flatMap(({ members }) => members))
  const drawn: (readonly (readonly Point[])[])[] = [
    ...graph.edges.filter(({ id }) => handles.has(id) && !bundled.has(id)).map(({ id }) => { const h = handles.get(id)!; return [laneRoute(h.source, h.target, LANE, rails.get(id)).points] }),
    ...bundles.map((bundle) => bundleRoute(bundle.members.map((id) => handles.get(id)!.source), handles.get(bundle.members[0]!)!.target, 0, LANE, bundle.offset, bundle.side).lines),
  ]
  const segments = drawn.map((lines) => lines.flatMap((line) => line.slice(1).map((to, i) => [line[i]!, to] as const)))
  const inside = (v: number, a: number, b: number) => v > Math.min(a, b) + 1 && v < Math.max(a, b) - 1
  let count = 0
  for (let i = 0; i < segments.length; i += 1) for (let j = i + 1; j < segments.length; j += 1) {
    for (const [a1, a2] of segments[i]!) for (const [b1, b2] of segments[j]!) {
      const aH = Math.abs(a1.y - a2.y) < 0.5, bH = Math.abs(b1.y - b2.y) < 0.5
      if (aH === bH) continue
      const [h1, h2, v1, v2] = aH ? [a1, a2, b1, b2] : [b1, b2, a1, a2]
      if (inside(v1.x, h1.x, h2.x) && inside(h1.y, v1.y, v2.y)) count += 1
    }
  }
  return count
}
