import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "demo.Customer", title: "Customer", version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String }), identity: [{ name: "primary", properties: ["code"] }],
})
export const Order = Norn.type({
  name: "demo.Order", title: "Order", version: "0.1.0",
  properties: Schema.Struct({ number: Schema.String, customer_code: Schema.String }), identity: [{ name: "primary", properties: ["number"] }],
  links: [{ id: "customer", target: Customer, cardinality: { to: "one", from: "many", required: true } }],
})
export const CreateOrder = Norn.action({
  id: "demo.order.create", title: "Create linked order", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ number: Schema.String, customer_code: Schema.String }), output: Schema.Struct({ number: Schema.String }),
  slice: [{ name: "customer", type: Customer, key: { property: { from: "input", property: "customer_code" } } }],
  createLinks: [{ link: "customer", targetType: Customer, targetIdField: "customer_code" }],
  writes: [{ type: Order, change: "created" }],
  effect: { kind: "upsert_input_object", type: Order, objectIdField: "number", writeMode: { kind: "create_only" } },
})
