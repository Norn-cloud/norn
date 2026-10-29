// expect: NORN-FLOW-LITERAL
// Missing vs null: a null default is refused (#2819); omit the default instead.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    note: Schema.NullOr(Schema.String),
  }),
  defaults: { note: null },
})
