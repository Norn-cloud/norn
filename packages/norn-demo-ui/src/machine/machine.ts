// The pure core of a Norn page machine (UI-MACHINE-1).
//
// `update(machine, evaluate, snapshot, msg)` is the only way client state changes: it fires at most
// one declared transition, applies its typed `assign`, and returns the commands its effects ask
// for. It never fetches, never times, never decides permission; the driver runs the commands and
// the server decides every call. The machine it runs is the admitted `WidgetPage.machine` document,
// unchanged: what the checker proved is what runs.

export type ValueType = "text" | "integer" | "number" | "boolean" | "date" | "timestamp" | "duration" | "json"

export interface MachineField {
  readonly fieldId: string
  readonly type: ValueType
  readonly required: boolean
  readonly initial?: unknown
}

export type CallOutcome =
  | "accepted"
  | "refused"
  | "pending_approval"
  | "answer_moved"
  | "network_failure"
  | "timeout"
  | "cancelled"

export type RefusalCategory = "visibility" | "permission" | "precondition" | "conflict" | "invalid" | "limit" | "internal"

export type EventSource =
  | { readonly kind: "page_opened" }
  | { readonly kind: "user"; readonly nodeId: string; readonly emits?: string }
  | { readonly kind: "outcome"; readonly outcome: CallOutcome; readonly category?: RefusalCategory }
  | { readonly kind: "subscription"; readonly bindingId: string }
  | { readonly kind: "timer"; readonly afterMs: number }

export interface MachineEvent {
  readonly eventId: string
  readonly source: EventSource
  readonly payload?: readonly MachineField[]
}

export type StateRole = "idle" | "pending" | "error" | "terminal"

export type MachineCall =
  | { readonly kind: "action"; readonly bindingId: string }
  | { readonly kind: "read"; readonly bindingId: string }
  | { readonly kind: "operation"; readonly operationId: string }

export interface MachineState {
  readonly stateId: string
  readonly role: StateRole
  readonly awaits?: MachineCall
  readonly deadlineMs?: number
}

export type MachineEffect =
  | { readonly kind: "start"; readonly call: MachineCall; readonly input?: Readonly<Record<string, string>> }
  | { readonly kind: "cancel" }
  | { readonly kind: "navigate"; readonly itemId: string }

export interface MachineTransition {
  readonly transitionId: string
  readonly from: string
  readonly on: string
  readonly target: string
  readonly guard?: string
  /** A guard the catalog component's own code decides: the component reports the target it took. */
  readonly componentGuard?: string
  readonly assign?: Readonly<Record<string, string>>
  readonly effects?: readonly MachineEffect[]
}

export interface Machine {
  readonly machineVersion: "0.1"
  readonly initialStateId: string
  readonly fields: readonly MachineField[]
  readonly events: readonly MachineEvent[]
  readonly states: readonly MachineState[]
  readonly transitions: readonly MachineTransition[]
  /**
   * The backend lifecycle the page acts on (UI-LIFECYCLE-1). The checker composes the two; the
   * runtime never reads it, since the server decides every lifecycle move.
   */
  readonly lifecycle?: MachineLifecycle
}

export interface MachineLifecycle {
  readonly objectType: string
  readonly property: string
  readonly syncs?: readonly { readonly transitionId: string; readonly lifecycleTransition: string }[]
  readonly assumes?: readonly { readonly stateId: string; readonly lifecycleStates: readonly string[] }[]
}

/** Evaluates one ZEN expression over `input.fields` and `input.event` (the checker's own engine). */
export type Evaluate = (expression: string, input: { readonly fields: Fields; readonly event: Fields }) => unknown

export type Fields = Readonly<Record<string, unknown>>

/**
 * Where the page is. `entry` counts state entries: commands and the messages they produce carry the
 * entry they belong to, so an outcome or timer from a state the page has left is dropped.
 */
export interface Snapshot {
  readonly stateId: string
  readonly fields: Fields
  readonly entry: number
}

export type Msg =
  | { readonly kind: "page_opened" }
  | { readonly kind: "user"; readonly eventId: string; readonly payload?: Fields; readonly to?: string }
  | { readonly kind: "subscription"; readonly bindingId: string; readonly payload?: Fields }
  | {
      readonly kind: "outcome"
      readonly entry: number
      readonly outcome: CallOutcome
      readonly category?: RefusalCategory
      readonly payload?: Fields
    }
  | { readonly kind: "timer"; readonly entry: number; readonly eventId: string }

export type Cmd =
  | { readonly kind: "start"; readonly entry: number; readonly call: MachineCall; readonly input: Fields; readonly deadlineMs: number }
  | { readonly kind: "cancel"; readonly entry: number }
  | { readonly kind: "arm"; readonly entry: number; readonly eventId: string; readonly afterMs: number }
  | { readonly kind: "navigate"; readonly itemId: string }

export interface Step {
  readonly snapshot: Snapshot
  readonly cmds: readonly Cmd[]
  /** The transition that fired, or null when the message matched none (it is then ignored). */
  readonly fired: MachineTransition | null
}

export class MachineError extends Error {}

