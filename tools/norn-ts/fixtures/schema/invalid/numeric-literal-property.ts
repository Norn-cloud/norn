// expect: NORN-FLOW-SCHEMA-PROJECTION
// Only string literals are enum values.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    level: Schema.Literals([1, 2, 3]),
  }),
})
