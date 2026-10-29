import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Invalid = Norn.interfaceType({
  name: "commerce.Invalid",
  title: "Invalid",
  version: "0.1.0",
  properties: Schema.Struct({ total: Schema.Int }),
  invariants: [{
    id: "positive",
    classification: "",
    expression: Norn.unsafeZen<boolean>("self.total >= 0"),
    message: "total must be non-negative",
  }],
})
