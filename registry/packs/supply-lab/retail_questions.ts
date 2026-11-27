import { Norn } from "@norn/authoring"
import { PosSale, PosSaleEvidence, StockLot, StoreCount } from "./retail.ts"

// Declared commercial KPI questions for the S1 Retail Truth Slice over the
// 16-week retail truth window (2026-07-06 .. 2026-10-26). Every question is
// chasm-trap safe, partitioned by exact unit/currency/basis, and admitted
// against the Rust checker's derived metric space. Cost-dependent questions
// group explicitly by `cost_status` (never silently filtering uncosted rows) so
// mixed buckets fail closed and withhold instead of publishing partial totals.

const window = { basis: "effective", from: "2026-07-06T00:00:00Z", to: "2026-10-26T00:00:00Z", freshness: { kind: "live", maxAgeMs: 60000 } } as const

/** Store availability: semi-additive as-of available balance per store, unit, and week. */
export const StoreAvailabilityWeekly = Norn.question({
  name: "supply.retail.store_availability_weekly", of: StoreCount,
  measure: Norn.metricId({ kind: "as_of", of: StoreCount, property: "on_hand_qty", time: "as_of" }),
  where: [{ property: "quantity_basis", operator: "eq", value: "available" }],
  by: [{ property: "unit" }, { property: "store_id" }, { property: "as_of", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Derived lot on-hand balance: semi-additive as-of remaining lot quantity per store, unit, and week. */
export const StoreOnHandWeekly = Norn.question({
  name: "supply.retail.store_on_hand_weekly", of: StockLot,
  measure: Norn.metricId({ kind: "as_of", of: StockLot, property: "remaining_qty", time: "as_of" }),
  where: [],
  by: [{ property: "unit" }, { property: "store_id" }, { property: "as_of", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/**
 * Holding stock valuation partitioned by `cost_status`: a bucket with any
 * `missing_cost` slice stays visible in the exact answer so consumers fail
 * closed and withhold valuation/GMROI instead of publishing a partial total.
 */
export const StoreStockValueWeekly = Norn.question({
  name: "supply.retail.store_stock_value_weekly", of: StockLot,
  measure: Norn.metricId({ kind: "as_of", of: StockLot, property: "stock_value", time: "as_of" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_id" }, { property: "cost_status" }, { property: "as_of", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Holding lot receipt cohorts: additive received volume bucketed by original `received_at` week. */
export const LotReceivedAgingWeekly = Norn.question({
  name: "supply.retail.lot_received_aging_weekly", of: StockLot,
  measure: Norn.metricId({ kind: "sum", of: StockLot, property: "received_qty", time: "received_at" }),
  where: [],
  by: [{ property: "unit" }, { property: "store_id" }, { property: "received_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Store unit sales: additive POS sold quantity per store, unit, and week. */
export const StoreSalesWeekly = Norn.question({
  name: "supply.retail.store_sales_weekly", of: PosSale,
  measure: Norn.metricId({ kind: "sum", of: PosSale, property: "sold_qty", time: "sold_at" }),
  where: [],
  by: [{ property: "unit" }, { property: "store_id" }, { property: "sold_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Store net revenue: additive POS revenue per store, currency, and week. */
export const StoreRevenueWeekly = Norn.question({
  name: "supply.retail.store_revenue_weekly", of: PosSale,
  measure: Norn.metricId({ kind: "sum", of: PosSale, property: "net_revenue", time: "sold_at" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_id" }, { property: "sold_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/**
 * Store gross margin rate: ratio of sums `sum(gross_margin) / sum(net_revenue)`
 * partitioned by `cost_status` so mixed buckets with any `missing_cost` sale
 * remain visible and fail closed (withheld) rather than reporting a partial rate.
 */
export const StoreGrossMarginRateWeekly = Norn.question({
  name: "supply.retail.store_gross_margin_rate_weekly", of: PosSale,
  measure: Norn.metricId({ kind: "ratio", of: PosSale, property: "gross_margin_rate", time: "sold_at" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_id" }, { property: "cost_status" }, { property: "sold_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Mapped POS feed revenue over `PosSaleEvidence` for end-to-end feed quarantine and `LINEAGE-SUM-1` source drill-through. */
export const PosFeedRevenueWeekly = Norn.question({
  name: "supply.retail.pos_feed_revenue_weekly", of: PosSaleEvidence,
  measure: Norn.metricId({ kind: "sum", of: PosSaleEvidence, property: "net_revenue", time: "sold_at" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_code" }, { property: "sold_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})
