import type { Envelope, Schemas } from "../transport.ts"
import { treeOf } from "../tree.ts"
import type { CanvasEdge, CanvasNode, Graph, Kinds } from "./graph.ts"

type Lifecycle = Schemas["NornIrLifecycle"]
type Transition = Schemas["NornIrTransition"]

/** Where a drawn node or edge lands in the compiled artifact, so a finding there can point back at it. */
export interface Origin {
  readonly pointer: string
  readonly node?: string
  readonly edge?: string
}

/** One lifecycle of one object type, the unit this canvas edits. */
export interface LifecycleRef {
  readonly artifactId: string
  readonly objectType: string
  readonly index: number
  readonly property: string
}

const objectTypeOf = (envelope: Envelope) =>
  envelope.kind === "entity" && "objectType" in envelope.payload ? envelope.payload.objectType : undefined

/** Every lifecycle the definitions declare. */
export const lifecyclesIn = (definitions: readonly Envelope[]): LifecycleRef[] => definitions.flatMap((envelope) => {
  const objectType = objectTypeOf(envelope)
  return (objectType?.lifecycles ?? []).map(({ property }, index) => ({ artifactId: envelope.artifactId, objectType: objectType?.name ?? "", index, property }))
})

/** A state's ports carry its lifecycle, so states of two lifecycles cannot be joined. */
export const stateType = (ref: Pick<LifecycleRef, "objectType" | "property">) => `state:${ref.objectType}.${ref.property}`

export const lifecycleKinds = (ref: LifecycleRef): Kinds => ({
  state: () => [{ id: "in", type: stateType(ref), side: "in" }, { id: "out", type: stateType(ref), side: "out" }],
})

const yes = (flag: boolean) => flag ? "yes" : ""

/** A transition from several states is drawn as one edge per source, sharing its id. */
export const edgeId = (transition: string, from: string) => `${transition}:${from}`

/**
 * Which edges are one step: the same step id, target, guards and description. Edges sharing an id but not the
 * rest stay apart, so the checker names the clash instead of one silently winning; the inspector edits a step's
 * edges by this same key, so editing one never overwrites the other.
 */
export const stepKey = (edge: CanvasEdge): string => JSON.stringify([edge.config.transition || edge.id, edge.target, edge.config.guards ?? "", edge.config.description ?? ""])

/** The lifecycle as a graph: states are nodes, each transition's sources are edges into its target. */
export function decompile(envelope: Envelope, ref: LifecycleRef): Graph {
  const lifecycle = objectTypeOf(envelope)?.lifecycles?.[ref.index]
  if (!lifecycle) throw new Error(`${ref.artifactId} has no lifecycle ${ref.index}`)
  const states = enumValues(envelope, ref.property)
  // The workflow view's left-to-right layout (#950): states sit on its even columns, steps between them.
  const placed = new Map(treeOf(lifecycle).nodes.flatMap((node) => node.kind === "state" ? [[node.state, { column: node.column / 2, row: node.row }] as const] : []))
  const lastColumn = Math.max(0, ...[...placed.values()].map(({ column }) => column)) + 1
  const nodes: CanvasNode[] = states.map((state, row) => {
    const at = placed.get(state) ?? { column: lastColumn, row }
    return {
      id: state,
      kind: "state",
      // A column is a state card and a gap wide enough for a step's label between ports (STATE_SPACING).
      position: { x: at.column * 280, y: at.row * 110 },
      config: {
        initial: yes(lifecycle.initial.includes(state)),
        terminal: yes(lifecycle.terminal?.includes(state) ?? false),
        immutable: yes(lifecycle.immutable?.includes(state) ?? false),
      },
    }
  })
  const edges: CanvasEdge[] = (lifecycle.transitions ?? []).flatMap((transition) => transition.from.map((from) => ({
    id: edgeId(transition.id, from),
    source: from,
    sourcePort: "out",
    target: transition.to,
    targetPort: "in",
    config: { transition: transition.id, guards: (transition.guards ?? []).join("\n"), description: transition.description ?? "" },
  })))
  return { nodes, edges }
}

