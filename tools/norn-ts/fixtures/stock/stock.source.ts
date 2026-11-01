import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Company = Norn.type({
  name: "stock.Company", title: "Company", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, name: Schema.String, currency: Schema.Literal("USD"), book_id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Warehouse = Norn.type({
  name: "stock.Warehouse", title: "Warehouse", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, name: Schema.String, stock_account: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "company", target: Company, cardinality: { to: "one", from: "many", required: true } }],
})
export const Item = Norn.type({
  name: "stock.Item", title: "Stock item", version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String, name: Schema.String, uom: Schema.String,
    valuation_method: Schema.Literals(["fifo", "moving-average"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})
export const Party = Norn.type({
  name: "stock.Party", title: "Stock counterparty", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, name: Schema.String, role: Schema.Literals(["supplier", "customer"]) }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Voucher = Norn.type({
  name: "stock.Voucher", title: "Stock voucher", version: "0.1.0",
  description: "Structural lifecycle only. Submit/cancel actions require declared atomic stock and GL posting laws.",
  properties: Schema.Struct({
    id: Schema.String, request_id: Schema.String, effective_at: Norn.timestamp("utc"),
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
  properties: Schema.Struct({ quantity: Norn.decimal(6), rate: Norn.decimal(6) }),
  links: [
    { id: "voucher", target: PurchaseReceipt, cardinality: { to: "one", from: "many", required: true } },
    { id: "destination", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const DeliveryLine = Norn.type({
  name: "stock.DeliveryLine", title: "Delivery line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.decimal(6) }),
  links: [
    { id: "voucher", target: DeliveryNote, cardinality: { to: "one", from: "many", required: true } },
    { id: "source", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const TransferLine = Norn.type({
  name: "stock.TransferLine", title: "Transfer line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.decimal(6) }),
  links: [
    { id: "voucher", target: StockEntry, cardinality: { to: "one", from: "many", required: true } },
    { id: "source", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
    { id: "destination", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const ReturnLine = Norn.type({
  name: "stock.ReturnLine", title: "Return line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ quantity: Norn.decimal(6) }),
  links: [
    { id: "voucher", target: SalesReturn, cardinality: { to: "one", from: "many", required: true } },
    { id: "original_line", target: DeliveryLine, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const ReconciliationLine = Norn.type({
  name: "stock.ReconciliationLine", title: "Reconciliation line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ counted_quantity: Norn.decimal(6), rate: Norn.decimal(6) }),
  links: [
    { id: "voucher", target: StockReconciliation, cardinality: { to: "one", from: "many", required: true } },
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})
