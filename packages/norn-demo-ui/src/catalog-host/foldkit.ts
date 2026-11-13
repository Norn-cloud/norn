// A Foldkit component's machine, compiled into Norn's machine IR (UI-CATALOG-1, rung c).
//
// Foldkit's `Machine.define` already declares the component as a statechart: its initial state, its
// state tags, and every edge (source, message tag, target, guard kind). The compiler reads that
// declaration, so the machine is written once, in the component, and the checker and the host see
// exactly what the component runs. `when` and `otherwise` edges are decided by the component's own
// code: they compile to component-guarded transitions (May for the checker; the host accepts them
// only with the target the component reports). Message payloads come from the Message union's
// schema fields.

import type { Machine, MachineField, MachineTransition, StateRole, ValueType } from "../machine/machine.ts"

/** The parts of a Foldkit `Machine` the compiler reads (`foldkit/experimental/machine`). */
export interface FoldkitMachine {
  readonly initial: { readonly _tag: string }
  readonly stateTags: readonly string[]
  readonly edges: readonly {
    readonly from: string
    readonly messageTag: string
    readonly target: string
    readonly guard: { readonly _tag: "Unguarded" } | { readonly _tag: "When" | "Otherwise"; readonly position: number }
  }[]
}

type SchemaLike = { readonly ast?: { readonly _tag?: string; readonly encoding?: unknown }; readonly fields?: Readonly<Record<string, SchemaLike>> }

/**
 * A Foldkit Message union (`defineMessageUnion`). Its runtime `members` are the tagged struct
 * constructors, but its public type does not list them, so the union is taken as any object and its
 * members are read through `SchemaLike`.
 */
export type FoldkitMessages = object

export interface CompileOptions {
  /** The component's Message union, for event payload types. */
  readonly messages?: FoldkitMessages
  /** Roles for states that are not plain idle states (an error to recover from, a finished state). */
  readonly roles?: Readonly<Record<string, Exclude<StateRole, "pending">>>
}

/** Effect Schema AST tags to Norn value types; anything richer is carried as JSON. */
function valueType(schema: SchemaLike | undefined): { type: ValueType; required: boolean } {
  const tag = schema?.ast?._tag
  switch (tag) {
    case "String":
    case "TemplateLiteral":
      return { type: "text", required: true }
    case "Number":
      return { type: "number", required: true }
    case "Boolean":
      return { type: "boolean", required: true }
    case "Literal":
      return { type: "text", required: true }
    default:
      return { type: "json", required: true }
  }
}

function tagOf(member: SchemaLike): string | null {
  const literal = member.fields?._tag?.ast as { readonly literal?: unknown } | undefined
  return typeof literal?.literal === "string" ? literal.literal : null
}

/** The payload a message tag carries, as declared by the union; empty when no union is given. */
function payloadOf(messages: FoldkitMessages | undefined, tag: string): MachineField[] {
  const members = (messages as { readonly members?: readonly SchemaLike[] } | undefined)?.members
  const member = members?.find((candidate) => tagOf(candidate) === tag)
  if (!member?.fields) return []
  return Object.entries(member.fields)
    .filter(([name]) => name !== "_tag")
    .map(([fieldId, schema]) => ({ fieldId, ...valueType(schema) }))
}

/** Compile a Foldkit machine. Edge order is kept, so the first matching edge stays the first. */
export function machineFromFoldkit(machine: FoldkitMachine, options: CompileOptions = {}): Machine {
  const messageTags = [...new Set(machine.edges.map((edge) => edge.messageTag))]
  const counts = new Map<string, number>()
  const transitions: MachineTransition[] = machine.edges.map((edge) => {
    const key = `${edge.from}--${edge.messageTag}`
    const n = counts.get(key) ?? 0
    counts.set(key, n + 1)
    return {
      transitionId: n === 0 ? key : `${key}--${n}`,
      from: edge.from,
      on: edge.messageTag,
      target: edge.target,
      ...(edge.guard._tag === "Unguarded" ? {} : { componentGuard: `${edge.guard._tag}#${edge.guard.position}` }),
    }
  })
  return {
    machineVersion: "0.1",
    initialStateId: machine.initial._tag,
    fields: [],
    events: messageTags.map((tag) => {
      const payload = payloadOf(options.messages, tag)
      return { eventId: tag, source: { kind: "user", nodeId: "self" }, ...(payload.length > 0 ? { payload } : {}) }
    }),
    states: machine.stateTags.map((stateId) => ({ stateId, role: options.roles?.[stateId] ?? "idle" })),
    transitions,
  }
}
