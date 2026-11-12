// Model-based test generation (UI-MACHINE-1): one walk per declared transition, the shortest
// sequence of messages from the opened page that fires it. Browser suites replay each walk against
// a stand-in cell and assert the page shows the state the machine says it is in, so every
// transition of every page is exercised: the "verb matrix", derived instead of hand-written.
//
// Guards are assumed satisfiable here (as the checker's reachability does); a walk whose guard
// needs a particular payload gets it from the suite's `payloadFor`.

import type { Machine, MachineEvent, MachineTransition, Msg } from "./machine.ts"

export interface WalkStep {
  readonly transition: MachineTransition
  /** The message that fires it. Outcome and timer messages carry the entry the driver will be at. */
  readonly msg: Msg
}

export interface Walk {
  readonly transitionId: string
  readonly steps: readonly WalkStep[]
}

export type PayloadFor = (transition: MachineTransition, event: MachineEvent) => Readonly<Record<string, unknown>> | undefined

function messageFor(event: MachineEvent, transition: MachineTransition, entry: number, payloadFor?: PayloadFor): Msg {
  const payload = payloadFor?.(transition, event)
  const withPayload = payload ? { payload } : {}
  switch (event.source.kind) {
    case "page_opened":
      return { kind: "page_opened" }
    case "user":
      return { kind: "user", eventId: event.eventId, ...withPayload }
    case "subscription":
      return { kind: "subscription", bindingId: event.source.bindingId, ...withPayload }
    case "timer":
      return { kind: "timer", entry, eventId: event.eventId }
    case "outcome":
      return {
        kind: "outcome",
        entry,
        outcome: event.source.outcome,
        ...(event.source.category ? { category: event.source.category } : event.source.outcome === "refused" ? { category: "precondition" as const } : {}),
        ...withPayload,
      }
  }
}

/** One walk per transition reachable from the initial state, in declaration order. */
export function walks(machine: Machine, payloadFor?: PayloadFor): Walk[] {
  const eventOf = new Map(machine.events.map((event) => [event.eventId, event]))
  // Breadth-first over states, recording the transitions that first reach each state.
  const pathTo = new Map<string, MachineTransition[]>([[machine.initialStateId, []]])
  const queue = [machine.initialStateId]
  while (queue.length > 0) {
    const state = queue.shift() as string
    const path = pathTo.get(state) ?? []
    for (const transition of machine.transitions) {
      if (transition.from !== state || pathTo.has(transition.target)) continue
      pathTo.set(transition.target, [...path, transition])
      queue.push(transition.target)
    }
  }
  const result: Walk[] = []
  for (const transition of machine.transitions) {
    const prefix = pathTo.get(transition.from)
    if (!prefix) continue
    const steps: WalkStep[] = []
    let entry = 0
    let state = machine.initialStateId
    for (const step of [...prefix, transition]) {
      const event = eventOf.get(step.on)
      if (!event) throw new Error(`transition ${step.transitionId} names undeclared event ${step.on}`)
      steps.push({ transition: step, msg: messageFor(event, step, entry, payloadFor) })
      if (step.target !== state || event.source.kind === "outcome") entry += 1
      state = step.target
    }
    result.push({ transitionId: transition.transitionId, steps })
  }
  return result
}
