// expect: NORN-FLOW-SCHEMA-PROJECTION
// Discriminated unions have no qualified projection in v1.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    event: Schema.TaggedUnion({ Opened: { at: Schema.String }, Closed: { by: Schema.String } }),
  }),
})
