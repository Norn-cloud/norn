// Intent: Norn.type / Norn.interfaceType -> EntityIR v2 object types and interfaces
// (T7-EFX-4, T7-TS-2): properties from the Schema subset plus the Norn value types
// (exact decimal, registry ref, timestamp, date); keys, literal defaults, lifecycles.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Priced = Norn.interfaceType({
  name: "shop.Priced",
  title: "Priced",
  version: "0.1.0",
  properties: Schema.Struct({ price: Norn.decimal(2), currency: Norn.ref("iso4217.currency_code") }),
})

export const Order = Norn.type({
  name: "shop.Order",
  title: "Order",
  version: "0.1.0",
  implements: [Priced],
  properties: Schema.Struct({
    order_id: Schema.String,
    price: Norn.decimal(2),
    currency: Norn.ref("iso4217.currency_code"),
    placed_at: Schema.NullOr(Norn.timestamp("utc")),
    due_on: Schema.NullOr(Norn.date()),
    status: Schema.Literals(["open", "closed"]),
  }),
  identity: [{ name: "primary", properties: ["order_id"] }],
  defaults: { price: 0.0, status: "open" },
  lifecycles: [{ property: "status", initial: ["open"], terminal: ["closed"], transitions: [{ id: "close", from: ["open"], to: "closed" }] }],
})
