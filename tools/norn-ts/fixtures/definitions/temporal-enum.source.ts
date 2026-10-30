import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Item = Norn.interfaceType({
  name: "temporal.Item", title: "Item", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String }),
})
export const Record = Norn.type({
  name: "temporal.Record", title: "Record", version: "0.1.0", implements: [Item],
  properties: Schema.Struct({
    id: Schema.String, state: Schema.Literals(["open", "closed"]),
    created: Schema.NullOr(Norn.timestamp("utc")), day: Norn.date(),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Source = Norn.sourceSchema({
  id: "temporal.source_schema", title: "Source", version: "0.1.0",
  source: { system: "fixture", instance: "test" }, observedAt: "2026-10-25T00:00:00Z",
  adapter: { name: "fixture", version: "0.1.0" },
  rawMetadata: { uri: "evidence://fixture/meta", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  datasets: [{ id: "items", nativeName: "items", kind: "table" }],
  fields: [
    { dataset: "items", name: "id", ordinal: 0, nativeType: "text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "items", name: "state", ordinal: 1, nativeType: "enum", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "items", name: "created", ordinal: 2, nativeType: "datetime", typeCandidate: { type: "datetime", origin: "declared" } },
    { dataset: "items", name: "day", ordinal: 3, nativeType: "date", typeCandidate: { type: "date", origin: "declared" }, required: true },
  ],
  codeSets: [{ id: "states", dataset: "items", field: "state", origin: "declared", values: [{ value: "open" }, { value: "closed", label: "Closed" }] }],
})
const Input = Schema.Struct({
  id: Schema.String, state: Schema.Literals(["open", "closed"]),
  created: Schema.NullOr(Norn.timestamp("offset")), day: Norn.date(),
})
export const Mapping = Norn.mapping({
  id: "temporal.items.mapping", title: "Mapping", version: "0.1.0",
  // The integration test replaces this placeholder with the Rust receipt's hash.
  source: { schemaId: "temporal.source_schema", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  interfaces: [Item], maps: [{
    id: "items", source: { dataset: "items" }, target: { type: Record, interface: Item },
    subject: Norn.expression(Input, input => input.id), sourceRecord: Norn.expression(Input, input => input.id),
    properties: [
      { property: "id", value: Norn.expression(Input, input => input.id) },
      { property: "state", value: Norn.expression(Input, input => input.state) },
      { property: "created", value: Norn.expression(Input, input => input.created === null ? null : Norn.parseTimestamp(input.created ?? "")) },
      { property: "day", value: Norn.expression(Input, input => Norn.parseDate(input.day)) },
    ],
  }],
})
