import { Norn } from "@norn/authoring"
import { Schema } from "effect"

// INV-OVERDRAW / TXN-TWO: two deliveries of four cannot consume the same five.
export const Holding = Norn.type({
  name: "fixture.RaceHolding", title: "Finite stock at uniform original cost", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, quantity: Schema.Int, value: Schema.Int }),
})
export const Delivery = Norn.type({
  name: "fixture.RaceDelivery", title: "Competing delivery", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, status: Schema.Literals(["pending", "posted"]) }),
  links: [{ id: "holding", target: Holding, cardinality: { to: "one", from: "many" } }],
  lifecycles: [{ property: "status", initial: ["pending"], terminal: ["posted"], transitions: [{ id: "post", from: ["pending"], to: "posted" }] }],
})
export const Leg = Norn.type({
  name: "fixture.RaceLeg", title: "Original-cost book entry", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, delivery: Schema.String, account: Schema.Literals(["Inventory", "COGS"]), amount: Schema.Int, currency: Schema.Literal("USD") }),
})
const Input = Schema.Struct({ id: Schema.String, holding: Schema.String, quantity: Schema.Int })
const Values = Schema.Struct({ id: Schema.optionalKey(Schema.String), delivery: Schema.optionalKey(Schema.String), account: Schema.optionalKey(Schema.Literals(["Inventory", "COGS"])), amount: Schema.optionalKey(Schema.Int), currency: Schema.optionalKey(Schema.Literal("USD")), quantity: Schema.optionalKey(Schema.Int), value: Schema.optionalKey(Schema.Int) })
const Slice = Schema.Struct({ holding: Schema.Struct({ id: Schema.String, quantity: Schema.Int, value: Schema.Int }) })
export const Writer = Norn.decision({
  id: "example.delivery.writer", title: "Stock and books in one proposal", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: Slice, event: Schema.Struct({ input: Input, actor: Schema.String, operationId: Schema.String }) }),
  // The current closed-schema profile requires optional values/patch members.
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literals(["create", "patch"]), objectType: Schema.Literals(["fixture.RaceHolding", "fixture.RaceLeg"]), key: Schema.String, values: Schema.optionalKey(Values), patch: Schema.optionalKey(Values) })), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})) }),
}, input => ({
  facts: [
    { op: "patch", objectType: "fixture.RaceHolding", key: input.event.input.holding, patch: { quantity: input.slice.holding.quantity - input.event.input.quantity, value: input.slice.holding.value - input.event.input.quantity * 10 } },
    { op: "create", objectType: "fixture.RaceLeg", key: input.event.input.id + "-debit", values: { id: input.event.input.id + "-debit", delivery: input.event.input.id, account: "COGS", amount: input.event.input.quantity * 10, currency: "USD" } },
    { op: "create", objectType: "fixture.RaceLeg", key: input.event.input.id + "-credit", values: { id: input.event.input.id + "-credit", delivery: input.event.input.id, account: "Inventory", amount: -input.event.input.quantity * 10, currency: "USD" } },
  ], commands: [], timers: []
}))
export const Deliver = Norn.action({
  id: "example.delivery.post", title: "Deliver with original-cost books", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Input, output: Schema.Struct({ id: Schema.String }), synchronousRule: Writer,
  slice: [{ name: "holding", type: Holding, key: { property: { from: "input", property: "holding" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Slice, input: Input }), input => input.input.quantity > 0 && input.slice.holding.quantity >= input.input.quantity && input.slice.holding.value >= input.input.quantity * 10)],
  writes: [{ type: Holding, change: "created" }, { type: Holding, change: "modified" }, { type: Delivery, change: "modified" }, { type: Leg, change: "created" }, { type: Leg, change: "modified" }],
  createLinks: [{ link: "holding", targetType: Holding, targetIdField: "holding" }],
  effect: { kind: "fire_transition", type: Delivery, objectIdField: "id", lifecycle: "status", transition: "post" },
})
