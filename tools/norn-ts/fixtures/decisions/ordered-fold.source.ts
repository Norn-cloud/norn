import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Item = Schema.Struct({ id: Schema.String, amount: Schema.Finite })
const State = Schema.Struct({ total: Schema.Finite, count: Schema.Int, ids: Schema.Array(Schema.String) })
export const Step = Norn.decision({
  id: "example.fold.step", title: "Accumulate", version: "1.0.0", contract: "sub_decision",
  input: Schema.Struct({ accumulator: State, item: Item, index: Schema.Int }), output: State,
  scopeRequired: ["orgId", "environmentId"],
}, input => ({ total: input.accumulator.total + input.item.amount, count: input.accumulator.count + 1, ids: input.accumulator.ids.concat([input.item.id]) }))

export const Fold = Norn.decision({
  id: "example.fold.total", title: "Ordered total", version: "1.0.0", contract: "decide",
  input: Schema.Struct({ slice: Schema.Struct({ items: Schema.Array(Item), initial: State }), event: Schema.Struct({}) }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({})), commands: Schema.Array(Schema.Struct({ actionId: Schema.String, state: State })), timers: Schema.Array(Schema.Struct({})) }),
  scopeRequired: ["orgId", "environmentId"],
}, input => {
  const state = Norn.orderedFold(Step, { items: input.slice.items, initial: input.slice.initial }, {
    identity: "id", order: [{ property: "id", kind: "string" }], maxItems: 32, maxAccumulatorBytes: 4096,
    arithmetic: { scale: 3, rounding: "refuse" },
  })
  return { facts: [], commands: [{ actionId: "total", state }], timers: [] }
})
