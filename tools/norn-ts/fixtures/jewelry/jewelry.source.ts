import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Item, Voucher, VoucherLine, Party, Warehouse } from "../stock/stock.source"

export const MetalLot = Norn.type({
  name: "jewelry.MetalLot", title: "Metal lot", version: "0.1.0", extends: Item,
  properties: Schema.Struct({
    metal: Schema.String, gross_micrograms: Norn.decimal(0),
    purity_numerator: Norn.decimal(0), fine_ticks: Norn.decimal(0),
    ownership: Schema.Literals(["company", "custody"]),
  }),
  links: [
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
    { id: "custody_owner", target: Party, cardinality: { to: "one", from: "many" } },
  ],
})
export const CustodyReceipt = Norn.type({
  name: "jewelry.CustodyReceipt", title: "Custody receipt", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
  links: [{ id: "owner", target: Party, cardinality: { to: "one", from: "many", required: true } }],
})
export const CustodyReturn = Norn.type({
  name: "jewelry.CustodyReturn", title: "Custody return", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
  links: [
    { id: "owner", target: Party, cardinality: { to: "one", from: "many", required: true } },
    { id: "receipt", target: CustodyReceipt, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const CustodyReceiptLine = Norn.type({
  name: "jewelry.CustodyReceiptLine", title: "Custody receipt line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({}),
  links: [
    { id: "voucher", target: CustodyReceipt, cardinality: { to: "one", from: "many", required: true } },
    { id: "lot", target: MetalLot, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const CustodyReturnLine = Norn.type({
  name: "jewelry.CustodyReturnLine", title: "Custody return line", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({}),
  links: [
    { id: "voucher", target: CustodyReturn, cardinality: { to: "one", from: "many", required: true } },
    { id: "original_line", target: CustodyReceiptLine, cardinality: { to: "one", from: "many", required: true } },
    { id: "lot", target: MetalLot, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const Refining = Norn.type({
  name: "jewelry.Refining", title: "Refining", version: "0.1.0", extends: Voucher,
  properties: Schema.Struct({}),
})
export const RefiningInput = Norn.type({
  name: "jewelry.RefiningInput", title: "Refining input", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ fine_ticks: Norn.decimal(0) }),
  links: [
    { id: "voucher", target: Refining, cardinality: { to: "one", from: "many", required: true } },
    { id: "lot", target: MetalLot, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const RefiningOutput = Norn.type({
  name: "jewelry.RefiningOutput", title: "Refining output", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({ fine_ticks: Norn.decimal(0) }),
  links: [
    { id: "voucher", target: Refining, cardinality: { to: "one", from: "many", required: true } },
    { id: "lot", target: MetalLot, cardinality: { to: "one", from: "many", required: true } },
  ],
})
export const RefiningLoss = Norn.type({
  name: "jewelry.RefiningLoss", title: "Recorded refining loss", version: "0.1.0", extends: VoucherLine,
  properties: Schema.Struct({
    fine_ticks: Norn.decimal(0), reason: Schema.String,
    company_value: Norn.decimal(6), process_loss_account: Schema.String,
  }),
  links: [
    { id: "voucher", target: Refining, cardinality: { to: "one", from: "many", required: true } },
    { id: "lot", target: MetalLot, cardinality: { to: "one", from: "many", required: true } },
  ],
})
