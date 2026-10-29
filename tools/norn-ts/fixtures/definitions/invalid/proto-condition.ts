// A literal member named __proto__ would silently vanish from the lowered data; it is refused.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Thing = Norn.type({
  name: "demo.Thing",
  title: "Thing",
  version: "0.1.0",
  properties: Schema.Struct({ thing_id: Schema.String }),
})

const condition = { role: { anyOf: ["Staff"] }, "__proto__": null }

export const ThingAccess = Norn.access({
  id: "demo.thing.access",
  title: "Thing access",
  version: "0.1.0",
  rules: [{ ruleId: "staff", actions: ["norn.read"], resourceType: Thing, scope: { dimensions: ["org"] }, allOf: [condition] }],
})
