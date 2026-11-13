import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Product } from "./product.ts"

// A second type with a link, so the demo has a neighbourhood to follow.
export const OrderLine = Norn.type({
  name: "shop.OrderLine", title: "Order line", version: "0.1.0",
  properties: Schema.Struct({ line: Schema.String, sku: Schema.String, quantity: Norn.decimal(0) }),
  identity: [{ name: "primary", properties: ["line"] }],
  links: [{ id: "product", target: "shop.Product", cardinality: { to: "one", from: "many" } }],
})

export const AddOrderLine = Norn.action({
  id: "shop.orderline.add", title: "Add order line", version: "0.1.0",
  input: Schema.Struct({ line: Schema.String, sku: Schema.String, quantity: Norn.decimal(0) }),
  output: Schema.Struct({ line: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: OrderLine, change: "created" }],
  // The linked product is read, by its key, in the same commit.
  slice: [{ name: "product", type: Product, key: { property: { from: "input", property: "sku" } } }],
  createLinks: [{ link: "product", targetType: Product, targetIdField: "sku" }],
  effect: { kind: "upsert_input_object", type: OrderLine, objectIdField: "line", writeMode: { kind: "create_only" } },
})
