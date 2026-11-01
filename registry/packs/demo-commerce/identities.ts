import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const SourceIdentity = Norn.interfaceType({
  name: "demo.commerce.SourceIdentity", title: "Explicit synthetic source identity", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String }),
})
export const CatalogEntry = Norn.type({
  name: "demo.commerce.CatalogEntry", title: "Catalog entry evidence", version: "0.1.0",
  implements: [SourceIdentity], properties: Schema.Struct({ id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Shipment = Norn.type({
  name: "demo.commerce.Shipment", title: "Shipment identity evidence", version: "0.1.0",
  implements: [SourceIdentity], properties: Schema.Struct({ id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "canonical_event", target: "core.EconomicEvent", cardinality: { to: "one", from: "many" } }],
})

export const ERPNext = Norn.sourceSchema({
  id: "demo.erpnext.identity.source", title: "Synthetic ERPNext identity cross-references", version: "0.1.0",
  source: { system: "synthetic-erpnext", instance: "demo", version: "16-custom-identity-v1" },
  observedAt: "2026-10-25T23:59:59Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://demo-commerce/identity-v1", sha256: "cf43670ecea4c65bd77bc6860a5644d4858e878a0d80ff9db0508d71f6fcfd7d" },
  datasets: [{ id: "Item", nativeName: "Synthetic Item", kind: "document" }, { id: "Delivery Note", nativeName: "Synthetic Delivery Note", kind: "document" }],
  fields: [
    { dataset: "Item", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Item", name: "custom_catalog_ref", ordinal: 1, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Item", name: "item_code", ordinal: 2, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Delivery Note", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Delivery Note", name: "custom_shipment_ref", ordinal: 1, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [
    { id: "item_id", kind: "primary_key", dataset: "Item", fields: ["name"], origin: "declared" },
    { id: "delivery_id", kind: "primary_key", dataset: "Delivery Note", fields: ["name"], origin: "declared" },
  ],
})
export const Shopify = Norn.sourceSchema({
  id: "demo.shopify.identity.source", title: "Synthetic Shopify identity cross-references", version: "0.1.0",
  source: { system: "synthetic-shopify", instance: "demo", version: "custom-identity-v1" },
  observedAt: "2026-10-25T23:59:59Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://demo-commerce/identity-v1", sha256: "cf43670ecea4c65bd77bc6860a5644d4858e878a0d80ff9db0508d71f6fcfd7d" },
  datasets: [{ id: "ProductVariant", nativeName: "Synthetic ProductVariant", kind: "document" }, { id: "Fulfillment", nativeName: "Synthetic Fulfillment", kind: "document" }],
  fields: [
    { dataset: "ProductVariant", name: "id", ordinal: 0, nativeType: "ID", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "ProductVariant", name: "custom_catalog_ref", ordinal: 1, nativeType: "String", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "ProductVariant", name: "sku", ordinal: 2, nativeType: "String", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Fulfillment", name: "id", ordinal: 0, nativeType: "ID", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Fulfillment", name: "custom_shipment_ref", ordinal: 1, nativeType: "String", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [
    { id: "variant_id", kind: "primary_key", dataset: "ProductVariant", fields: ["id"], origin: "declared" },
    { id: "fulfillment_id", kind: "primary_key", dataset: "Fulfillment", fields: ["id"], origin: "declared" },
  ],
})
export const ERPNextMapping = Norn.mapping({
  id: "demo.erpnext.identity.mapping", title: "ERPNext explicit demo identities", version: "0.1.0",
  source: { schemaId: "demo.erpnext.identity.source", version: "0.1.0", sha256: "f6874ecd93a6cfb36cb57f66e81ee60e1149b9c6970f1f64505a1052a58935b3" },
  sourceContract: { kind: "generic" }, interfaces: [SourceIdentity],
  maps: [
    { id: "catalog", source: { dataset: "Item" }, target: { type: CatalogEntry, interface: SourceIdentity },
      subject: "input.custom_catalog_ref", sourceRecord: "'Item/' + input.name",
      properties: [{ property: "id", value: "input.custom_catalog_ref" }],
      ignoredFields: [{ field: "item_code", reason: "SKU is evidence, never a cross-system identity or a Holding." }] },
    { id: "shipment", source: { dataset: "Delivery Note" }, target: { type: Shipment, interface: SourceIdentity },
      subject: "input.custom_shipment_ref", sourceRecord: "'Delivery Note/' + input.name",
      properties: [{ property: "id", value: "input.custom_shipment_ref" }],
      gaps: [{ member: "canonical_event", reason: "deferred", note: "Requires admitted canonical shipment promotion; never deplete stock from each source independently." }] },
  ],
})
export const ShopifyMapping = Norn.mapping({
  id: "demo.shopify.identity.mapping", title: "Shopify explicit demo identities", version: "0.1.0",
  source: { schemaId: "demo.shopify.identity.source", version: "0.1.0", sha256: "2d5f8307387f9ef7094bdc02498536c593586ae3151b8ad614ceba9f317afda1" },
  sourceContract: { kind: "generic" }, interfaces: [SourceIdentity],
  maps: [
    { id: "catalog", source: { dataset: "ProductVariant" }, target: { type: CatalogEntry, interface: SourceIdentity },
      subject: "input.custom_catalog_ref", sourceRecord: "'ProductVariant/' + input.id",
      properties: [{ property: "id", value: "input.custom_catalog_ref" }],
      ignoredFields: [{ field: "sku", reason: "SKU is evidence, never a cross-system identity or a Holding." }] },
    { id: "shipment", source: { dataset: "Fulfillment" }, target: { type: Shipment, interface: SourceIdentity },
      subject: "input.custom_shipment_ref", sourceRecord: "'Fulfillment/' + input.id",
      properties: [{ property: "id", value: "input.custom_shipment_ref" }],
      gaps: [{ member: "canonical_event", reason: "deferred", note: "Same explicit shipment key as ERPNext; evidence adds no independent economic event or stock effect." }] },
  ],
})
