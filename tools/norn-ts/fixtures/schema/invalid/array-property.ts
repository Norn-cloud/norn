// expect: NORN-FLOW-SCHEMA-PROJECTION
// EntityIR v2 properties are scalar; lists are to-many links.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    tags: Schema.Array(Schema.String),
  }),
})
