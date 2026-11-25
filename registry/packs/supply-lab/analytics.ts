import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { DemandForecast, DemandObservation, OrderLine, ShipmentReceipt, StockMovement } from "./supply.ts"

// Derived datasets and the demand model. Questions and the dashboard live in
// `questions/` (the question compiler; they are not installable artifacts).

/** Weekly issued demand per SKU and warehouse: the forecasting feature table. */
export const WeeklyDemandFeatures = Norn.transform({
  id: "supply.weekly_demand_features", title: "Weekly demand features", version: "0.1.0",
  inputs: { movement: StockMovement },
  nodes: {
    movement: { op: "scan", input: "movement" },
    issues: { op: "filter", input: "movement", predicate: "input.movement_kind == \"issue\"" },
    weekly: {
      op: "aggregate", input: "issues",
      groupBy: [{ column: "sku_id" }, { column: "warehouse_id" }, { column: "unit" }, { column: "occurred_at", grain: "week", label: "Week" }],
      measures: [{ name: "issued_qty", fn: "sum", column: "qty", label: "Issued" }, { name: "issues", fn: "count" }],
    },
    lagged: {
      op: "window", input: "weekly", partitionBy: ["sku_id", "warehouse_id", "unit"],
      orderBy: [{ column: "occurred_at", direction: "asc" }],
      columns: [{ name: "week_number", fn: "row_number" }, { name: "previous_week_qty", fn: "lag", column: "issued_qty", offset: 1 }, { name: "to_date_qty", fn: "running_sum", column: "issued_qty" }],
    },
  },
  output: "lagged",
})

/**
 * Supplier fill per order line, chasm-trap safe: receipts are summed to their
 * line first, then joined once to the line, so a line with three deliveries is
 * counted once and its ordered quantity is never multiplied by its receipts.
 */
export const LineFill = Norn.transform({
  id: "supply.line_fill", title: "Order-line fill", version: "0.1.0",
  inputs: { receipt: ShipmentReceipt, line: OrderLine },
  nodes: {
    receipt: { op: "scan", input: "receipt" },
    line: { op: "scan", input: "line" },
    linked: { op: "filter", input: "receipt", predicate: "input.line_id != null" },
    per_line: {
      op: "aggregate", input: "linked",
      groupBy: [{ column: "line_id" }, { column: "unit" }],
      measures: [{ name: "received", fn: "sum", column: "received_qty" }, { name: "damaged", fn: "sum", column: "damaged_qty" }, { name: "deliveries", fn: "count" }],
    },
    joined: { op: "join", left: "per_line", right: "line", on: { key: { left: ["line_id"], right: "primary" } }, kind: "inner", prefix: "l_" },
    fill: {
      op: "project", input: "joined", columns: [
        { name: "line_id", expression: "input.line_id" },
        { name: "sku_id", expression: "input.l_sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "ordered", expression: "input.l_ordered_qty" },
        { name: "received", expression: "input.received" },
        { name: "accepted", expression: "input.received - input.damaged" },
        { name: "deliveries", expression: "input.deliveries" },
      ],
    },
  },
  output: "fill",
})

/**
 * Received quantity by the week each receipt's order line was promised. A
 * receipt keeps its own time (METRIC-LINK-OWN-FIRST derives no linked-time
 * metric for it), so bucketing it by its line's time is a derived dataset:
 * each linked receipt joins its one line (the line's primary key, no fan-out),
 * then sums by the promised week, per unit.
 */
export const ReceiptsByPromisedWeek = Norn.transform({
  id: "supply.receipts_by_promised_week", title: "Receipts by promised week", version: "0.1.0",
  inputs: { receipt: ShipmentReceipt, line: OrderLine },
  nodes: {
    receipt: { op: "scan", input: "receipt" },
    line: { op: "scan", input: "line" },
    linked: { op: "filter", input: "receipt", predicate: "input.line_id != null" },
    joined: { op: "join", left: "linked", right: "line", on: { key: { left: ["line_id"], right: "primary" } }, kind: "inner", prefix: "l_" },
    weekly: {
      op: "aggregate", input: "joined",
      groupBy: [{ column: "l_promised_at", grain: "week", label: "Promised week" }, { column: "unit" }],
      measures: [{ name: "received", fn: "sum", column: "received_qty", label: "Received" }],
    },
  },
  output: "weekly",
})

/** Next-week demand per SKU and warehouse; internals opaque, outputs contract-checked. */
export const DemandModel = Norn.model({
  id: "supply.demand_forecast", title: "Weekly demand forecast", version: "0.1.0",
  input: DemandObservation,
  features: ["week_index", "lead_time_days", "promotion"],
  target: "issued_qty",
  outputs: Schema.Struct({ predicted_qty: Norn.decimal(6), stockout_risk: Schema.Literals(["low", "medium", "high"]) }),
  trust: "contract_checked",
  contracts: [
    { id: "non_negative", output: "predicted_qty", rule: { kind: "range", min: "0" } },
    { id: "finite", output: "predicted_qty", rule: { kind: "finite" } },
    { id: "risk_band", output: "stockout_risk", rule: { kind: "oneOf", values: ["low", "medium", "high"] } },
  ],
  predictions: DemandForecast,
  method: { label: "gradient-boosted trees over 12 weeks of issues", version: "synthetic-1" },
})
