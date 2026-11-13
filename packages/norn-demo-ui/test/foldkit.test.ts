// A real Foldkit component (foldkit 0.170.0, effect 4.1.0): its `Machine.define` declaration is
// compiled into Norn's machine IR, and the host runs the compiled machine against the component's
// own reports. The machine is written once, in the component.

import { expect, test } from "bun:test"
import { Schema } from "effect"
import * as Machine from "foldkit/experimental/machine"
import { defineMessageUnion } from "foldkit/message"
import { defineTaggedUnion } from "foldkit/schema"

import { type CatalogEntry, type Violation, createHost } from "../src/catalog-host/host.ts"
import { type FoldkitVariant, foldkitMessages, machineFromFoldkit } from "../src/catalog-host/foldkit.ts"

const TileState = defineTaggedUnion({
  Collapsed: {},
  Expanded: { period: Schema.String },
  Pinned: {},
})
const TileMessage = defineMessageUnion({
  ClickedExpand: {},
  ClickedCollapse: {},
  ChoseCompare: { period: Schema.String },
  ClickedPin: {},
})

// Expanding is open to everyone; pinning is decided by the component's own rule (a `when` edge).
const tile = Machine.define({ state: TileState, message: TileMessage })({
  initial: TileState.Collapsed(),
  states: {
    Collapsed: {
      on: {
        ClickedExpand: Machine.to("Expanded", () => ({ model: TileState.Expanded({ period: "week" }) })),
      },
    },
    Expanded: {
      on: {
        ChoseCompare: Machine.to("Expanded", ({ message }) => ({ model: TileState.Expanded({ period: message.period }) })),
        ClickedCollapse: Machine.to("Collapsed", () => ({ model: TileState.Collapsed() })),
        ClickedPin: [
          Machine.when((state) => state.period === "week", "Pinned", () => ({ model: TileState.Pinned() })),
          Machine.otherwise(Machine.to("Expanded", ({ state }) => ({ model: state }))),
        ],
      },
    },
    Pinned: {
      on: {
        ClickedPin: Machine.to("Expanded", () => ({ model: TileState.Expanded({ period: "week" }) })),
      },
    },
  },
})

// A concrete variant is a `FoldkitVariant` with no cast.
TileMessage.ChoseCompare satisfies FoldkitVariant

const compiled = machineFromFoldkit(tile, { messages: foldkitMessages(TileMessage) })

test("the component's own declaration compiles: states, events with payloads, edges in order, component guards", () => {
  expect(compiled.initialStateId).toBe("Collapsed")
  expect(compiled.states.map((state) => state.stateId).sort()).toEqual(["Collapsed", "Expanded", "Pinned"])
  expect(compiled.events.find((event) => event.eventId === "ChoseCompare")?.payload).toEqual([{ fieldId: "period", type: "text", required: true }])
  const pin = compiled.transitions.filter((transition) => transition.from === "Expanded" && transition.on === "ClickedPin")
  expect(pin.map((transition) => [transition.target, transition.componentGuard])).toEqual([
    ["Pinned", "When#0"],
    ["Expanded", "Otherwise#1"],
  ])
  expect(compiled.transitions.find((transition) => transition.from === "Collapsed")?.componentGuard).toBeUndefined()
})

test("the host runs the compiled machine: guarded edges need the target the component reports, and only a declared one", () => {
  const entry: CatalogEntry = {
    entryId: "kpi-tile",
    trust: "contract_checked",
    framework: "foldkit",
    events: [{ eventId: "ChoseCompare", payload: [{ fieldId: "period", type: "text", required: true }] }],
    machine: compiled,
  }
  const refused: Violation[] = []
  const states: string[] = []
  const emitted: string[] = []
  const host = createHost({
    entry,
    evaluate: () => {
      throw new Error("no ZEN in a compiled Foldkit machine")
    },
    onEmit: (id) => emitted.push(id),
    onViolation: (violation) => refused.push(violation),
    onView: (view) => states.push(view.state ?? "-"),
  })
  // The component runs its own `transition` and reports where it went.
  let model = tile.initial
  const send = (message: typeof TileMessage.Type, payload: Record<string, unknown> = {}) => {
    const next = tile.transition(model, message)
    model = next.model
    host.receive({ type: "emit", eventId: message._tag, payload, to: model._tag })
  }
  send(TileMessage.ClickedExpand())
  send(TileMessage.ChoseCompare({ period: "month" }), { period: "month" })
  send(TileMessage.ClickedPin())
  expect(states).toEqual(["Expanded", "Expanded", "Expanded"])
  expect(emitted).toEqual(["ChoseCompare"])
  // A component that claims a target its contract does not declare is refused.
  host.receive({ type: "emit", eventId: "ClickedPin", to: "Collapsed" })
  expect(refused).toEqual([{ kind: "not_offered", eventId: "ClickedPin", state: "Expanded" }])

  // The boundary is the runtime check, whatever the sender's static types say: an event the contract
  // does not declare, a mistyped payload and a payload with an extra member are all refused, and none
  // of them moves the machine or reaches the page.
  const before = { states: states.length, emitted: emitted.length }
  const raw = host.receive as (message: unknown) => void
  raw({ type: "emit", eventId: "ClickedExplode", payload: {} })
  raw({ type: "emit", eventId: "ChoseCompare", payload: { period: 7 }, to: "Expanded" })
  raw({ type: "emit", eventId: "ChoseCompare", payload: { period: "day", admin: true }, to: "Expanded" })
  expect(refused.slice(1).map((violation) => [violation.kind, "eventId" in violation ? violation.eventId : null])).toEqual([
    ["undeclared_event", "ClickedExplode"],
    ["ill_typed_payload", "ChoseCompare"],
    ["ill_typed_payload", "ChoseCompare"],
  ])
  expect({ states: states.length, emitted: emitted.length }).toEqual(before)
})

test("a union's helpers are never read as messages: only a variant whose own tag matches resolves", () => {
  const messages = foldkitMessages(TileMessage)
  expect(messages("ChoseCompare")?.fields.period).toBeDefined()
  for (const helper of ["match", "ast", "members", "toString", "constructor", "Nope"]) expect(messages(helper)).toBeUndefined()
})
