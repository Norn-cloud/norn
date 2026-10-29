// implements lists interfaces; an object type there is NORN-FLOW-VALUE.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Base = Norn.type({
  name: "demo.Base",
  title: "Base",
  version: "0.1.0",
  properties: Schema.Struct({ base_id: Schema.String }),
})

export const Derived = Norn.type({
  name: "demo.Derived",
  title: "Derived",
  version: "0.1.0",
  // @ts-expect-error -- an object type is not an interface definition
  implements: [Base],
  properties: Schema.Struct({ base_id: Schema.String }),
})
