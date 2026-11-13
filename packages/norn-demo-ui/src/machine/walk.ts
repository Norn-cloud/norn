// Model-based test generation (UI-MACHINE-1): one walk per declared transition, the shortest
// sequence of messages from the opened page that fires it. Browser suites replay each walk against
// a stand-in cell and assert the page shows the state the machine says it is in, so every
// transition of every page is exercised: the "verb matrix", derived instead of hand-written.
//
// The walk explores concrete snapshots with `update` and the checker's engine, so guards are
// honoured: a guarded transition is reached only through the field values that open it. Payloads
// come from the suite (`payloadFor`), one or more candidates per transition.

import type { Evaluate, Fields, Machine, MachineEvent, MachineTransition, Msg, Snapshot } from "./machine.ts"
import { init, update } from "./machine.ts"

export interface WalkStep {
  readonly transition: MachineTransition
  /** The message that fires it. Outcome and timer messages carry the entry the driver will be at. */
  readonly msg: Msg
}

export interface Walk {
  readonly transitionId: string
  readonly steps: readonly WalkStep[]
}

/** Candidate payloads for firing `transition` on `event`; none means an empty payload. */
export type PayloadFor = (transition: MachineTransition, event: MachineEvent, snapshot: Snapshot) => readonly Fields[] | undefined

const CATEGORIES = ["internal", "limit", "visibility", "conflict", "invalid", "permission", "precondition"] as const

/**
 * The refusal category a catch-all `refused` event is walked with: one that no event naming its
 * category handles in that state, so the catch-all is the transition that fires.
 */
function catchAllCategory(machine: Machine, from: string): (typeof CATEGORIES)[number] {
  const specific = new Set(
    machine.transitions
      .filter((transition) => transition.from === from)
      .flatMap((transition) => {
        const source = machine.events.find((event) => event.eventId === transition.on)?.source
        return source?.kind === "outcome" && source.outcome === "refused" && source.category ? [source.category] : []
      }),
  )
  return CATEGORIES.find((category) => !specific.has(category)) ?? "internal"
}

function messageFor(event: MachineEvent, entry: number, payload: Fields | undefined, catchAll: (typeof CATEGORIES)[number] = "precondition"): Msg {
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
        ...(event.source.category ? { category: event.source.category } : event.source.outcome === "refused" ? { category: catchAll } : {}),
        ...withPayload,
      }
  }
}

/**
 * One walk per transition the exploration reaches within `maxDepth` steps, in declaration order.
 * A transition with no walk is reported by its absence; suites assert the count.
 */
export function walks(machine: Machine, evaluate: Evaluate, payloadFor?: PayloadFor, maxDepth = 16): Walk[] {
  const eventOf = new Map(machine.events.map((event) => [event.eventId, event]))
  const found = new Map<string, WalkStep[]>()
  const key = (snapshot: Snapshot) => `${snapshot.stateId}\u0000${JSON.stringify(snapshot.fields)}`
  const seen = new Set<string>()
  const opened = init(machine).snapshot
  let frontier: { snapshot: Snapshot; steps: WalkStep[] }[] = [{ snapshot: opened, steps: [] }]
  seen.add(key(opened))
  for (let depth = 0; depth < maxDepth && frontier.length > 0 && found.size < machine.transitions.length; depth += 1) {
    const next: typeof frontier = []
    for (const { snapshot, steps } of frontier) {
      for (const transition of machine.transitions) {
        if (transition.from !== snapshot.stateId) continue
        const event = eventOf.get(transition.on)
        if (!event) throw new Error(`transition ${transition.transitionId} names undeclared event ${transition.on}`)
        for (const payload of payloadFor?.(transition, event, snapshot) ?? [undefined]) {
          const msg = messageFor(event, snapshot.entry, payload, catchAllCategory(machine, transition.from))
          let step
          try {
            step = update(machine, evaluate, snapshot, msg)
          } catch {
            continue
          }
          if (step.fired?.transitionId !== transition.transitionId) continue
          const path = [...steps, { transition, msg }]
          if (!found.has(transition.transitionId)) found.set(transition.transitionId, path)
          if (!seen.has(key(step.snapshot))) {
            seen.add(key(step.snapshot))
            next.push({ snapshot: step.snapshot, steps: path })
          }
        }
      }
    }
    frontier = next
  }
  return machine.transitions.flatMap((transition) => {
    const steps = found.get(transition.transitionId)
    return steps ? [{ transitionId: transition.transitionId, steps }] : []
  })
}
