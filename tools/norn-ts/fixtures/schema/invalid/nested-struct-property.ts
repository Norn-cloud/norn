// expect: NORN-FLOW-SCHEMA-PROJECTION
// EntityIR v2 properties are scalar; nested records are their own types.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    address: Schema.Struct({ city: Schema.String }),
  }),
})
