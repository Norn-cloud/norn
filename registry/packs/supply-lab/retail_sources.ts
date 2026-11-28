import { Norn } from "@norn/authoring"
import { CountEvidence, ReceiptEvidence } from "./supply.ts"
import {
  CostEvidence, PosSaleEvidence, RetailReceiptEvidence, SaleEvidence,
  StoreCountEvidence, UnitCostEvidence,
} from "./retail.ts"

// Four messy retail feeds (POS sales, store physical counts, retail supplier
// receipts, and supplier unit costs). Every row lands as evidence with source
// provenance; enum or law violations are quarantined with durable findings and
// never silently dropped or promoted.

// --------------------------------------------------------------- POS sales

export const PosSalesSource = Norn.sourceSchema({
  id: "supply.pos_sales.source", title: "Synthetic retail POS sales feed", version: "0.1.0",
  source: { system: "synthetic-pos", instance: "retail-lab", version: "pos-sales-v1" },
  observedAt: "2026-10-26T00:00:00Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://supply-lab/pos-sales-v1", sha256: "2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a" },
  datasets: [{ id: "pos_sale_line", nativeName: "POS ticket line export", kind: "table" }],
  fields: [
    { dataset: "pos_sale_line", name: "pos_line_id", ordinal: 0, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "branch_code", ordinal: 1, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "barcode_sku", ordinal: 2, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "qty", ordinal: 3, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "uom", ordinal: 4, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "net_amount", ordinal: 5, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "ccy", ordinal: 6, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "pos_sale_line", name: "sold_at", ordinal: 7, nativeType: "TIMESTAMP", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [{ id: "pos_line_identity", kind: "primary_key", dataset: "pos_sale_line", fields: ["pos_line_id"], origin: "declared" }],
})

export const PosSalesMapping = Norn.mapping({
  id: "supply.pos_sales.mapping", title: "POS sales into sale evidence", version: "0.1.0",
  source: PosSalesSource,
  sourceContract: { kind: "generic" }, interfaces: [SaleEvidence],
  maps: [{
    id: "pos_sale", source: { dataset: "pos_sale_line" }, target: { type: PosSaleEvidence, interface: SaleEvidence },
    subject: "input.pos_line_id", sourceRecord: "input.branch_code + '/' + input.pos_line_id",
    properties: [
      { property: "pos_line_id", value: "input.pos_line_id" },
      { property: "store_code", value: "input.branch_code" },
      { property: "sku_code", value: "input.barcode_sku" },
      { property: "sold_qty", value: "input.qty" },
      { property: "unit", value: "({'PCS': 'each', 'EA': 'each', 'KG': 'kg', 'L': 'l'})[input.uom] ?? input.uom" },
      { property: "net_revenue", value: "input.net_amount" },
      { property: "currency", value: "({'EGP': 'EGP', 'USD': 'USD', 'EUR': 'EUR', 'LE': 'EGP'})[input.ccy] ?? input.ccy" },
      { property: "sold_at", value: "timestamp(input.sold_at)" },
    ],
  }],
})

export const PosSalesConnector = Norn.connector({
  id: "supply.pos_sales", title: "Retail POS sales feed", version: "0.1.0",
  auth: { kind: "api_key", authId: "pos", placement: "header", name: "X-Pos-Key", secretRef: "SUPPLY_POS_READ_KEY" },
  resources: [{ kind: "cursor", resourceId: "pos_sale_lines", endpoint: "/v1/pos/sales", cursorParam: "after", nextCursorPath: "/next", itemsPath: "/lines", missingItemsIsEmpty: true }],
  mappings: [{ kind: "external_id", resourceId: "pos_sale_lines", sourcePath: "/pos_line_id", targetObjectKind: "supply.PosSaleEvidence", externalSource: "synthetic-pos" }],
  cursor: { cursorId: "pos_sales", resourceId: "pos_sale_lines", storageKey: "supply.pos.sales" },
  deadLetterPolicy: { maxFailures: 5, route: "ledger" },
  poll: { feeds: [{ resourceId: "pos_sale_lines", recordKeyPath: "/pos_line_id" }] },
})

// --------------------------------------------------- store physical counts

export const StoreCountSource = Norn.sourceSchema({
  id: "supply.store_counts.source", title: "Synthetic store physical count feed", version: "0.1.0",
  source: { system: "synthetic-store-count", instance: "retail-lab", version: "store-counts-v1" },
  observedAt: "2026-10-26T00:00:00Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://supply-lab/store-counts-v1", sha256: "3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b" },
  datasets: [{ id: "store_count", nativeName: "Handheld store cycle count", kind: "table" }],
  fields: [
    { dataset: "store_count", name: "count_id", ordinal: 0, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "store_count", name: "store_code", ordinal: 1, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "store_count", name: "sku_code", ordinal: 2, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "store_count", name: "counted_qty", ordinal: 3, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "store_count", name: "uom", ordinal: 4, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "store_count", name: "basis", ordinal: 5, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "store_count", name: "counted_at", ordinal: 6, nativeType: "TIMESTAMP", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [{ id: "store_count_identity", kind: "primary_key", dataset: "store_count", fields: ["count_id"], origin: "declared" }],
})

export const StoreCountMapping = Norn.mapping({
  id: "supply.store_counts.mapping", title: "Store counts into count evidence", version: "0.1.0",
  source: StoreCountSource,
  sourceContract: { kind: "generic" }, interfaces: [CountEvidence],
  maps: [{
    id: "store_count", source: { dataset: "store_count" }, target: { type: StoreCountEvidence, interface: CountEvidence },
    subject: "input.count_id", sourceRecord: "input.store_code + '/' + input.count_id",
    properties: [
      { property: "count_id", value: "input.count_id" },
      { property: "store_code", value: "input.store_code" },
      { property: "sku_code", value: "input.sku_code" },
      { property: "on_hand_qty", value: "input.counted_qty" },
      { property: "unit", value: "({'PCS': 'each', 'EA': 'each', 'KG': 'kg', 'L': 'l'})[input.uom] ?? input.uom" },
      { property: "quantity_basis", value: "input.basis" },
      { property: "as_of", value: "timestamp(input.counted_at)" },
    ],
  }],
})

export const StoreCountConnector = Norn.connector({
  id: "supply.store_counts", title: "Store physical count feed", version: "0.1.0",
  auth: { kind: "api_key", authId: "counts", placement: "header", name: "X-Count-Key", secretRef: "SUPPLY_COUNT_READ_KEY" },
  resources: [{ kind: "cursor", resourceId: "store_counts", endpoint: "/v1/store-counts", cursorParam: "after", nextCursorPath: "/next", itemsPath: "/counts", missingItemsIsEmpty: true }],
  mappings: [{ kind: "external_id", resourceId: "store_counts", sourcePath: "/count_id", targetObjectKind: "supply.StoreCountEvidence", externalSource: "synthetic-store-count" }],
  cursor: { cursorId: "store_counts", resourceId: "store_counts", storageKey: "supply.store.counts" },
  deadLetterPolicy: { maxFailures: 5, route: "ledger" },
  poll: { feeds: [{ resourceId: "store_counts", recordKeyPath: "/count_id" }] },
})

// ------------------------------------------------- retail supplier receipts

export const RetailReceiptSource = Norn.sourceSchema({
  id: "supply.retail_receipts.source", title: "Synthetic retail supplier receipt feed", version: "0.1.0",
  source: { system: "synthetic-retail-receipt", instance: "retail-lab", version: "retail-receipts-v1" },
  observedAt: "2026-10-26T00:00:00Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://supply-lab/retail-receipts-v1", sha256: "4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c" },
  datasets: [{ id: "retail_receipt", nativeName: "Store supplier receipt line", kind: "child_document" }],
  fields: [
    { dataset: "retail_receipt", name: "receipt_line_id", ordinal: 0, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "store_code", ordinal: 1, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "sku_code", ordinal: 2, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "vendor_code", ordinal: 3, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "received_qty", ordinal: 4, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "damaged_qty", ordinal: 5, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "uom", ordinal: 6, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "unit_cost", ordinal: 7, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: false },
    { dataset: "retail_receipt", name: "ccy", ordinal: 8, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "retail_receipt", name: "received_at", ordinal: 9, nativeType: "TIMESTAMP", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [{ id: "retail_receipt_identity", kind: "primary_key", dataset: "retail_receipt", fields: ["receipt_line_id"], origin: "declared" }],
})

export const RetailReceiptMapping = Norn.mapping({
  id: "supply.retail_receipts.mapping", title: "Retail receipts into receipt evidence", version: "0.1.0",
  source: RetailReceiptSource,
  sourceContract: { kind: "generic" }, interfaces: [ReceiptEvidence],
  maps: [{
    id: "retail_receipt", source: { dataset: "retail_receipt" }, target: { type: RetailReceiptEvidence, interface: ReceiptEvidence },
    subject: "input.receipt_line_id", sourceRecord: "input.store_code + '/' + input.receipt_line_id",
    properties: [
      { property: "receipt_line_id", value: "input.receipt_line_id" },
      { property: "store_code", value: "input.store_code" },
      { property: "sku_code", value: "input.sku_code" },
      { property: "supplier_id", value: "({'ATELIER-01': 'SUP-ATELIER', 'GOLD-09': 'SUP-GOLD'})[input.vendor_code] ?? input.vendor_code" },
      { property: "received_qty", value: "input.received_qty" },
      { property: "damaged_qty", value: "input.damaged_qty" },
      { property: "unit", value: "({'PCS': 'each', 'EA': 'each', 'KG': 'kg', 'L': 'l'})[input.uom] ?? input.uom" },
      { property: "unit_cost", value: "input.unit_cost" },
      { property: "currency", value: "({'EGP': 'EGP', 'USD': 'USD', 'EUR': 'EUR', 'LE': 'EGP'})[input.ccy] ?? input.ccy" },
      { property: "received_at", value: "timestamp(input.received_at)" },
    ],
  }],
})

export const RetailReceiptConnector = Norn.connector({
  id: "supply.retail_receipts", title: "Retail supplier receipts feed", version: "0.1.0",
  auth: { kind: "api_key", authId: "retail_receipts", placement: "header", name: "X-Receipt-Key", secretRef: "SUPPLY_RECEIPT_READ_KEY" },
  resources: [{ kind: "cursor", resourceId: "retail_receipts", endpoint: "/v1/retail-receipts", cursorParam: "after", nextCursorPath: "/next", itemsPath: "/receipts", missingItemsIsEmpty: true }],
  mappings: [{ kind: "external_id", resourceId: "retail_receipts", sourcePath: "/receipt_line_id", targetObjectKind: "supply.RetailReceiptEvidence", externalSource: "synthetic-retail-receipt" }],
  cursor: { cursorId: "retail_receipts", resourceId: "retail_receipts", storageKey: "supply.retail.receipts" },
  deadLetterPolicy: { maxFailures: 5, route: "ledger" },
  poll: { feeds: [{ resourceId: "retail_receipts", recordKeyPath: "/receipt_line_id" }] },
})

// ------------------------------------------------------ supplier unit costs

export const UnitCostSource = Norn.sourceSchema({
  id: "supply.unit_costs.source", title: "Synthetic supplier unit cost feed", version: "0.1.0",
  source: { system: "synthetic-cost", instance: "retail-lab", version: "unit-costs-v1" },
  observedAt: "2026-10-26T00:00:00Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://supply-lab/unit-costs-v1", sha256: "5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d" },
  datasets: [{ id: "unit_cost", nativeName: "Supplier unit cost sheet", kind: "table" }],
  fields: [
    { dataset: "unit_cost", name: "cost_id", ordinal: 0, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "unit_cost", name: "sku_code", ordinal: 1, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "unit_cost", name: "vendor_code", ordinal: 2, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "unit_cost", name: "unit_cost", ordinal: 3, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "unit_cost", name: "ccy", ordinal: 4, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "unit_cost", name: "effective_from", ordinal: 5, nativeType: "TIMESTAMP", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [{ id: "unit_cost_identity", kind: "primary_key", dataset: "unit_cost", fields: ["cost_id"], origin: "declared" }],
})

export const UnitCostMapping = Norn.mapping({
  id: "supply.unit_costs.mapping", title: "Supplier costs into unit cost evidence", version: "0.1.0",
  source: UnitCostSource,
  sourceContract: { kind: "generic" }, interfaces: [CostEvidence],
  maps: [{
    id: "unit_cost", source: { dataset: "unit_cost" }, target: { type: UnitCostEvidence, interface: CostEvidence },
    subject: "input.cost_id", sourceRecord: "input.vendor_code + '/' + input.cost_id",
    properties: [
      { property: "cost_id", value: "input.cost_id" },
      { property: "sku_code", value: "input.sku_code" },
      { property: "supplier_id", value: "({'ATELIER-01': 'SUP-ATELIER', 'GOLD-09': 'SUP-GOLD'})[input.vendor_code] ?? input.vendor_code" },
      { property: "unit_cost", value: "input.unit_cost" },
      { property: "currency", value: "({'EGP': 'EGP', 'USD': 'USD', 'EUR': 'EUR', 'LE': 'EGP'})[input.ccy] ?? input.ccy" },
      { property: "effective_from", value: "timestamp(input.effective_from)" },
    ],
  }],
})

export const UnitCostConnector = Norn.connector({
  id: "supply.unit_costs", title: "Supplier unit cost feed", version: "0.1.0",
  auth: { kind: "api_key", authId: "costs", placement: "header", name: "X-Cost-Key", secretRef: "SUPPLY_COST_READ_KEY" },
  resources: [{ kind: "cursor", resourceId: "unit_costs", endpoint: "/v1/unit-costs", cursorParam: "after", nextCursorPath: "/next", itemsPath: "/costs", missingItemsIsEmpty: true }],
  mappings: [{ kind: "external_id", resourceId: "unit_costs", sourcePath: "/cost_id", targetObjectKind: "supply.UnitCostEvidence", externalSource: "synthetic-cost" }],
  cursor: { cursorId: "unit_costs", resourceId: "unit_costs", storageKey: "supply.unit.costs" },
  deadLetterPolicy: { maxFailures: 5, route: "ledger" },
  poll: { feeds: [{ resourceId: "unit_costs", recordKeyPath: "/cost_id" }] },
})
