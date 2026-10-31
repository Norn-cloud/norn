import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Invoice = Norn.type({
  name: "boards.Invoice", title: "Invoice", version: "0.1.0",
  properties: Schema.Struct({ name: Schema.String, due: Norn.date(), currency: Schema.Literals(["EGP"]), open: Norn.decimal(2, { currency: "currency" }) }),
  identity: [{ name: "primary", properties: ["name"] }],
})
export const Open = Norn.metric({ id: "boards.open", title: "Open", over: Invoice, measure: { sum: "open" }, time: "due", owner: "boards" })
export const Board = Norn.board({
  id: "boards.bad", title: "Bad", version: "0.1.0", rows: Invoice, results: Invoice,
  panels: [{ id: "p", title: "P", metric: Open, groupBy: "region", window: { start: "1900-01-01T00:00:00Z", end: "2200-01-01T00:00:00Z" }, maxAgeMs: 60000 }],
})
