// expect: NORN-FLOW-SCHEMA-PROJECTION
// Missing vs null: EntityIR has one absence policy (nullable); optionalKey is not approximated.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    note: Schema.optionalKey(Schema.String),
  }),
})
