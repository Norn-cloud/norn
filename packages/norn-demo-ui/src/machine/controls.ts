// What a renderer may offer, read from the admitted machine (UI-MACHINE-1): a control is enabled
// exactly when the current state has a transition on one of its events whose guard holds. No
// renderer decides on its own which buttons exist; the declared machine does, and the server
// decides what any call is allowed to do.

import type { Evaluate, Fields, Machine, MachineEvent, Snapshot } from "./machine.ts"

/** The user events a node raises, in declaration order. */
export function eventsOf(machine: Machine, nodeId: string): MachineEvent[] {
  return machine.events.filter((event) => event.source.kind === "user" && event.source.nodeId === nodeId)
}

/**
 * Whether the node can do anything now. A guard that reads the event's payload cannot be judged
 * before the person acts, so it counts as possibly holding; a guard over the fields alone is
 * evaluated with the checker's engine.
 */
export function enabled(machine: Machine, evaluate: Evaluate, snapshot: Snapshot, nodeId: string, payload: Fields = {}): boolean {
  const events = new Set(eventsOf(machine, nodeId).map((event) => event.eventId))
  return machine.transitions.some((transition) => {
    if (transition.from !== snapshot.stateId || !events.has(transition.on)) return false
    if (transition.guard === undefined) return true
    if (transition.guard.includes("input.event")) return true
    return evaluate(transition.guard, { fields: snapshot.fields, event: payload }) === true
  })
}
