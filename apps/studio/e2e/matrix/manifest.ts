import type { Evaluate, Fields, Machine, MachineEvent, MachineTransition, Snapshot, Walk } from "../../../../packages/norn-react/src/machine/index.ts"
import { walks } from "../../../../packages/norn-react/src/machine/index.ts"
import type { WidgetNode, WidgetUx } from "../../../../packages/norn-react/src/index.ts"

/**
 * The state matrix of one admitted page (#1762 milestone C), generated from its Widget UX manifest
 * and `walk.ts`, never written by hand: one walk per declared transition (the shortest message path
 * that fires it), the shortest walk into each state, and each pending state's outcomes. A
 * transition or state the document adds gets its rows with no new test code.
 */
export interface Matrix {
  readonly walks: readonly Walk[]
  /** For each reachable state, the walk that enters it in the fewest steps. */
  readonly states: ReadonlyMap<string, Walk>
  /** Every (pending state, outcome) pair the machine handles. */
  readonly outcomes: readonly { readonly state: string; readonly outcome: string; readonly transitionId: string }[]
  /** Declared states no walk reaches (the suite asserts there are none). */
  readonly unreached: readonly string[]
}

export function matrixOf(machine: Machine, evaluate: Evaluate, payloadFor?: (transition: MachineTransition, event: MachineEvent, snapshot: Snapshot) => readonly Fields[] | undefined): Matrix {
  const all = walks(machine, evaluate, payloadFor)
  const states = new Map<string, Walk>([[machine.initialStateId, { transitionId: "(opened)", steps: [] }]])
  for (const walk of [...all].sort((a, b) => a.steps.length - b.steps.length)) {
    const target = walk.steps.at(-1)?.transition.target
    if (target !== undefined && !states.has(target)) states.set(target, walk)
  }
  const outcomes = machine.transitions.flatMap((transition) => {
    const source = machine.events.find((event) => event.eventId === transition.on)?.source
    return source?.kind === "outcome" ? [{ state: transition.from, outcome: source.outcome, transitionId: transition.transitionId }] : []
  })
  return { walks: all, states, outcomes, unreached: machine.states.map((state) => state.stateId).filter((state) => !states.has(state)) }
}

/** Every node of a page tree, by id. */
export function nodesById(ux: WidgetUx): ReadonlyMap<string, WidgetNode> {
  const out = new Map<string, WidgetNode>()
  const visit = (node: WidgetNode) => {
    out.set(node.nodeId, node)
    node.children.forEach(visit)
  }
  ux.pages.forEach((page) => visit(page.root))
  return out
}
