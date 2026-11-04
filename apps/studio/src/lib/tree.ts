import type { Schemas } from "./transport.ts"

type Lifecycle = Schemas["NornIrLifecycle"]
type Transition = Schemas["NornIrTransition"]

/** A placed element of the decision tree: a state or one transition out of one source state. */
export type TreeNode =
  | { readonly kind: "state"; readonly id: string; readonly state: string; readonly column: number; readonly row: number }
  | { readonly kind: "step"; readonly id: string; readonly transition: Transition; readonly from: string; readonly column: number; readonly row: number; readonly order: number }

export interface Tree {
  /** The happy path: start to a declared terminal along the longest simple path (else the longest simple path). */
  readonly spine: readonly string[]
  readonly nodes: readonly TreeNode[]
  /** Connectors between placed elements: state → step and step → state. */
  readonly edges: readonly { readonly source: string; readonly target: string }[]
  /** Steps back to an already-placed state, shown as chips on their source state instead of long arcs. */
  readonly backs: ReadonlyMap<string, readonly { readonly transition: Transition; readonly to: string }[]>
}

export const stepId = (transition: Transition, from: string) => `${transition.id}:${from}`

/**
 * The happy path: the shortest path from the start to the declared terminal furthest from it;
 * with no reachable terminal, the longest simple path from the start.
 */
export function spineOf(lifecycle: Lifecycle): string[] {
  const transitions = lifecycle.transitions ?? []
  const parent = new Map<string, string | undefined>(lifecycle.initial.map((state) => [state, undefined]))
  const order = [...lifecycle.initial]
  for (let index = 0; index < order.length; index += 1) {
    const at = order[index] ?? ""
    for (const { from, to } of transitions) if (from.includes(at) && !parent.has(to)) { parent.set(to, at); order.push(to) }
  }
  const terminal = order.filter((state) => lifecycle.terminal?.includes(state)).at(-1)
  if (terminal !== undefined) {
    const path: string[] = []
    for (let at: string | undefined = terminal; at !== undefined; at = parent.get(at)) path.unshift(at)
    return path
  }
  let best: string[] = []
  let budget = 50_000
  const walk = (path: string[]) => {
    if (--budget < 0) return
    if (path.length > best.length) best = [...path]
    for (const { from, to } of transitions) if (from.includes(path.at(-1) ?? "") && !path.includes(to)) walk([...path, to])
  }
  for (const start of lifecycle.initial) walk([start])
  return best
}

/**
 * The lifecycle laid out left to right: the spine on row 0 (state, step, state, …), every other
 * forward step stacked below its source in the next column, its target after it. A step to a
 * state already placed in an earlier or the same column becomes a back chip on its source.
 */
export function treeOf(lifecycle: Lifecycle): Tree {
  const transitions = lifecycle.transitions ?? []
  const spine = spineOf(lifecycle)
  const placed = new Map<string, { column: number; row: number }>()
  const nodes: TreeNode[] = []
  const edges: { source: string; target: string }[] = []
  const backs = new Map<string, { transition: Transition; to: string }[]>()
  const rows = new Map<number, number>()
  const take = (column: number, row?: number) => {
    const next = Math.max(row ?? 0, rows.get(column) ?? 0)
    rows.set(column, next + 1)
    return next
  }
  const placeState = (state: string, column: number, row?: number) => {
    const at = { column, row: take(column, row) }
    placed.set(state, at)
    nodes.push({ kind: "state", id: state, state, ...at })
  }
  spine.forEach((state, index) => placeState(state, index * 2, 0))
  let order = 0
  const visit = (queue: string[]) => {
    while (queue.length > 0) {
      const from = queue.shift() ?? ""
      const source = placed.get(from)
      if (source === undefined) continue
      const along = (transition: Transition) => spine.indexOf(from) >= 0 && spine[spine.indexOf(from) + 1] === transition.to
      // The spine's own step first, so it keeps row 0.
      const outgoing = transitions.filter(({ from: sources }) => sources.includes(from)).sort((a, b) => Number(along(b)) - Number(along(a)))
      for (const transition of outgoing) {
        const target = placed.get(transition.to)
        const onSpine = along(transition)
        if (target !== undefined && !onSpine && target.column <= source.column) {
          backs.set(from, [...backs.get(from) ?? [], { transition, to: transition.to }])
          continue
        }
        const id = stepId(transition, from)
        const step = { column: source.column + 1, row: take(source.column + 1, onSpine ? 0 : source.row) }
        nodes.push({ kind: "step", id, transition, from, ...step, order: ++order })
        edges.push({ source: from, target: id })
        if (target === undefined) { placeState(transition.to, step.column + 1, step.row); queue.push(transition.to) }
        edges.push({ source: id, target: transition.to })
      }
    }
  }
  visit([...spine])
  // States no step reaches from the start still appear, below everything else.
  const unreached = transitions.flatMap(({ from, to }) => [...from, to]).filter((state, index, all) => !placed.has(state) && all.indexOf(state) === index)
  const bottom = Math.max(0, ...rows.values())
  for (const state of unreached) { placeState(state, 0, bottom); visit([state]) }
  return { spine, nodes, edges, backs }
}

/** The elements on the shortest path from the start to a node, and the options right after it. */
export function pathTo(tree: Tree, id: string): { readonly path: readonly string[]; readonly next: ReadonlySet<string> } {
  const parents = new Map<string, string>()
  const roots = tree.nodes.filter((node) => node.kind === "state" && node.column === 0 && node.row === 0).map(({ id: root }) => root)
  const queue = [...roots]
  const seen = new Set(queue)
  while (queue.length > 0) {
    const current = queue.shift() ?? ""
    for (const { source, target } of tree.edges) if (source === current && !seen.has(target)) { seen.add(target); parents.set(target, current); queue.push(target) }
  }
  const path: string[] = []
  for (let at: string | undefined = id; at !== undefined && path.length <= tree.nodes.length; at = parents.get(at)) path.unshift(at)
  const next = new Set<string>()
  for (const { source, target } of tree.edges) if (source === id) {
    next.add(target)
    for (const edge of tree.edges) if (edge.source === target) next.add(edge.target)
  }
  return { path, next }
}

/** Large lifecycles show the focused path and two hops on; the rest folds into a count. Small ones show everything. */
export function visibleOf(tree: Tree, focus: string | undefined, expanded: boolean): { readonly shown: ReadonlySet<string>; readonly hidden: number } {
  const states = tree.nodes.filter(({ kind }) => kind === "state").length
  if (expanded || states <= 8) return { shown: new Set(tree.nodes.map(({ id }) => id)), hidden: 0 }
  const shown = new Set<string>(focus === undefined ? tree.spine.slice(0, 3) : pathTo(tree, focus).path)
  // Two states on: four connectors (step, state, step, state), one ring at a time.
  for (let hop = 0; hop < 4; hop += 1) {
    const ring = tree.edges.filter(({ source }) => shown.has(source)).map(({ target }) => target)
    for (const id of ring) shown.add(id)
  }
  for (const node of tree.nodes) if (node.kind === "step" && shown.has(node.id)) shown.add(node.from)
  const hidden = tree.nodes.filter((node) => node.kind === "state" && !shown.has(node.id)).length
  return { shown, hidden }
}
