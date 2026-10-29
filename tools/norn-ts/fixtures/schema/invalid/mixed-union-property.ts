// expect: NORN-FLOW-SCHEMA-PROJECTION
// Unions other than literal unions and NullOr have no projection.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    code: Schema.Union([Schema.String, Schema.Int]),
  }),
})
