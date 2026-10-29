// expect: NORN-FLOW-SCHEMA-PROJECTION
// Schema.Number admits NaN/Infinity.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    amount: Schema.Number,
  }),
})
