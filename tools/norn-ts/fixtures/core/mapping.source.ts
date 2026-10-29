import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Row = Norn.type({ name: "toy.Row", title: "Row", version: "1.0.0", properties: Schema.Struct({ id: Schema.String }), identity: [{ name: "primary", properties: ["id"] }] })
const Identified = Norn.interfaceType({ name: "toy.Identified", title: "Identified", version: "1.0.0", properties: Schema.Struct({ id: Schema.String }) })
export const Map = Norn.mapping({
  id: "toy.rows", title: "Synthetic row mapping", version: "1.0.0",
  source: { schemaId: "toy.schema", version: "1.0.0", sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
  sourceContract: { kind: "generic" }, interfaces: [Identified],
  maps: [{ id: "row", source: { dataset: "Rows" }, target: { type: Row, interface: Identified }, subject: "input.id", sourceRecord: "input.id" }],
})
