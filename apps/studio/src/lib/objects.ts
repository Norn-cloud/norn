import type { Lifecycle } from "./catalog.ts"
import type { ObjectRecord } from "./transport.ts"

/** The record's own name when it declares one, else its identity. */
export const recordTitle = (record: ObjectRecord): string => {
  const label = record.values.label ?? record.values.name
  return typeof label === "string" && label !== "" ? label : record.id
}

/** A lifecycle's states in the order the declared transitions first reach them from the initial states. */
export function lifecycleStates(lifecycle: Lifecycle): readonly string[] {
  const order = [...lifecycle.initial]
  for (let index = 0; index < order.length; index += 1) {
    for (const transition of lifecycle.transitions ?? []) {
      if (transition.from.includes(order[index] ?? "") && !order.includes(transition.to)) order.push(transition.to)
    }
  }
  return order
}

/** The record's position in a lifecycle and the steps the definition declares from there. */
export function lifecycleOf(lifecycle: Lifecycle, record: ObjectRecord) {
  const value = record.values[lifecycle.property]
  const state = typeof value === "string" ? value : undefined
  return {
    states: lifecycleStates(lifecycle),
    state,
    terminal: state !== undefined && (lifecycle.terminal ?? []).includes(state),
    next: state === undefined ? [] : (lifecycle.transitions ?? []).filter(({ from }) => from.includes(state)),
  }
}
