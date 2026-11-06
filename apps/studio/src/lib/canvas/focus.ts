/**
 * The canvas's focus as CSS: what is selected or under the pointer lights its connections in the accent and every
 * other step and label dims. It is a stylesheet keyed by edge id, written straight to the page, so hovering never
 * rebuilds an edge (its end grabbers, selection and identity stay as they are) and costs no render.
 */
import type { Graph } from "./graph.ts"

export interface Focus { readonly nodes: readonly string[]; readonly edges: readonly string[] }

const quoted = (value: string) => JSON.stringify(value)

/** The rules for one canvas (`scope` is its surface's data-canvas-id); none when nothing is in focus. */
export function focusCss(graph: Graph, focus: Focus, scope: string): string {
  if (focus.nodes.length + focus.edges.length === 0) return ""
  const nodes = new Set(focus.nodes), edges = new Set(focus.edges)
  const lit = graph.edges.filter(({ id, source, target }) => edges.has(id) || nodes.has(source) || nodes.has(target)).map(({ id }) => id)
  const root = `[data-canvas-id=${quoted(scope)}]`
  const labels = (id?: string) => id === undefined ? `${root} [data-edge-label], ${root} [data-exit-label]` : `${root} [data-edge-label=${quoted(id)}], ${root} [data-exit-label=${quoted(id)}]`
  return [
    `${root} .react-flow__edge:not(.canvas-edge-failing), ${labels()} { opacity: 0.35; }`,
    ...lit.flatMap((id) => [
      `${root} .react-flow__edge[data-id=${quoted(id)}], ${labels(id)} { opacity: 1; }`,
      `${root} .react-flow__edge[data-id=${quoted(id)}]:not(.canvas-edge-failing) .react-flow__edge-path { stroke: var(--edge-active); stroke-width: 2px; }`,
    ]),
  ].join("\n")
}
