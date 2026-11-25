import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Match = Norn.decision({
  id: "probe.match", title: "Match", version: "1", contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ values: Schema.Array(Schema.Int), b0: Schema.Int, price: Schema.Int }),
  output: Schema.Struct({ found: Schema.Boolean }),
}, root => ({ found: root.values.some(price => root.values.every(item => item > price && price > root.price)) }))
