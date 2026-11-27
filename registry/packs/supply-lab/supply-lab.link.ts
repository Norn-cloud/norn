import { Norn } from "@norn/authoring"

const REVIEWED_AT = "2026-11-27T00:00:00Z"

export const Pack = Norn.packLink({
  id: "norn.link.supply-lab.canon",
  title: "Supply Lab to the Norn canon",
  version: "0.1.0",
  fromPack: { packId: "norn.ontology-pack.supply-lab", version: "0.1.0" },
  toPack: { packId: "norn.ontology-pack.canon", version: "0.1.0" },
  correspondences: [
    { correspondenceId: "corr.supply.sku", fromTermId: "norn.supply.sku", relation: "subsumed_by", toTermIds: ["norn.core.resource.type"], justification: "A SKU is a catalog good kind; canon resource types also cover services.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 9000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.stock.movement", fromTermId: "norn.supply.stock.movement", relation: "subsumed_by", toTermIds: ["norn.core.event"], justification: "Stock movements cover receipts, issues, transfers, and adjustments, so the economic event superclass holds.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8500, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.stock.movement.qty", fromTermId: "norn.supply.stock.movement.qty", relation: "subsumed_by", toTermIds: ["norn.core.resource.quantity"], justification: "The quantity moved by a stock movement in its SKU unit of measure.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8500, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.purchase.order", fromTermId: "norn.supply.purchase.order", relation: "subsumed_by", toTermIds: ["norn.core.commitment"], justification: "A purchase order is an inbound purchasing commitment; canon `norn.commerce.order` covers sales orders only.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8500, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.order.line", fromTermId: "norn.supply.order.line", relation: "subsumed_by", toTermIds: ["norn.core.commitment"], justification: "Each order line is a SKU-level delivery commitment within a purchase order.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8500, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.shipment.receipt", fromTermId: "norn.supply.shipment.receipt", relation: "subsumed_by", toTermIds: ["norn.core.event.transfer"], justification: "A shipment receipt records physical transfer of goods into a warehouse.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8500, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.shipment.receipt.received.qty", fromTermId: "norn.supply.shipment.receipt.received.qty", relation: "subsumed_by", toTermIds: ["norn.core.resource.quantity"], justification: "The additive quantity received on a delivery transfer event.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8500, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.shipment.receipt.order.line", fromTermId: "norn.supply.shipment.receipt.order.line", relation: "subsumed_by", toTermIds: ["norn.core.event.fulfils"], justification: "Links a shipment receipt to the purchase order line commitment it fulfils.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 9000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.guard.movement.nonpositive", fromTermId: "norn.supply.guard.movement.nonpositive", relation: "subsumed_by", toTermIds: ["norn.finance.guard.amount.nonpositive"], justification: "Non-adjustment stock movements must carry a strictly positive quantity.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
    { correspondenceId: "corr.supply.guard.currency.mismatch", fromTermId: "norn.supply.guard.currency.mismatch", relation: "subsumed_by", toTermIds: ["norn.finance.guard.currency.mismatch"], justification: "An order line must match its parent purchase order currency.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 9000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
  ],
  residuals: [
    { termId: "norn.supply.warehouse", reason: "canon_gap", note: "The canon ontology pack has no physical warehouse or storage-site class yet." },
    { termId: "norn.supply.inventory.snapshot", reason: "canon_gap", note: "The canon ontology pack has no semi-additive stock-position snapshot term yet (executable `canon.Holding` lives in `canon/records.ts`)." },
    { termId: "norn.supply.inventory.snapshot.quantity.basis", reason: "canon_gap", note: "The canon ontology pack has no `on_hand` / `available` / `reserved` quantity-basis partition term yet." },
    { termId: "norn.supply.purchase.order.supplier", reason: "canon_gap", note: "Canon `norn.commerce.order.party` models sales customers; purchase-commitment counterparty is not yet in the canon pack." },
    { termId: "norn.supply.inspection", reason: "canon_gap", note: "The canon ontology pack has no quality-inspection evidence class yet." },
    { termId: "norn.supply.demand.observation", reason: "canon_gap", note: "The canon ontology pack has no weekly demand-observation analytical input class." },
    { termId: "norn.supply.demand.forecast", reason: "canon_gap", note: "The canon ontology pack has no contract-checked ML demand-forecast prediction class." },
    { termId: "norn.supply.guard.unit.mismatch", reason: "canon_gap", note: "The canon ontology pack has a currency-mismatch guard but no unit-of-measure mismatch guard term yet." },
    { termId: "norn.supply.wms.count", reason: "system_specific", note: "Raw WMS scan feed evidence staging record kept separate from governed stock positions." },
    { termId: "norn.supply.supplier.receipt.line", reason: "system_specific", note: "Raw ERP supplier-feed line evidence kept separate from governed shipment receipts." },
  ],
})
