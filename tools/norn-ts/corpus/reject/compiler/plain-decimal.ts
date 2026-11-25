import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Item = Schema.Struct({ id: Schema.String })
const State = Schema.Struct({ amount: Norn.decimal(2) })
export const Step = Norn.decision({
  id: "probe.step", title: "Step", version: "1", contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ accumulator: State, item: Item, index: Schema.Int }), output: State,
}, input => ({ amount: input.accumulator.amount }))

export const Caller = Norn.decision({
  id: "probe.caller", title: "Caller", version: "1", contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ items: Schema.Array(Item), amount: Schema.Finite }), output: Schema.Struct({ ok: Schema.Boolean }),
}, input => {
  const state = Norn.orderedFold(Step, { items: input.items, initial: { amount: input.amount } }, {
    identity: "id", order: [{ property: "id", kind: "string" }],
    maxItems: 3, maxAccumulatorBytes: 4096, arithmetic: { scale: 2, rounding: "refuse" },
  })
  return { ok: state.amount > 0 }
})
