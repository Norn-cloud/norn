import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Company = Norn.type({
  name: "stock.Company", title: "Company", version: "0.1.0",
  implements: ["core.Agent"],
  properties: Schema.Struct({ id: Norn.ref("core.code"), display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]), currency: Norn.ref("core.code"), book_id: Norn.ref("core.code") }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Warehouse = Norn.type({
  name: "stock.Warehouse", title: "Warehouse", version: "0.1.0",
  properties: Schema.Struct({ id: Norn.ref("core.code"), name: Norn.ref("core.display_name"), stock_account: Norn.ref("core.code") }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "company", target: Company, cardinality: { to: "one", from: "many", required: true } }],
})
export const Item = Norn.type({
  name: "stock.Item", title: "Stock item", version: "0.1.0",
  properties: Schema.Struct({
    id: Norn.ref("core.code"), name: Norn.ref("core.display_name"), uom: Norn.ref("ucum.unit_code"),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Party = Norn.type({
  name: "stock.Party", title: "Stock counterparty", version: "0.1.0",
  implements: ["core.Agent"],
  properties: Schema.Struct({ id: Norn.ref("core.code"), display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]) }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Voucher = Norn.type({
  name: "stock.Voucher", title: "Stock voucher", version: "0.1.0",
  description: "Structural lifecycle only. Submit/cancel actions require declared atomic stock and GL posting laws.",
  properties: Schema.Struct({
    id: Norn.ref("core.document_number"), request_id: Norn.ref("core.code"), effective_at: Norn.ref("iso8601.timestamp"),
    status: Schema.Literals(["draft", "submitted", "cancelled"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }], defaults: { status: "draft" },
  links: [
    { id: "company", target: Company, cardinality: { to: "one", from: "many", required: true } },
    { id: "amended_from", target: "stock.Voucher", cardinality: { to: "one", from: "many" }, acyclic: true },
  ],
  lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["cancelled"],
    transitions: [
      { id: "submit", from: ["draft"], to: "submitted" },
      { id: "cancel", from: ["submitted"], to: "cancelled" },
    ],
  }],
})
export const PurchaseReceipt = Norn.type({
  name: "stock.PurchaseReceipt", title: "Purchase Receipt", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
  links: [{ id: "supplier", target: Party, cardinality: { to: "one", from: "many", required: true } }],
})
export const DeliveryNote = Norn.type({
  name: "stock.DeliveryNote", title: "Delivery Note", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
  links: [{ id: "customer", target: Party, cardinality: { to: "one", from: "many", required: true } }],
})
export const StockEntry = Norn.type({
  name: "stock.StockEntry", title: "Stock Entry (transfer)", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
})
export const SalesReturn = Norn.type({
  name: "stock.SalesReturn", title: "Sales Return", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
  links: [{ id: "delivery", target: DeliveryNote, cardinality: { to: "one", from: "many", required: true } }],
})
export const StockReconciliation = Norn.type({
  name: "stock.StockReconciliation", title: "Stock Reconciliation", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
})

export const VoucherLine = Norn.type({
  name: "stock.VoucherLine", title: "Stock voucher line", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, line_number: Schema.Int }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } }],
})
export const ReceiptLine = Norn.type({
  name: "stock.ReceiptLine", title: "Receipt line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.ref("core.quantity"), rate: Norn.ref("core.money_amount") }),
  links: [
    { id: "voucher", target: PurchaseReceipt, cardinality: { to: "one", from: "many", required: true } },
    { id: "destination", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const DeliveryLine = Norn.type({
  name: "stock.DeliveryLine", title: "Delivery line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.ref("core.quantity") }),
  links: [
    { id: "voucher", target: DeliveryNote, cardinality: { to: "one", from: "many", required: true } },
    { id: "source", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const TransferLine = Norn.type({
  name: "stock.TransferLine", title: "Transfer line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.ref("core.quantity") }),
  links: [
    { id: "voucher", target: StockEntry, cardinality: { to: "one", from: "many", required: true } },
    { id: "source", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
    { id: "destination", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const ReturnLine = Norn.type({
  name: "stock.ReturnLine", title: "Return line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.ref("core.quantity") }),
  links: [
    { id: "voucher", target: SalesReturn, cardinality: { to: "one", from: "many", required: true } },
    { id: "original_line", target: DeliveryLine, cardinality: { to: "one", from: "many", required: true } },
    { id: "destination", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const ReconciliationLine = Norn.type({
  name: "stock.ReconciliationLine", title: "Reconciliation line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ counted_quantity: Norn.ref("core.quantity"), target_value: Norn.ref("core.money_amount") }),
  links: [
    { id: "voucher", target: StockReconciliation, cardinality: { to: "one", from: "many", required: true } },
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const Holding = Norn.type({
  name: "stock.Holding", title: "Stock holding", version: "0.1.0", implements: ["core.Resource"],
  properties: Schema.Struct({
    resource_code: Norn.ref("core.code"), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
    valuation_method: Schema.NullOr(Schema.Literals(["fifo", "moving_average"])),
  }),
  identity: [{ name: "primary", properties: ["resource_code"] }],
  links: [
    { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } },
    { id: "location", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", target: "core.Agent", cardinality: { to: "one", from: "many" } },
  ],
})
export const OrderLine = Norn.type({
  name: "stock.OrderLine", title: "Order commitment", version: "0.1.0", implements: ["core.Commitment"],
  properties: Schema.Struct({
    status: Schema.Literals(["requested", "promised", "fulfilled", "cancelled", "breached"]),
    due_at: Norn.ref("iso8601.timestamp"), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "provider", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", target: Holding, cardinality: { to: "one", from: "many" } },
    { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const Reservation = Norn.type({
  name: "stock.Reservation", title: "Stock reservation", version: "0.1.0", implements: ["core.Hold"],
  properties: Schema.Struct({
    status: Schema.Literals(["open", "released", "consumed", "expired"]), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
    expires_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
  }),
  links: [
    { id: "resource", target: Holding, cardinality: { to: "one", from: "many", required: true } },
    { id: "holder", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "secures", target: OrderLine, cardinality: { to: "one", from: "many" } },
  ],
})
export const Movement = Norn.type({
  name: "stock.Movement", title: "Stock movement", version: "0.1.0", implements: ["core.EconomicEvent"],
  properties: Schema.Struct({
    action: Schema.Literals(["transfer_ownership", "transfer_custody", "consume", "produce"]),
    occurred_at: Norn.ref("iso8601.timestamp"), quantity: Norn.ref("core.quantity"),
    unit: Schema.NullOr(Norn.ref("ucum.unit_code")), currency: Schema.NullOr(Norn.ref("core.code")),
  }),
  links: [
    { id: "provider", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "receiver", target: "core.Agent", cardinality: { to: "one", from: "many", required: true } },
    { id: "resource", target: Holding, cardinality: { to: "one", from: "many", required: true } },
    { id: "fulfils", target: OrderLine, cardinality: { to: "one", from: "many" } },
    { id: "reverses", target: "stock.Movement", cardinality: { to: "one", from: "one" } },
    { id: "voucher", target: Voucher, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const GLEntry = Norn.type({
  name: "stock.GLEntry", title: "Signed GL entry", version: "0.1.0", implements: ["core.LedgerEffect"],
  properties: Schema.Struct({
    account_code: Norn.ref("core.code"), amount: Norn.ref("core.money_amount"), currency: Norn.ref("core.code"),
  }),
  links: [{ id: "event", target: Movement, cardinality: { to: "one", from: "many", required: true } }],
})
