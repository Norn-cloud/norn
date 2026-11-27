import { Norn } from "@norn/authoring"
import { PosSale, PosSaleEvidence, StockLot, StoreCommercialRollup, StoreCount } from "./retail.ts"

// Declared commercial KPI questions for the S1 Retail Truth Slice over the
// 16-week retail truth window (2026-07-06 .. 2026-10-26). Every question is
// chasm-trap safe, partitioned by exact unit/currency/basis, and admitted
// against the Rust checker's derived metric space. Cost-dependent questions
// group explicitly by `cost_status` (never silently filtering uncosted rows) so
// mixed buckets remain separated while declared transforms (`StoreSalesPerformance`,
// `StoreLotAging`) null-withhold incomplete groups in IR; declarative feed/count
// source freshness wires into B6 (`#1905` `RequiredSources`) once merged.

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
 * Store gross margin amount partitioned by `cost_status` so mixed buckets with
 * any `missing_cost` sale fail closed (withheld) rather than reporting a partial total.
 */
export const StoreGrossMarginWeekly = Norn.question({
  name: "supply.retail.store_gross_margin_weekly", of: PosSale,
  measure: Norn.metricId({ kind: "sum", of: PosSale, property: "gross_margin", time: "sold_at" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_id" }, { property: "cost_status" }, { property: "sold_at", grain: "week" }],
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

/**
 * Immutable receipt-time lot opening valuation partitioned by `cost_status` so
 * GMROI opening denominators use each lot's actual receipt-time cost rather than
 * current SKU unit cost, while failing closed on any `missing_cost` slice.
 */
export const StoreOpeningStockValueWeekly = Norn.question({
  name: "supply.retail.store_opening_stock_value_weekly", of: StockLot,
  measure: Norn.metricId({ kind: "sum", of: StockLot, property: "opening_stock_value", time: "as_of" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_id" }, { property: "cost_status" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Store sell-through: declared ratio of sums `sum(depleted_qty) / sum(received_qty)` across complete lot cohorts. */
export const StoreSellThroughWeekly = Norn.question({
  name: "supply.retail.store_sell_through_weekly", of: StockLot,
  measure: Norn.metricId({ kind: "ratio", of: StockLot, property: "sell_through_rate", time: "as_of" }),
  where: [],
  by: [{ property: "unit" }, { property: "store_id" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Store-SKU weeks of supply: declared ratio of sums `sum(on_hand_qty) / sum(avg_weekly_sales_qty)` at unique store-SKU-week grain (null when withheld). */
export const StoreWeeksOfSupplyWeekly = Norn.question({
  name: "supply.retail.store_weeks_of_supply_weekly", of: StoreCommercialRollup,
  measure: Norn.metricId({ kind: "ratio", of: StoreCommercialRollup, property: "weeks_of_supply", time: "period_end" }),
  where: [],
  by: [{ property: "unit" }, { property: "store_id" }, { property: "sku_id" }, { property: "period_end", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Store-SKU GMROI: declared ratio of sums `sum(period_gross_margin) / sum(average_inventory_value)` at unique store-SKU-week grain (null when withheld). */
export const StoreGmroiWeekly = Norn.question({
  name: "supply.retail.store_gmroi_weekly", of: StoreCommercialRollup,
  measure: Norn.metricId({ kind: "ratio", of: StoreCommercialRollup, property: "gmroi", time: "period_end" }),
  where: [],
  by: [{ property: "currency" }, { property: "store_id" }, { property: "sku_id" }, { property: "period_end", grain: "week" }],
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

/**
 * Declared read-only Retail Combined Management View dashboard: projects all
 * store-level commercial KPIs side-by-side without write authority or statutory
 * consolidation claims (`READ-LOCAL-1`).
 */
export const RetailCombinedManagementView = Norn.dashboard({
  title: "Retail combined management view (read-only; not consolidated financial statements)",
  tiles: [
    { title: "Store availability, weekly", question: StoreAvailabilityWeekly, view: "bar" },
    { title: "Store on-hand, weekly", question: StoreOnHandWeekly, view: "line" },
    { title: "Store sell-through, weekly", question: StoreSellThroughWeekly, view: "line" },
    { title: "Lot receipt aging cohorts, weekly", question: LotReceivedAgingWeekly, view: "bar" },
    { title: "Store stock valuation, weekly", question: StoreStockValueWeekly, view: "area" },
    { title: "Store net revenue, weekly", question: StoreRevenueWeekly, view: "bar" },
    { title: "Store gross margin, weekly", question: StoreGrossMarginWeekly, view: "bar" },
    { title: "Store gross margin rate, weekly", question: StoreGrossMarginRateWeekly, view: "line" },
    { title: "Store weeks of supply, weekly", question: StoreWeeksOfSupplyWeekly, view: "line" },
    { title: "Store GMROI, weekly", question: StoreGmroiWeekly, view: "line" },
    { title: "POS feed revenue, weekly", question: PosFeedRevenueWeekly, view: "bar" },
  ],
})
