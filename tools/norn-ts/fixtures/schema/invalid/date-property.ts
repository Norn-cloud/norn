// expect: NORN-FLOW-SCHEMA-PROJECTION
// Date has no canonical codec in the profile.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    created: Schema.Date,
  }),
})
