// expect: NORN-FLOW-LITERAL
// A decimal scale is an integer literal between 0 and 28.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    amount: Norn.decimal(29),
  }),
})
