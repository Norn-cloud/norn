import { Norn } from "@norn/authoring"
import { Resource } from "../../core/src/physics.ts"
import { Catalog, Item, Holding } from "../canon/records.ts"

export const ShopifySource = Norn.sourceSchema({
  id: "shopify.live.source",
  title: "Shopify rows shaped by norn source pull",
  version: "0.1.0",
  source: {system: "shopify", instance: "live", version: "shopify-live/1"},
  observedAt: "2026-11-04T00:00:00Z",
  adapter: {name: "norn-source-pull", version: "1"},
  rawMetadata: {uri: "registry/packs/live/synthetic.json", sha256: "f1903e2546a9faae0e1c45dece8f147e4bfe267917ba4371f97c2776e675f435"},
  datasets: [
    {id: "ProductVariant", nativeName: "ProductVariant", kind: "document"},
    {id: "InventoryLevel", nativeName: "InventoryLevel", kind: "document"}
  ],
  fields: [
    {dataset: "ProductVariant", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "norn_ref", ordinal: 1, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "code", ordinal: 2, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "sku", ordinal: 3, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "label", ordinal: 4, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "price", ordinal: 5, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "norn_ref", ordinal: 1, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "resource_code", ordinal: 2, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "quantity", ordinal: 3, nativeType: "Decimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "location", ordinal: 4, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "item_ref", ordinal: 5, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true}
  ],
  constraints: [
    {id: "source_id_0", kind: "primary_key", dataset: "ProductVariant", fields: ["id"], origin: "declared"},
    {id: "source_id_1", kind: "primary_key", dataset: "InventoryLevel", fields: ["id"], origin: "declared"}
  ]
})

export const ShopifyMapping = Norn.mapping({
  id: "shopify.live.mapping",
  title: "Shopify live reads to the REA canon",
  version: "0.1.0",
  source: {schemaId: "shopify.live.source", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000"},
  sourceContract: {kind: "generic"},
  interfaces: [Catalog, Resource],
  maps: [
    {
      id: "item",
      source: {dataset: "ProductVariant"},
      target: {type: Item, interface: Catalog},
      subject: "input.norn_ref",
      sourceRecord: "'shopify/live/ProductVariant/' + input.id",
      properties: [{property: "id", value: "input.norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
      ignoredFields: [{field: "sku", reason: "Alias only; a SKU never establishes identity."}, {field: "price", reason: "Sales price is not stock cost."}]
    },
    {
      id: "holding",
      source: {dataset: "InventoryLevel"},
      target: {type: Holding, interface: Resource},
      subject: "input.norn_ref",
      sourceRecord: "'shopify/live/InventoryLevel/' + input.id",
      properties: [
        {property: "resource_code", value: "input.resource_code"},
        {property: "quantity", value: "input.quantity"},
        {property: "unit", value: "'{each}'"},
        {property: "currency", value: "null"},
        {property: "valuation_method", value: "null"},
        {property: "location", value: "input.location"},
        {property: "quantity_basis", value: "'available'"}
      ],
      links: [{link: "item", targetMap: "item", key: "input.item_ref"}],
      gaps: [
        {member: "stock_cost", reason: "not_in_source", note: "This read supplies no valuation."},
        {member: "cost_currency", reason: "not_in_source", note: "This read supplies no valuation."}
      ]
    }
  ]
})

export const ErpnextSource = Norn.sourceSchema({
  id: "erpnext.live.source",
  title: "Erpnext rows shaped by norn source pull",
  version: "0.1.0",
  source: {system: "erpnext", instance: "live", version: "erpnext-live/1"},
  observedAt: "2026-11-04T00:00:00Z",
  adapter: {name: "norn-source-pull", version: "1"},
  rawMetadata: {uri: "registry/packs/live/synthetic.json", sha256: "f1903e2546a9faae0e1c45dece8f147e4bfe267917ba4371f97c2776e675f435"},
  datasets: [
    {id: "Item", nativeName: "Item", kind: "document"},
    {id: "Bin", nativeName: "Bin", kind: "document"}
  ],
  fields: [
    {dataset: "Item", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "norn_ref", ordinal: 1, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "code", ordinal: 2, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "label", ordinal: 3, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "uom", ordinal: 4, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Bin", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Bin", name: "norn_ref", ordinal: 1, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Bin", name: "resource_code", ordinal: 2, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Bin", name: "quantity", ordinal: 3, nativeType: "Decimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Bin", name: "uom", ordinal: 4, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Bin", name: "location", ordinal: 5, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Bin", name: "item_ref", ordinal: 6, nativeType: "Text", typeCandidate: {type: "string", origin: "declared"}, required: true}
  ],
  codeSets: [
    {id: "item_uom", dataset: "Item", field: "uom", origin: "declared", values: [{value: "Nos"}]},
    {id: "bin_uom", dataset: "Bin", field: "uom", origin: "declared", values: [{value: "Nos"}]}
  ],
  constraints: [
    {id: "source_id_0", kind: "primary_key", dataset: "Item", fields: ["name"], origin: "declared"},
    {id: "source_id_1", kind: "primary_key", dataset: "Bin", fields: ["name"], origin: "declared"}
  ]
})

export const ErpnextMapping = Norn.mapping({
  id: "erpnext.live.mapping",
  title: "Erpnext live reads to the REA canon",
  version: "0.1.0",
  source: {schemaId: "erpnext.live.source", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000"},
  sourceContract: {kind: "generic"},
  interfaces: [Catalog, Resource],
  maps: [
    {
      id: "item",
      source: {dataset: "Item"},
      target: {type: Item, interface: Catalog},
      subject: "input.norn_ref",
      sourceRecord: "'erpnext/live/Item/' + input.name",
      properties: [{property: "id", value: "input.norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
      ignoredFields: [{field: "uom", reason: "Stock unit; quantity is on the Bin."}]
    },
    {
      id: "holding",
      source: {dataset: "Bin"},
      target: {type: Holding, interface: Resource},
      subject: "input.norn_ref",
      sourceRecord: "'erpnext/live/Bin/' + input.name",
      properties: [
        {property: "resource_code", value: "input.resource_code"},
        {property: "quantity", value: "input.quantity"},
        {property: "unit", value: "'{each}'"},
        {property: "currency", value: "null"},
        {property: "valuation_method", value: "null"},
        {property: "location", value: "input.location"},
        {property: "quantity_basis", value: "'on_hand'"}
      ],
      links: [{link: "item", targetMap: "item", key: "input.item_ref"}],
      gaps: [
        {member: "stock_cost", reason: "not_in_source", note: "This read supplies no valuation."},
        {member: "cost_currency", reason: "not_in_source", note: "This read supplies no valuation."}
      ],
      ignoredFields: [{field: "uom", reason: "The declared code set admits only Nos, which maps to {each}."}]
    }
  ]
})