function stateOf(machine: Machine, stateId: string): MachineState {
  const state = machine.states.find((candidate) => candidate.stateId === stateId)
  if (!state) throw new MachineError(`state ${stateId} is not declared`)
  return state
}

/** Timers the state arms on entry: every timer event a transition out of it waits on. */
function armsOnEntry(machine: Machine, stateId: string, entry: number): Cmd[] {
  const armed = new Set<string>()
  const cmds: Cmd[] = []
  for (const transition of machine.transitions) {
    if (transition.from !== stateId || armed.has(transition.on)) continue
    const event = machine.events.find((candidate) => candidate.eventId === transition.on)
    if (event?.source.kind !== "timer") continue
    armed.add(transition.on)
    cmds.push({ kind: "arm", entry, eventId: event.eventId, afterMs: event.source.afterMs })
  }
  return cmds
}

/** The page as it opens: the initial state, initial field values, its timers. */
export function init(machine: Machine): Step {
  const fields: Record<string, unknown> = {}
  for (const field of machine.fields) fields[field.fieldId] = field.initial ?? null
  const snapshot = { stateId: machine.initialStateId, fields, entry: 0 }
  stateOf(machine, snapshot.stateId)
  return { snapshot, cmds: armsOnEntry(machine, snapshot.stateId, 0), fired: null }
}

/** Events a message can be: matched by source, in declaration order. */
function candidateEvents(machine: Machine, snapshot: Snapshot, msg: Msg): MachineEvent[] {
  return machine.events.filter((event) => {
    const source = event.source
    switch (msg.kind) {
      case "page_opened":
        return source.kind === "page_opened"
      case "user":
        return source.kind === "user" && event.eventId === msg.eventId
      case "subscription":
        return source.kind === "subscription" && source.bindingId === msg.bindingId
      case "timer":
        return source.kind === "timer" && event.eventId === msg.eventId && msg.entry === snapshot.entry
      case "outcome":
        return (
          source.kind === "outcome" &&
          msg.entry === snapshot.entry &&
          source.outcome === msg.outcome &&
          (source.category === undefined || source.category === msg.category)
        )
    }
  })
}

function truthy(value: unknown, where: string): boolean {
  if (typeof value !== "boolean") throw new MachineError(`${where} did not evaluate to a Boolean`)
  return value
}

/**
 * Fire the first declared transition out of the current state whose event matches `msg` and whose
 * guard holds. Specific refusal categories win over the catch-all, then declaration order.
 */
export function update(machine: Machine, evaluate: Evaluate, snapshot: Snapshot, msg: Msg): Step {
  const payload: Fields = "payload" in msg && msg.payload ? msg.payload : {}
  const events = candidateEvents(machine, snapshot, msg).sort((left, right) => {
    const rank = (event: MachineEvent) => (event.source.kind === "outcome" && event.source.category ? 0 : 1)
    return rank(left) - rank(right)
  })
  for (const event of events) {
    for (const transition of machine.transitions) {
      if (transition.from !== snapshot.stateId || transition.on !== event.eventId) continue
      // A component-decided guard: only the declared target the component reports fires.
      if (transition.componentGuard !== undefined && (msg.kind !== "user" || msg.to !== transition.target)) continue
      const input = { fields: snapshot.fields, event: payload }
      if (transition.guard !== undefined && !truthy(evaluate(transition.guard, input), `guard of ${transition.transitionId}`)) continue
      return fire(machine, evaluate, snapshot, transition, input)
    }
  }
  return { snapshot, cmds: [], fired: null }
}

function fire(
  machine: Machine,
  evaluate: Evaluate,
  snapshot: Snapshot,
  transition: MachineTransition,
  input: { readonly fields: Fields; readonly event: Fields },
): Step {
  const fields: Record<string, unknown> = { ...snapshot.fields }
  for (const [field, expression] of Object.entries(transition.assign ?? {})) {
    fields[field] = evaluate(expression, input)
  }
  const target = stateOf(machine, transition.target)
  const event = machine.events.find((candidate) => candidate.eventId === transition.on)
  // Entering a state: any move to another state, or an outcome re-entering a pending one.
  const enters = transition.target !== snapshot.stateId || event?.source.kind === "outcome"
  const entry = enters ? snapshot.entry + 1 : snapshot.entry
  const cmds: Cmd[] = []
  for (const effect of transition.effects ?? []) {
    switch (effect.kind) {
      case "start": {
        const body: Record<string, unknown> = {}
        for (const [member, expression] of Object.entries(effect.input ?? {})) body[member] = evaluate(expression, input)
        cmds.push({ kind: "start", entry, call: effect.call, input: body, deadlineMs: target.deadlineMs ?? 0 })
        break
      }
      case "cancel":
        cmds.push({ kind: "cancel", entry: snapshot.entry })
        break
      case "navigate":
        cmds.push({ kind: "navigate", itemId: effect.itemId })
        break
    }
  }
  if (enters) cmds.push(...armsOnEntry(machine, target.stateId, entry))
  return { snapshot: { stateId: target.stateId, fields, entry }, cmds, fired: transition }
}
