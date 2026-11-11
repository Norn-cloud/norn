/**
 * What an automation step's exits say on the canvas, and room for them: a route's branch exits are labelled by
 * their conditions with the part they share elided (`status == paid`, `status == draft` read `paid`, `draft`), so
 * siblings differ at a glance; the full condition stays on hover and in the inspector.
 */
import { branchesOf, type Condition } from "./automation.ts"
import { optionPort, optionsOf } from "./decision.ts"
import type { CanvasNode, Graph } from "./graph.ts"
import { CARD } from "./viewport.ts"

const STEP_WIDTH = CARD.step
/** Room after a step with labelled exits: the widest label plus this, never more than LABEL_ROOM_MAX. */
export const LABEL_ROOM_PAD = 24
export const LABEL_ROOM_MAX = 200

export const formatCondition = (all: readonly Condition[]) => all.map(({ left, op, right }) => `${left} ${op} ${right}`).join(" and ")

/** What an exit means in words: a route branch's own condition, a decision option's label, or the port. */
export function exitLabel(node: CanvasNode | undefined, port: string): string | undefined {
  if (!node || port === "next") return undefined
  const branch = /^b(\d+)$/.exec(port)
  if (branch && node.kind === "route") return formatCondition(branchesOf(node)[Number(branch[1])] ?? []) || "no condition yet"
  if (port.startsWith("opt:") && node.kind === "decide") return optionsOf(node).find((option) => optionPort(option) === port)?.label ?? port.slice(4)
  return port
}

/** A condition without its result prefixes (`invoice.status` reads `status`). */
const bare = (text: string) => text.replace(/\b[A-Za-z_][A-Za-z0-9_]*\.(?=[A-Za-z_])/g, "")

/** The words every text starts with, cut back to a whole word (up to and including a space). */
function sharedWords(texts: readonly string[]): string {
  let prefix = texts[0] ?? ""
  for (const text of texts) while (!text.startsWith(prefix)) prefix = prefix.slice(0, -1)
  const cut = prefix.lastIndexOf(" ")
  return cut >= 0 ? prefix.slice(0, cut + 1) : ""
}

/** Each labelled exit of a step: the text drawn on it and the full meaning for hover. */
export function exitLabels(node: CanvasNode): ReadonlyMap<string, { readonly label: string; readonly full: string }> {
  const ports = node.kind === "route" ? branchesOf(node).map((_, index) => `b${index}`) : node.kind === "decide" ? optionsOf(node).map(optionPort) : []
  const entries = ports.map((port) => ({ port, full: exitLabel(node, port) ?? port }))
  const texts = entries.map(({ full }) => node.kind === "route" ? bare(full) : full)
  const shared = node.kind === "route" && texts.length >= 2 ? sharedWords(texts) : ""
  const elided = texts.map((text) => text.slice(shared.length))
  // Elide only when every sibling keeps words of its own and they stay distinct.
  const use = shared !== "" && elided.every(Boolean) && new Set(elided).size === elided.length ? elided : texts
  return new Map(entries.map(({ port, full }, index) => [port, { label: use[index]!, full }]))
}

/** About how wide a label draws (11px medium text, its padding and border), without measuring. */
export const labelWidth = (text: string) => Math.ceil(text.length * 6.4) + 18

/**
 * Room for labels: after each step with labelled exits, the steps those exits lead to sit at least the widest
 * label plus LABEL_ROOM_PAD to the right (at most LABEL_ROOM_MAX); everything from there rightwards moves with
 * them, so the drawing keeps its order. Used for the layouts Studio draws; a person's own placement is theirs.
 */
export function spaceForLabels(graph: Graph): Graph {
  let nodes = [...graph.nodes]
  for (const source of [...nodes].sort((a, b) => a.position.x - b.position.x)) {
    const labels = exitLabels(nodes.find(({ id }) => id === source.id)!)
    if (labels.size === 0) continue
    const room = Math.min(LABEL_ROOM_MAX, Math.max(...[...labels.values()].map(({ label }) => labelWidth(label))) + LABEL_ROOM_PAD)
    const from = nodes.find(({ id }) => id === source.id)!.position.x + STEP_WIDTH
    const targets = graph.edges.filter((edge) => edge.source === source.id && labels.has(edge.sourcePort)).map(({ target }) => nodes.find(({ id }) => id === target)!).filter(Boolean)
    const nearest = Math.min(...targets.filter(({ position }) => position.x > from - STEP_WIDTH / 2).map(({ position }) => position.x))
    if (!Number.isFinite(nearest) || nearest - from >= room) continue
    const shift = from + room - nearest
    nodes = nodes.map((node) => node.position.x >= nearest ? { ...node, position: { ...node.position, x: node.position.x + shift } } : node)
  }
  return { nodes, edges: graph.edges }
}
