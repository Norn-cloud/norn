import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Entirely fictional observations; no vendor schema or business records.
export const Demand = Norn.type({
  name: "demo.Demand", title: "Synthetic demand", version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String, stock: Schema.Int, units: Schema.Int,
    weight: Norn.decimal(2, { unit: { system: "ucum", code: "kg" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})
