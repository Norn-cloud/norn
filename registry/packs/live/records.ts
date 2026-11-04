import { Norn } from "@norn/authoring"
import { Catalog, Item } from "../canon/records.ts"

export const ErpnextSource = Norn.sourceSchema({
  id: "erpnext.live.source", title: "ERPNext Item reads", version: "0.1.0",
  source: {system: "erpnext", instance: "live", version: "erpnext-live/1"},
  observedAt: "2026-11-04T00:00:00Z",
  adapter: {name: "norn-source-pull", version: "1"},
  rawMetadata: {uri: "fixtures/connectors/frappe.connector.json", sha256: "c4d0f7c5f7bf2b24b90e442b1a735e2cba947a007e45eebfab242698deae1890"},
  datasets: [{id: "Item", nativeName: "Item", kind: "document"}],
  fields: [
    {dataset: "Item", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "norn_ref", ordinal: 1, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "code", ordinal: 2, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "label", ordinal: 3, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "uom", ordinal: 4, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true}
  ],
  constraints: [{id: "source_id", kind: "primary_key", dataset: "Item", fields: ["name"], origin: "declared"}]
})

export const ErpnextMapping = Norn.mapping({
  id: "erpnext.live.mapping", title: "ERPNext Items to canonical catalog", version: "0.1.0",
  source: {schemaId: "erpnext.live.source", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000"},
  sourceContract: {kind: "generic"}, interfaces: [Catalog],
  maps: [{id: "item", source: {dataset: "Item"}, target: {type: Item, interface: Catalog},
    subject: "input.norn_ref", sourceRecord: "'erpnext/live/Item/' + input.name",
    properties: [{property: "id", value: "input.norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
    ignoredFields: [{field: "uom", reason: "Catalog identity has no stock quantity."}]
  }]
})
