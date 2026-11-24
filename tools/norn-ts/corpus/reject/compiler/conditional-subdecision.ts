import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Shape = Schema.Struct({ flag: Schema.Boolean })
export const Child = Norn.decision({
  id: "probe.child", title: "Child", version: "1", contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"], input: Shape, output: Shape,
}, input => ({ flag: input.flag }))

export const Parent = Norn.decision({
  id: "probe.parent", title: "Parent", version: "1", contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"], input: Shape, output: Shape,
}, input => {
  if (input.flag) return { flag: true }
  const value = Norn.evaluate(Child, input)
  return { flag: value.flag }
})
