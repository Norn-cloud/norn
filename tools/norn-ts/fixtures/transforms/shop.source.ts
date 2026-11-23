// ViewIR v1 authoring fixture (#341): two synthetic types and one transform
// over them: revenue by customer region per ISO week, ranked within the region.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "shop.Customer", title: "Customer", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String, region: Schema.String }),
  identity: [{ name: "email", properties: ["email"] }],
})

export const Order = Norn.type({
  name: "shop.Order", title: "Order", version: "0.1.0",
  properties: Schema.Struct({
    amount: Norn.decimal(2, { additivity: { kind: "additive" } }),
    placed_at: Norn.timestamp("utc"),
    note: Schema.NullOr(Schema.String),
  }),
  links: [{ id: "customer", target: Customer, cardinality: { to: "one", from: "many" } }],
})

export const RevenueByRegion = Norn.transform({
  id: "shop.revenue_by_region",
  title: "Revenue by region",
  version: "0.1.0",
  inputs: { order: Order, customer: Customer },
  nodes: {
    order: { op: "scan", input: "order" },
    customer: { op: "scan", input: "customer" },
    paid: { op: "filter", input: "order", predicate: "input.amount > 0" },
    joined: { op: "join", left: "paid", right: "customer", on: { link: { column: "id", link: "customer" } }, kind: "left", prefix: "c_" },
    regional: {
      op: "project", input: "joined", columns: [
        { name: "id", expression: "input.id" },
        { name: "region", expression: "input.c_region ?? \"unknown\"" },
        { name: "amount", expression: "input.amount" },
        { name: "placed_at", expression: "input.placed_at" },
      ],
    },
    weekly: {
      op: "aggregate", input: "regional",
      groupBy: [{ column: "region" }, { column: "placed_at", grain: "week" }],
      measures: [{ name: "revenue", fn: "sum", column: "amount" }, { name: "orders", fn: "count" }],
    },
    ranked: {
      op: "window", input: "weekly", partitionBy: ["region"],
      orderBy: [{ column: "placed_at", direction: "asc" }],
      columns: [{ name: "week_number", fn: "row_number" }, { name: "to_date", fn: "running_sum", column: "revenue" }],
    },
  },
  output: "ranked",
})
