// expect: NORN-FLOW-TYPE
// Unknown keys: definition metadata is a closed member set (rejected by the type checker first).
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
  color: "blue",
})
