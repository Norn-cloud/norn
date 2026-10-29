// expect: NORN-FLOW-SCHEMA-PROJECTION
// Refinements (checks) are rejected by the profile, never approximated.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    code: Schema.String.check(Schema.isMinLength(3)),
  }),
})
