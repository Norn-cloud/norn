import { Norn } from "@norn/authoring"
import { Schema } from "effect"

export const Product = Norn.type({
  name: "__NAMESPACE__.Product", title: "Product", version: "0.1.0",
  properties: Schema.Struct({ sku: Schema.String, name: Schema.String, price: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["sku"] }],
})

export const AddProduct = Norn.action({
  id: "__NAMESPACE__.product.add", title: "Add product", version: "0.1.0",
  input: Schema.Struct({ sku: Schema.String, name: Schema.String, price: Norn.decimal(2) }),
  output: Schema.Struct({ sku: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Product, change: "created" }],
  effect: { kind: "upsert_input_object", type: Product, objectIdField: "sku", writeMode: { kind: "create_only" } },
})
