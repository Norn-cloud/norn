import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Timed = Norn.interfaceType({ name: "boards.Timed", title: "Timed", version: "0.1.0",
  properties: Schema.Struct({ due: Norn.date(), customer: Schema.String }) })
const Classified = Norn.interfaceType({ name: "boards.Classified", title: "Classified", version: "0.1.0",
  extends: [Timed], properties: Schema.Struct({ state: Schema.String }) })
const Amount = Norn.type({ name: "boards.Amount", title: "Amount", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, open: Schema.Int }) })
export const Invoice = Norn.type({ name: "boards.Invoice", title: "Invoice", version: "0.1.0",
  extends: Amount, implements: [Classified], properties: Schema.Struct({ note: Schema.String }) })
export const Open = Norn.metric({ id: "boards.open", title: "Open", over: Invoice,
  measure: { sum: "open" }, time: "due", where: [{ property: "state", op: "eq", value: "submitted" }], owner: "boards" })
export const Board = Norn.board({ id: "boards.inherited", title: "Inherited", version: "0.1.0", rows: Invoice, results: Invoice,
  panels: [{ id: "customer", title: "Customer", metric: Open, groupBy: "customer",
    window: { start: "2026-01-01T00:00:00Z", end: "2027-01-01T00:00:00Z" }, maxAgeMs: 60000 }] })
