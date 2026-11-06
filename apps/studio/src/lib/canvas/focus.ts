/**
 * The canvas's focus as CSS: what is selected or under the pointer lights its connections in the accent and every
 * other step and label dims. It is a stylesheet keyed by edge id, written straight to the page, so hovering never
 * rebuilds an edge (its end grabbers, selection and identity stay as they are) and costs no render.
 */
import type { Graph } from "./graph.ts"

export interface Focus { readonly nodes: readonly string[]; readonly edges: readonly string[] }
/** Steps drawn as one edge (`id`): the steps it holds, where they come from, and the state they go to. */
export interface Bundle { readonly id: string; readonly members: readonly string[]; readonly sources: readonly string[]; readonly target: string }

const quoted = (value: string) => JSON.stringify(value)

/** The rules for one canvas (`scope` is its surface's data-canvas-id); none when nothing is in focus. */
export function focusCss(graph: Graph, focus: Focus, scope: string, bundles: readonly Bundle[] = []): string {
  if (focus.nodes.length + focus.edges.length === 0) return ""
  // A bundle in focus is its steps in focus, and its sources are lit as well.
  const focused = bundles.filter(({ id, members }) => focus.edges.includes(id) || members.every((member) => focus.edges.includes(member)))
  const nodes = new Set(focus.nodes), edges = new Set(focus.edges.flatMap((id) => bundles.find((bundle) => bundle.id === id)?.members ?? [id]))
  const steps = graph.edges.filter(({ id, source, target }) => edges.has(id) || nodes.has(source) || nodes.has(target)).map(({ id }) => id)
  const lit = [...steps, ...bundles.filter(({ members }) => members.some((member) => steps.includes(member))).map(({ id }) => id)]
  const root = `[data-canvas-id=${quoted(scope)}]`
  const labels = (id?: string) => id === undefined ? `${root} [data-edge-label], ${root} [data-exit-label]` : `${root} [data-edge-label=${quoted(id)}], ${root} [data-exit-label=${quoted(id)}]`
  return [
    `${root} .react-flow__edge:not(.canvas-edge-failing), ${labels()} { opacity: 0.35; }`,
    ...lit.flatMap((id) => [
      `${root} .react-flow__edge[data-id=${quoted(id)}], ${labels(id)} { opacity: 1; }`,
      `${root} .react-flow__edge[data-id=${quoted(id)}]:not(.canvas-edge-failing) .react-flow__edge-path { stroke: var(--edge-active); stroke-width: 2px; }`,
    ]),
    ...focused.flatMap(({ sources }) => sources.map((source) => `${root} .react-flow__node[data-id=${quoted(source)}] .canvas-node { box-shadow: var(--node-highlight), 0 0 0 2px var(--canvas-surface), 0 0 0 4px var(--edge-active), var(--elevation-node-hover); }`)),
  ].join("\n")
}
