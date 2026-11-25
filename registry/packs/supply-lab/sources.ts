import { Norn } from "@norn/authoring"
import { CountEvidence, ReceiptEvidence, SupplierReceiptLine, WmsCount } from "./supply.ts"

// Two messy synthetic feeds. Every row is captured as evidence with its source
// record kept; a row that violates a law (negative quantity, more damaged than
// received, an unknown unit) is quarantined with a finding, never dropped and
// never promoted. Promotion to positions and receipts is a governed action.

// ------------------------------------------------------- ERP supplier feed

export const ErpSupplierSource = Norn.sourceSchema({
  id: "supply.erp_supplier.source", title: "Synthetic ERP supplier feed", version: "0.1.0",
  source: { system: "synthetic-erp", instance: "supply-lab", version: "16-purchase-receipt-v1" },
  observedAt: "2026-10-26T00:00:00Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://supply-lab/erp-supplier-v1", sha256: "6f0e7e3a49c7c1c4f2b0b8f6f1d0a2f2b6c1e4d9a8b7c6d5e4f3a2b1c0d9e8f7" },
  datasets: [{ id: "Purchase Receipt Item", nativeName: "Synthetic Purchase Receipt Item", kind: "child_document" }],
  fields: [
    { dataset: "Purchase Receipt Item", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Purchase Receipt Item", name: "parent", ordinal: 1, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Purchase Receipt Item", name: "supplier", ordinal: 2, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Purchase Receipt Item", name: "purchase_order_item", ordinal: 3, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "Purchase Receipt Item", name: "received_qty", ordinal: 4, nativeType: "Float", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Purchase Receipt Item", name: "rejected_qty", ordinal: 5, nativeType: "Float", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Purchase Receipt Item", name: "uom", ordinal: 6, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Purchase Receipt Item", name: "posting_datetime", ordinal: 7, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [{ id: "row_identity", kind: "primary_key", dataset: "Purchase Receipt Item", fields: ["name"], origin: "declared" }],
})

export const ErpSupplierMapping = Norn.mapping({
  id: "supply.erp_supplier.mapping", title: "ERP supplier receipts into receipt evidence", version: "0.1.0",
  // MAPPING-SOURCE-REF-1: bound by reference; the compiler pins the schema's Norn1 hash.
  source: ErpSupplierSource,
  sourceContract: { kind: "generic" }, interfaces: [ReceiptEvidence],
  maps: [{
    id: "receipt_line", source: { dataset: "Purchase Receipt Item" }, target: { type: SupplierReceiptLine, interface: ReceiptEvidence },
    subject: "input.name", sourceRecord: "input.parent + '/' + input.name",
    properties: [
      { property: "receipt_line_id", value: "input.name" },
      // ERP vendor codes onto canonical supplier codes; an unknown vendor keeps its own code.
      { property: "supplier_id", value: "({'NILE-001': 'SUP-NILE', 'DELTA-07': 'SUP-DELTA'}[input.supplier]) ?? input.supplier" },
      { property: "po_item_ref", value: "input.purchase_order_item" },
      { property: "received_qty", value: "input.received_qty" },
      { property: "damaged_qty", value: "input.rejected_qty" },
      // ERP units onto canonical units via a bounded literal lookup table (O(1) key-length
      // cost regardless of table size); any other unit passes through and the `unit` enum
      // quarantines it (MAP-ENUM-TEXT-1).
      { property: "unit", value: "({'Nos': 'each', 'Kg': 'kg', 'Litre': 'l'}[input.uom]) ?? input.uom" },
      { property: "received_at", value: "timestamp(input.posting_datetime)" },
    ],
  }],
})

export const ErpSupplierConnector = Norn.connector({
  id: "supply.erp_supplier", title: "ERP supplier feed", version: "0.1.0",
  auth: { kind: "api_key", authId: "erp", placement: "header", name: "Authorization", secretRef: "SUPPLY_ERP_READ_KEY", prefix: "token " },
  resources: [{ kind: "delta", resourceId: "purchase_receipt_items", endpoint: "/api/resource/Purchase Receipt Item", sinceParam: "modified_since", updatedAtPath: "/modified", itemsPath: "/data" }],
  mappings: [{ kind: "external_id", resourceId: "purchase_receipt_items", sourcePath: "/name", targetObjectKind: "supply.SupplierReceiptLine", externalSource: "synthetic-erp" }],
  cursor: { cursorId: "erp_receipts", resourceId: "purchase_receipt_items", storageKey: "supply.erp.receipts" },
  deadLetterPolicy: { maxFailures: 3, route: "ledger" },
  poll: { feeds: [{ resourceId: "purchase_receipt_items", recordKeyPath: "/name" }] },
})

// ------------------------------------------------------------ WMS scan feed

export const WmsScanSource = Norn.sourceSchema({
  id: "supply.wms_scan.source", title: "Synthetic WMS scan feed", version: "0.1.0",
  source: { system: "synthetic-wms", instance: "supply-lab", version: "scan-export-v3" },
  observedAt: "2026-10-26T00:00:00Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://supply-lab/wms-scan-v3", sha256: "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809" },
  datasets: [{ id: "stock_count", nativeName: "WMS stock count export", kind: "table" }],
  fields: [
    { dataset: "stock_count", name: "scan_id", ordinal: 0, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "stock_count", name: "site", ordinal: 1, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "stock_count", name: "item", ordinal: 2, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "stock_count", name: "qty", ordinal: 3, nativeType: "NUMERIC", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "stock_count", name: "uom", ordinal: 4, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "stock_count", name: "basis", ordinal: 5, nativeType: "VARCHAR", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "stock_count", name: "counted_at", ordinal: 6, nativeType: "TIMESTAMP", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  // `basis` declares no code set: the scanner sends free text, mapped into the
  // `quantity_basis` enum (MAP-ENUM-TEXT-1), so an unknown basis lands quarantined.
  constraints: [{ id: "scan_identity", kind: "primary_key", dataset: "stock_count", fields: ["scan_id"], origin: "declared" }],
})

export const WmsScanMapping = Norn.mapping({
  id: "supply.wms_scan.mapping", title: "WMS scans into count evidence", version: "0.1.0",
  source: WmsScanSource,
  sourceContract: { kind: "generic" }, interfaces: [CountEvidence],
  maps: [{
    id: "scan", source: { dataset: "stock_count" }, target: { type: WmsCount, interface: CountEvidence },
    subject: "input.scan_id", sourceRecord: "input.site + '/' + input.scan_id",
    properties: [
      { property: "scan_id", value: "input.scan_id" },
      { property: "warehouse_code", value: "input.site" },
      { property: "sku_code", value: "input.item" },
      { property: "on_hand_qty", value: "input.qty" },
      { property: "unit", value: "({'EA': 'each', 'KG': 'kg', 'L': 'l'}[input.uom]) ?? input.uom" },
      { property: "quantity_basis", value: "input.basis" },
      { property: "as_of", value: "timestamp(input.counted_at)" },
    ],
  }],
})

export const WmsScanConnector = Norn.connector({
  id: "supply.wms_scan", title: "WMS scan feed", version: "0.1.0",
  auth: { kind: "api_key", authId: "wms", placement: "header", name: "X-Api-Key", secretRef: "SUPPLY_WMS_READ_KEY" },
  resources: [{ kind: "cursor", resourceId: "stock_counts", endpoint: "/v3/stock-counts", cursorParam: "after", nextCursorPath: "/next", itemsPath: "/counts", missingItemsIsEmpty: true }],
  mappings: [{ kind: "external_id", resourceId: "stock_counts", sourcePath: "/scan_id", targetObjectKind: "supply.WmsCount", externalSource: "synthetic-wms" }],
  cursor: { cursorId: "wms_scans", resourceId: "stock_counts", storageKey: "supply.wms.scans" },
  deadLetterPolicy: { maxFailures: 5, route: "ledger" },
  poll: { feeds: [{ resourceId: "stock_counts", recordKeyPath: "/scan_id" }] },
})
