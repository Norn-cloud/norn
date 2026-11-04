import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Identified = Norn.interfaceType({
  name: "demo.LineageRow", title: "Mapped scrap source row", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String }),
})
export const Conversion = Norn.type({
  name: "demo.Conversion", title: "Synthetic scrap conversion", version: "0.1.0",
  implements: [Identified],
  dependencies: [{ artifactId: "core.units", kind: "entity", version: "1.0.0" }],
  properties: Schema.Struct({
    id: Schema.String,
    input: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    recovered: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    occurred_at: Norn.timestamp("utc"), metal: Schema.String,
  }), identity: [{ name: "primary", properties: ["id"] }],
})
export const Source = Norn.sourceSchema({
  id: "demo.scrap.source", title: "Synthetic scrap feed", version: "0.1.0",
  source: { system: "synthetic-erpnext", instance: "demo" }, observedAt: "2026-10-26T00:00:00Z",
  adapter: { name: "synthetic-scrap", version: "0.1.0" },
  rawMetadata: { uri: "evidence://synthetic/scrap-yield", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  datasets: [{ id: "conversions", nativeName: "conversions", kind: "table" }],
  fields: [
    { dataset: "conversions", name: "id", ordinal: 0, nativeType: "text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "conversions", name: "source_row", ordinal: 1, nativeType: "text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "conversions", name: "input", ordinal: 2, nativeType: "decimal grams", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "conversions", name: "recovered", ordinal: 3, nativeType: "decimal grams", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "conversions", name: "loss", ordinal: 4, nativeType: "decimal grams", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "conversions", name: "occurred_at", ordinal: 5, nativeType: "datetime", typeCandidate: { type: "datetime", origin: "declared" }, required: true },
    { dataset: "conversions", name: "metal", ordinal: 6, nativeType: "text", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
})
const Input = Schema.Struct({
  id: Schema.String, source_row: Schema.String,
  input: Schema.Finite, recovered: Schema.Finite, loss: Schema.Finite,
  occurred_at: Schema.String, metal: Schema.String,
})
export const Mapping = Norn.mapping({
  id: "demo.scrap.mapping", title: "Pinned scrap source mapping", version: "0.1.0",
  // The test binds this pin to the compiled source's actual canonical digest.
  source: { schemaId: "demo.scrap.source", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  sourceContract: { kind: "generic" }, interfaces: [Identified],
  maps: [{ id: "conversion", source: { dataset: "conversions" }, target: { type: Conversion, interface: Identified },
    subject: Norn.expression(Input, row => row.id), sourceRecord: Norn.expression(Input, row => row.source_row),
    properties: [
      { property: "id", value: Norn.expression(Input, row => row.id) },
      { property: "input", value: Norn.expression(Input, row => row.input) },
      { property: "recovered", value: Norn.expression(Input, row => row.recovered) },
      { property: "loss", value: Norn.expression(Input, row => row.loss) },
      { property: "occurred_at", value: Norn.expression(Input, row => Norn.parseTimestamp(row.occurred_at)) },
      { property: "metal", value: Norn.expression(Input, row => row.metal) },
    ],
  }],
})
