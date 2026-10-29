// expect: NORN-FLOW-LITERAL
// Numeric defaults are plain decimal literals (exact text), never exponents.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const T = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({
    thing_id: Schema.String,
    total: Schema.Int,
  }),
  defaults: { total: 1e3 },
})
