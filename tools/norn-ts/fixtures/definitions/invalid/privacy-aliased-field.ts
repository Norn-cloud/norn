import { Schema } from "effect"
import { Norn } from "@norn/authoring"
const propertyMarkings = { email: ["pii.contact"], undeclared: ["pii.identity"] }
export const Contact = Norn.type({
  name: "privacy.Contact", title: "Contact", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String }), propertyMarkings,
})
