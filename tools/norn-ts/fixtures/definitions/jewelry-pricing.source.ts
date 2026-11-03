import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// JEWEL-PRICE: the governed pricing step shared by sell and scrap exchanges.
const PricingInput = Schema.Struct({
  quote_id: Schema.String,
  side: Schema.String,
  currency: Schema.Literal("USD"),
  price_per_g: Norn.rate(6, { currency: "currency", perUnit: { system: "ucum", code: "g" } }),
  fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
  amount: Norn.decimal(2, { currency: "currency" }),
})

export const ExchangePrice = Norn.type({
  name: "jewelry.ExchangePrice",
  title: "Jewelry exchange price",
  version: "1.0.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: PricingInput,
  identity: [{ name: "primary", properties: ["quote_id"] }],
})

export const PriceExchange = Norn.action({
  id: "jewelry.exchange.price",
  title: "Price a jewelry exchange",
  version: "1.0.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: PricingInput,
  output: Schema.Struct({ quote_id: Schema.String }),
  derive: [{
    field: "amount",
    inputRequired: false,
    expression: Norn.expression(PricingInput, input => input.price_per_g * input.fine_g),
  }],
  preconditions: [Norn.expression(PricingInput, input => input.fine_g > 0)],
  writes: [{ type: ExchangePrice, change: "created" }],
  effect: { kind: "upsert_input_object", type: ExchangePrice, objectIdField: "quote_id", writeMode: { kind: "create_only" } },
})