type Property = NonNullable<Schemas["NornIrObjectTypeDef"]["properties"]>[number]
type EnumValue = { readonly value: string } & Record<string, unknown>

const propertyOf = (envelope: Envelope, property: string): Property | undefined =>
  objectTypeOf(envelope)?.properties?.find(({ id }) => id === property)

function enumValues(envelope: Envelope, property: string): string[] {
  const type = propertyOf(envelope, property)?.type as { kind?: string; values?: EnumValue[] } | undefined
  return type?.kind === "enum" ? (type.values ?? []).map(({ value }) => value) : []
}

/**
 * The entity artifact with this lifecycle replaced by the graph, and the
 * lifecycle property's enum set to the drawn states. Everything else in the
 * artifact is kept as admitted.
 */
export function compile(envelope: Envelope, ref: LifecycleRef, graph: Graph): { readonly envelope: Envelope; readonly origins: readonly Origin[] } {
  const next = structuredClone(envelope) as Envelope & { payload: { objectType: Schemas["NornIrObjectTypeDef"] } }
  const objectType = next.payload.objectType
  const original = objectType.lifecycles?.[ref.index]
  if (!original) throw new Error(`${ref.artifactId} has no lifecycle ${ref.index}`)
  const at = `/payload/objectType/lifecycles/${ref.index}`
  const origins: Origin[] = [{ pointer: at }]
  const states = graph.nodes.filter(({ kind }) => kind === "state")
  const flagged = (flag: string) => states.filter(({ config }) => config[flag] === "yes").map(({ id }) => id)

  const groups = new Map<string, CanvasEdge[]>()
  // Edges sharing an id but not a target, guards or description stay apart, so the checker names the clash instead of one silently winning.
  for (const edge of graph.edges) {
    const key = stepKey(edge)
    groups.set(key, [...(groups.get(key) ?? []), edge])
  }
  const transitions: Transition[] = [...groups.values()].map((edges, position) => {
    const [first] = edges as [CanvasEdge, ...CanvasEdge[]]
    const id = first.config.transition || first.id
    edges.forEach((edge, index) => origins.push({ pointer: `${at}/transitions/${position}/from/${index}`, edge: edge.id }))
    origins.push({ pointer: `${at}/transitions/${position}`, edge: first.id })
    const guards = (first.config.guards ?? "").split("\n").map((guard) => guard.trim()).filter(Boolean)
    const description = first.config.description?.trim()
    return { id, from: edges.map(({ source }) => source), to: first.target, ...(guards.length ? { guards } : {}), ...(description ? { description } : {}) }
  })

  const lifecycle: Lifecycle = { ...original, initial: flagged("initial"), transitions }
  const terminal = flagged("terminal")
  const immutable = flagged("immutable")
  if (terminal.length) lifecycle.terminal = terminal; else delete lifecycle.terminal
  if (immutable.length) lifecycle.immutable = immutable; else delete lifecycle.immutable
  for (const [member, list] of [["initial", lifecycle.initial], ["terminal", terminal], ["immutable", immutable]] as const)
    list.forEach((state, index) => origins.push({ pointer: `${at}/${member}/${index}`, node: state }))
  objectType.lifecycles = objectType.lifecycles?.map((other, index) => index === ref.index ? lifecycle : other)

  const propertyIndex = objectType.properties?.findIndex(({ id }) => id === ref.property) ?? -1
  const property = objectType.properties?.[propertyIndex]
  const type = property?.type as { kind?: string; values?: EnumValue[] } | undefined
  if (type?.kind === "enum") {
    const kept = new Map((type.values ?? []).map((value) => [value.value, value]))
    type.values = states.map(({ id }) => kept.get(id) ?? { value: id })
    states.forEach(({ id }, index) => origins.push({ pointer: `/payload/objectType/properties/${propertyIndex}/type/values/${index}`, node: id }))
  }
  return { envelope: next, origins }
}
