// expect: NORN-FLOW-TYPE
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Node = Norn.type({
  name: "tree.Node", title: "Node", version: "0.1.0",
  properties: Schema.Struct({ name: Schema.String }),
  links: [{ id: "parent", target: "tree.Node", cardinality: { to: "one", from: "many" }, acyclic: "yes" }],
})
