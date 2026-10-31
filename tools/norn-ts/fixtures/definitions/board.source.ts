// A typed metric board over one money-bearing type (Norn.metric / Norn.board).
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Invoice = Norn.type({
  name: "boards.Invoice", title: "Invoice", version: "0.1.0",
  properties: Schema.Struct({
    name: Schema.String, customer: Schema.String, state: Schema.Literals(["draft", "submitted"]),
    due: Norn.date(), currency: Schema.Literals(["EGP"]), open: Norn.decimal(2, { currency: "currency" }),
  }),
  identity: [{ name: "primary", properties: ["name"] }],
})

export const Row = Norn.type({
  name: "boards.Row", title: "Row", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, buckets: Norn.json() }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Open = Norn.metric({
  id: "boards.open", title: "Open", over: Invoice, measure: { sum: "open" }, time: "due",
  where: [{ property: "state", op: "eq", value: "submitted" }], owner: "boards",
})

const always = { start: "1900-01-01T00:00:00Z", end: "2200-01-01T00:00:00Z" }

export const Board = Norn.board({
  id: "boards.open_board", title: "Open", version: "0.1.0", rows: Row, results: Row,
  panels: [
    { id: "total", title: "Total", metric: Open, window: always, maxAgeMs: 60000 },
    { id: "by_customer", title: "By customer", metric: Open, groupBy: "customer", window: always, maxAgeMs: 60000 },
  ],
})
