import { Norn } from "@norn/authoring"

const SUPPLY_SOURCE_URL = "https://github.com/Norn-cloud/norn/blob/main/registry/packs/supply-lab/supply.ts"
const REVIEWED_AT = "2026-11-27T00:00:00Z"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.supply-lab",
  title: "Supply Lab domain ontology pack",
  version: "0.1.0",
  sources: [
    {
      sourceId: "source.supply-lab",
      releaseId: "0.1.0@560599ae2984",
      releasedAt: "2026-11-27",
      retrievedAt: "2026-11-27",
      canonicalUrl: "https://github.com/Norn-cloud/norn/tree/main/registry/packs/supply-lab",
      license: "Apache-2.0",
      redistribution: "reference_only",
      importMode: "map_only",
      files: [
        {
          path: SUPPLY_SOURCE_URL,
          sha256: "560599ae29843296e062b27147c8d34c06aae7ea9c04205dad890db8dc6e3c4f",
        },
        {
          path: "https://github.com/Norn-cloud/norn/blob/main/registry/packs/supply-lab/actions.ts",
          sha256: "4279b4572b3a3170dad6ac00e0f893db8c828ef17b7e232e1441d9fddfff2465",
        },
      ],
    },
  ],
  terms: [
    { termId: "norn.supply.sku", label: "SKU", definition: "A catalog stock-keeping unit describing a product kind, never a held balance.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.warehouse", label: "Warehouse", definition: "A physical storage site that holds inventory positions and has an active/closed lifecycle.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.inventory.snapshot", label: "Inventory position", definition: "A semi-additive counted stock balance for one warehouse, SKU, and quantity basis, versioned by valid time.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.inventory.snapshot.quantity.basis", label: "Quantity basis (InventorySnapshot)", definition: "Whether a counted stock position is on_hand, available, or reserved; distinct bases never add.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.supply.inventory.snapshot" },
    { termId: "norn.supply.wms.count", label: "WMS scan row", definition: "Raw warehouse-management scan evidence prior to governed promotion into an inventory position.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.stock.movement", label: "Stock movement", definition: "An additive flow event recording a receipt, issue, transfer, or adjustment of a SKU at a warehouse.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.stock.movement.qty", label: "Quantity (StockMovement)", definition: "The additive quantity moved by a stock movement in its SKU unit of measure.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.supply.stock.movement" },
    { termId: "norn.supply.purchase.order", label: "Purchase order", definition: "A purchasing commitment to a supplier with an explicit draft-to-fulfilled/cancelled lifecycle.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.purchase.order.supplier", label: "Supplier (PurchaseOrder)", definition: "The canonical supplier counterparty on a purchase order.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.supply.purchase.order" },
    { termId: "norn.supply.order.line", label: "Order line", definition: "One SKU commitment line on a purchase order with ordered quantity, cumulative received quantity, and unit price.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.shipment.receipt", label: "Shipment receipt", definition: "An inspected delivery event recording received and damaged quantities at a warehouse, optionally linked to an order line.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.shipment.receipt.received.qty", label: "Received quantity (ShipmentReceipt)", definition: "The additive quantity received on a shipment receipt in its unit of measure.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.supply.shipment.receipt" },
    { termId: "norn.supply.shipment.receipt.order.line", label: "Order line (ShipmentReceipt)", definition: "The optional purchase order line commitment fulfilled by a shipment receipt.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.supply.shipment.receipt" },
    { termId: "norn.supply.supplier.receipt.line", label: "Supplier receipt row", definition: "Raw ERP supplier-feed line evidence captured before governed promotion into a shipment receipt.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.inspection", label: "Receipt inspection", definition: "A quality-inspection evidence record over a shipment receipt, bucketed by its receipt time.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.demand.observation", label: "Weekly demand observation", definition: "One SKU by warehouse by ISO week observed demand row feeding the forecasting model.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.demand.forecast", label: "Demand forecast", definition: "One contract-checked demand prediction produced by a production ML model version.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.guard.movement.nonpositive", label: "Non-positive movement guard", definition: "A receipt, issue, or transfer quantity is zero or negative (only adjustments may carry a sign).", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.guard.currency.mismatch", label: "Order-line currency mismatch guard", definition: "An order line carries a currency different from its parent purchase order.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.supply.guard.unit.mismatch", label: "Unit-of-measure mismatch guard", definition: "A stock position, movement, order line, receipt, or inspection uses a unit different from its SKU or linked parent.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
  ],
  mappings: [
    { mappingId: "map.norn.supply.sku", termId: "norn.supply.sku", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.Sku` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.warehouse", termId: "norn.supply.warehouse", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.Warehouse` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.inventory.snapshot", termId: "norn.supply.inventory.snapshot", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.InventorySnapshot` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.stock.movement", termId: "norn.supply.stock.movement", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.StockMovement` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.purchase.order", termId: "norn.supply.purchase.order", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.PurchaseOrder` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.order.line", termId: "norn.supply.order.line", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.OrderLine` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.shipment.receipt", termId: "norn.supply.shipment.receipt", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.ShipmentReceipt` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.inspection", termId: "norn.supply.inspection", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.Inspection` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.demand.observation", termId: "norn.supply.demand.observation", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.DemandObservation` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.supply.demand.forecast", termId: "norn.supply.demand.forecast", sourceId: "source.supply-lab", externalIri: SUPPLY_SOURCE_URL, relation: "close_match", justification: "Declares the `supply.DemandForecast` entity in `registry/packs/supply-lab/supply.ts`.", evidenceRefs: [SUPPLY_SOURCE_URL], confidenceBasisPoints: 10000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
  ],
  shapes: [
    { shapeId: "shape.norn.supply.sku", termId: "norn.supply.sku", closed: true, fields: [{ fieldId: "sku_id", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.warehouse", termId: "norn.supply.warehouse", closed: true, fields: [{ fieldId: "warehouse_id", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.inventory.snapshot", termId: "norn.supply.inventory.snapshot", closed: true, fields: [{ fieldId: "on_hand_qty", valueType: "decimal", minCount: 1, maxCount: 1 }, { fieldId: "as_of", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.stock.movement", termId: "norn.supply.stock.movement", closed: true, fields: [{ fieldId: "qty", valueType: "decimal", minCount: 1, maxCount: 1 }, { fieldId: "occurred_at", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.purchase.order", termId: "norn.supply.purchase.order", closed: true, fields: [{ fieldId: "po_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "promised_at", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.order.line", termId: "norn.supply.order.line", closed: true, fields: [{ fieldId: "ordered_qty", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.shipment.receipt", termId: "norn.supply.shipment.receipt", closed: true, fields: [{ fieldId: "received_qty", valueType: "decimal", minCount: 1, maxCount: 1 }, { fieldId: "received_at", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.inspection", termId: "norn.supply.inspection", closed: true, fields: [{ fieldId: "inspected_qty", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.demand.observation", termId: "norn.supply.demand.observation", closed: true, fields: [{ fieldId: "issued_qty", valueType: "decimal", minCount: 1, maxCount: 1 }, { fieldId: "week_start", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.supply.demand.forecast", termId: "norn.supply.demand.forecast", closed: true, fields: [{ fieldId: "predicted_qty", valueType: "decimal", minCount: 1, maxCount: 1 }, { fieldId: "week_start", valueType: "date_time", minCount: 1, maxCount: 1 }] },
  ],
})
