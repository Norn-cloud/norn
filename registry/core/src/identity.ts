/**
 * Core identity and party closure definitions referenced by
 * `identity.BusinessUnit.entity.json` and `party.Brand.entity.json`.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Directory = Norn.interfaceType({
  name: "identity.Directory",
  title: "Directory",
  version: "0.1.0",
  description: "Named directory entry implemented by identity scope boundaries such as identity.BusinessUnit.",
  properties: Schema.Struct({
    name: Norn.ref("core.display_name"),
  }),
})

export const Organization = Norn.type({
  name: "party.Organization",
  title: "Organization",
  version: "0.1.0",
  description: "A legal entity or company owning brands and identity business units.",
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})
