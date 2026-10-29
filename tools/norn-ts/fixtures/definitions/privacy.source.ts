import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Catalog = Norn.markings({
  id: "privacy.markings.entity", title: "Privacy", version: "0.1.0",
  markings: [
    { id: "pii.contact", label: "Contact", severity: "high", category: "pii.contact", mask: { kind: "redacted" } },
    { id: "pii.identity", label: "Identity", severity: "critical", category: "pii.identity", mask: { kind: "null" } },
  ],
})
export const Person = Norn.interfaceType({
  name: "privacy.Person", title: "Person", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String }),
  markings: ["pii.identity"], propertyMarkings: { email: ["pii.contact"] },
})
export const Contact = Norn.type({
  name: "privacy.Contact", title: "Contact", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String }), implements: [Person],
  markings: ["pii.identity"], propertyMarkings: { email: ["pii.contact"] },
})
