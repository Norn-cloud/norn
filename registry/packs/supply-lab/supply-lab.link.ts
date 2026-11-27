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
    { correspondenceId: "corr.supply.guard.movement.nonpositive", fromTermId: "norn.supply.guard.movement.nonpositive", relation: "subsumed_by", toTermIds: ["norn.finance.guard.amount.nonpositive"], justification: "Non-adjustment stock movements must carry a strictly positive quantity.", evidenceRefs: ["source.supply-lab"], confidenceBasisPoints: 8000, reviewedBy: "lane-a5 (agent, proposed)", reviewedAt: REVIEWED_AT },
  ],
  residuals: [
    { termId: "norn.supply.warehouse", reason: "canon_gap", note: "The canon ontology pack has no physical warehouse or storage-site class yet." },
    { termId: "norn.supply.inventory.snapshot", reason: "canon_gap", note: "The canon ontology pack has no semi-additive stock-position snapshot term yet (executable `canon.Holding` lives in `canon/records.ts`)." },
    { termId: "norn.supply.inventory.snapshot.quantity.basis", reason: "canon_gap", note: "The canon ontology pack has no `on_hand` / `available` / `reserved` quantity-basis partition term yet." },
    { termId: "norn.supply.stock.movement", reason: "canon_gap", note: "Supply Lab `StockMovement` admits signed and zero-quantity `adjustment` rows that need not change who holds a resource, so it is not strictly subsumed by canon `norn.core.event`." },
    { termId: "norn.supply.stock.movement.qty", reason: "canon_gap", note: "Signed movement/adjustment quantity on `StockMovement`; not strictly subsumed by `norn.core.resource.quantity` because zero-quantity adjustments are admitted." },
    { termId: "norn.supply.purchase.order", reason: "canon_gap", note: "Supply Lab `PurchaseOrder` covers unsubmitted `draft` orders (which may be cancelled before any promise is made) and headers submitted with zero lines, so it is not strictly subsumed by canon `norn.core.commitment` / `core.Commitment`." },
    { termId: "norn.supply.purchase.order.supplier", reason: "canon_gap", note: "Canon `norn.commerce.order.party` models sales customers; purchase-commitment counterparty is not yet in the canon pack." },
    { termId: "norn.supply.order.line", reason: "canon_gap", note: "Supply Lab `OrderLine` is created on `draft` purchase orders prior to submission, so it covers proposed lines before a promise is established and is not strictly subsumed by canon `norn.core.commitment`." },
    { termId: "norn.supply.shipment.receipt", reason: "canon_gap", note: "Supply Lab `ReceiptEvidence` permits zero-quantity receipts (`received_qty >= 0 && damaged_qty >= 0`), whereas canon `core.EconomicEvent` / `norn.core.event.transfer` requires strictly positive `quantity > 0`." },
    { termId: "norn.supply.shipment.receipt.received.qty", reason: "canon_gap", note: "Non-negative received quantity on `ShipmentReceipt`; not strictly subsumed by `norn.core.resource.quantity` because zero-quantity receipts are admitted." },
    { termId: "norn.supply.shipment.receipt.order.line", reason: "canon_gap", note: "Optional order-line reference on `ShipmentReceipt`; not strictly subsumed by `norn.core.event.fulfils` because `ShipmentReceipt` admits zero-quantity receipt records." },
    { termId: "norn.supply.inspection", reason: "canon_gap", note: "The canon ontology pack has no quality-inspection evidence class yet." },
    { termId: "norn.supply.demand.observation", reason: "canon_gap", note: "The canon ontology pack has no weekly demand-observation analytical input class." },
    { termId: "norn.supply.demand.forecast", reason: "canon_gap", note: "The canon ontology pack has no contract-checked ML demand-forecast prediction class." },
    { termId: "norn.supply.guard.currency.mismatch", reason: "canon_gap", note: "Checks order-line-to-purchase-order currency equality before any arithmetic combination, whereas canon `norn.finance.guard.currency.mismatch` is scoped to combining amounts in different currencies." },
    { termId: "norn.supply.guard.unit.mismatch", reason: "canon_gap", note: "The canon ontology pack has a currency-mismatch guard but no unit-of-measure mismatch guard term yet." },
    { termId: "norn.supply.wms.count", reason: "system_specific", note: "Raw WMS scan feed evidence staging record kept separate from governed stock positions." },
    { termId: "norn.supply.supplier.receipt.line", reason: "system_specific", note: "Raw ERP supplier-feed line evidence kept separate from governed shipment receipts." },
  ],
})
