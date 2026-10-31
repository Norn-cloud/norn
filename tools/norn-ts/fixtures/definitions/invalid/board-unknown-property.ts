import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Invoice = Norn.type({
  name: "boards.Invoice", title: "Invoice", version: "0.1.0",
  properties: Schema.Struct({ name: Schema.String, due: Norn.date(), currency: Schema.Literals(["EGP"]), open: Norn.decimal(2, { currency: "currency" }) }),
  identity: [{ name: "primary", properties: ["name"] }],
})
export const Open = Norn.metric({ id: "boards.open", title: "Open", over: Invoice, measure: { sum: "missing_amount" }, time: "due", owner: "boards" })
