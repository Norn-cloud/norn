import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Accumulation = Norn.type({ name: "example.Accumulation", title: "Accumulation", version: "1.0.0", properties: Schema.Struct({ id: Schema.String, total: Norn.decimal(3), count: Schema.Int }), identity: [{ name: "primary", properties: ["id"] }] })
const Item = Schema.Struct({ id: Schema.String, amount: Schema.Finite })
const State = Schema.Struct({ total: Schema.Finite, count: Schema.Int, ids: Schema.Array(Schema.String), segments: Schema.Array(Schema.Array(Schema.Finite)) })
export const Step = Norn.decision({
  id: "example.fold.step", title: "Accumulate", version: "1.0.0", contract: "sub_decision",
  input: Schema.Struct({ accumulator: State, item: Item, index: Schema.Int }), output: State,
  scopeRequired: ["orgId", "environmentId"],
}, input => ({ total: input.accumulator.total + input.item.amount, count: input.accumulator.count + 1, ids: input.accumulator.ids.concat([input.item.id]), segments: input.accumulator.segments.concat([[input.item.amount]]) }))

export const Fold = Norn.decision({
  id: "example.fold.total", title: "Ordered total", version: "1.0.0", contract: "decide",
  input: Schema.Struct({ slice: Schema.Struct({ items: Schema.Array(Item), initial: State }), event: Schema.Struct({ kind: Schema.String }) }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literal("delete"), objectType: Schema.Literal("example.Accumulation"), key: Schema.String })), commands: Schema.Array(Schema.Struct({ actionId: Schema.String, state: State })), timers: Schema.Array(Schema.Struct({})) }),
  scopeRequired: ["orgId", "environmentId"],
}, input => {
  const state = Norn.orderedFold(Step, { items: input.slice.items, initial: input.slice.initial }, {
    identity: "id", order: [{ property: "id", kind: "string" }], maxItems: 32, maxAccumulatorBytes: 4096,
    arithmetic: { scale: 3, rounding: "refuse" },
  })
  return { facts: [], commands: [{ actionId: "total", state }], timers: [] }
})
