import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Node = Norn.type({
  name: "tree.Node", title: "Node", version: "0.1.0",
  pathEquations: [{ id: "same_parent", left: ["parent"], right: ["parent"] }],
  properties: Schema.Struct({ name: Schema.String }),
  links: [
    { id: "parent", target: "tree.Node", cardinality: { to: "one", from: "many" }, acyclic: true },
    { id: "related", target: "tree.Node", cardinality: { to: "many", from: "many" }, acyclic: false },
  ],
})
