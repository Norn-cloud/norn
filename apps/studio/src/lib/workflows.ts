import type { Envelope, Schemas } from "./transport.ts"

type Lifecycle = Schemas["NornIrLifecycle"]
type Transition = Schemas["NornIrTransition"]
type Action = Extract<Envelope, { kind: "action" }>
type Law = Extract<Envelope, { kind: "law" }>

/** A step of a lifecycle, with the declared actions that fire it. */
export interface Step {
  readonly transition: Transition
  readonly actions: readonly Action["payload"][]
}

/** One admitted lifecycle, as its object type declares it. */
export interface Workflow {
  readonly objectType: string
  readonly description: string | undefined
  readonly lifecycle: Lifecycle
  readonly steps: readonly Step[]
  /** Laws scoped to the object type. */
  readonly laws: readonly Law["payload"][]
  /** Each state's column (shortest declared path from an initial state) and row within that column. */
  readonly layout: ReadonlyMap<string, { readonly column: number; readonly row: number }>
}

/** Columns by breadth-first distance from the initial states; unreachable states follow in a last column. */
export function layout(lifecycle: Lifecycle): Map<string, { column: number; row: number }> {
  const transitions = lifecycle.transitions ?? []
  const depth = new Map<string, number>(lifecycle.initial.map((state) => [state, 0]))
  for (let frontier = [...lifecycle.initial]; frontier.length > 0;) {
    const next: string[] = []
    for (const state of frontier) {
      for (const { from, to } of transitions) {
        if (from.includes(state) && !depth.has(to)) {
          depth.set(to, (depth.get(state) ?? 0) + 1)
          next.push(to)
        }
      }
    }
    frontier = next
  }
  const last = Math.max(0, ...depth.values()) + 1
  for (const state of transitions.flatMap(({ from, to }) => [...from, to])) if (!depth.has(state)) depth.set(state, last)
  const rows = new Map<number, number>()
  const positions = new Map<string, { column: number; row: number }>()
  for (const [state, column] of depth) {
    const row = rows.get(column) ?? 0
    rows.set(column, row + 1)
    positions.set(state, { column, row })
  }
  return positions
}

/** Every lifecycle declared by an object type among the definitions, with its firing actions and governing laws. */
export function workflows(definitions: readonly Envelope[]): Workflow[] {
  const actions = definitions.flatMap((envelope) => envelope.kind === "action" ? [envelope.payload] : [])
  const laws = definitions.flatMap((envelope) => envelope.kind === "law" ? [envelope.payload] : [])
  return definitions.flatMap((envelope) => {
    if (envelope.kind !== "entity" || !("objectType" in envelope.payload)) return []
    const { name, description, lifecycles } = envelope.payload.objectType
    return (lifecycles ?? []).map((lifecycle) => ({
      objectType: name,
      description,
      lifecycle,
      steps: (lifecycle.transitions ?? []).map((transition) => ({
        transition,
        actions: actions.filter(({ effectProgram }) => effectProgram?.kind === "fire_transition"
          && effectProgram.objectKind === name && effectProgram.lifecycle === lifecycle.property && effectProgram.transition === transition.id),
      })),
      laws: laws.filter(({ scope }) => scope.objectTypes.includes(name)),
      layout: layout(lifecycle),
    }))
  })
}

/** The fields a declared guard reads, in words: `self.candidate_digest` → `candidate digest`. */
export const guardedFields = (guards: readonly string[]): string[] =>
  [...new Set(guards.flatMap((guard) => [...guard.matchAll(/\bself\.([A-Za-z_][A-Za-z0-9_]*)/g)].map(([, field = ""]) => field.replaceAll("_", " "))))]
