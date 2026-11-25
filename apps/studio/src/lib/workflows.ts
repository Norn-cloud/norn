import type { RunFlow } from "./flows.ts"
import { words } from "./format.ts"
import type { Envelope, Schemas } from "./transport.ts"

/** The DOM anchor id for one admitted workflow section. */
export const workflowAnchor = (workflow: Pick<Workflow, "objectType" | "lifecycle">) =>
  `workflow-${workflow.objectType}-${workflow.lifecycle.property}`

/** A workflow's display name: the title of the flow whose runs it is, or its object type in words. */
export const workflowTitle = (workflow: Pick<Workflow, "objectType">, flows: ReadonlyMap<string, RunFlow>) =>
  flows.get(workflow.objectType)?.title ?? words(workflow.objectType)

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
    }))
  })
}

/** The fields a declared guard reads, in words: `self.candidate_digest` → `candidate digest`. */
export const guardedFields = (guards: readonly string[]): string[] =>
  [...new Set(guards.flatMap((guard) => [...guard.matchAll(/\bself\.([A-Za-z_][A-Za-z0-9_]*)/g)].map(([, field = ""]) => field.replaceAll("_", " "))))]
