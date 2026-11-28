import { Norn } from "@norn/authoring"
import { PosSale } from "../supply-lab/retail.ts"
import { OtbEnvelope, OtbPoBinding, PlanLine, PublishedForecast } from "./plan.ts"

// Declared planning, OTB, and variance questions over the Q3–Q4 2026 retail
// planning horizon (2026-07-01 .. 2027-01-01). Every question partitions
// strictly by currency/unit (`METRIC-UNIT-PARTITION-1`), keeps semi-additive
// EOM stock on the `as_of` snapshot contract (`METRIC-SNAPSHOT-ASOF-1`), and
// pins `plan_version_id` / `model_version` / `forecast_revision`.

const window = { basis: "effective", from: "2026-07-01T00:00:00Z", to: "2027-01-01T00:00:00Z", freshness: { kind: "live", maxAgeMs: 60000 } } as const

/** Planned retail sales at full plan grain (entity x store x category x brand x ISO month) per PlanVersion. */
export const PlannedSalesRetailMonthly = Norn.question({
  name: "plan.planned_sales_retail_monthly", of: PlanLine,
  measure: Norn.metricId({ kind: "sum", of: PlanLine, property: "planned_sales_retail", time: "period_start" }),
  where: [],
  by: [
    { property: "plan_version_id" }, { property: "entity_id" }, { property: "store_id" },
    { property: "category" }, { property: "brand" }, { property: "currency" },
    { property: "period_start", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Planned purchase-at-cost receipts budget at full plan grain per PlanVersion. */
export const PlannedReceiptsCostMonthly = Norn.question({
  name: "plan.planned_receipts_cost_monthly", of: PlanLine,
  measure: Norn.metricId({ kind: "sum", of: PlanLine, property: "planned_receipts_cost", time: "period_start" }),
  where: [],
  by: [
    { property: "plan_version_id" }, { property: "entity_id" }, { property: "store_id" },
    { property: "category" }, { property: "brand" }, { property: "currency" },
    { property: "period_start", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Planned markdowns at cost per plan version, entity, store, category, brand, currency, and ISO month. */
export const PlannedMarkdownsCostMonthly = Norn.question({
  name: "plan.planned_markdowns_cost_monthly", of: PlanLine,
  measure: Norn.metricId({ kind: "sum", of: PlanLine, property: "planned_markdowns_cost", time: "period_start" }),
  where: [],
  by: [
    { property: "plan_version_id" }, { property: "entity_id" }, { property: "store_id" },
    { property: "category" }, { property: "brand" }, { property: "currency" },
    { property: "period_start", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Planned fine-metal grams (UCUM `g`, #1794) per plan version, entity, store, category, brand, and ISO month. */
export const PlannedFineMetalMonthly = Norn.question({
  name: "plan.planned_fine_metal_monthly", of: PlanLine,
  measure: Norn.metricId({ kind: "sum", of: PlanLine, property: "planned_fine_metal_g", time: "period_start" }),
  where: [],
  by: [
    { property: "plan_version_id" }, { property: "entity_id" }, { property: "store_id" },
    { property: "category" }, { property: "brand" }, { property: "period_start", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Planned EOM stock at cost: semi-additive snapshot (`METRIC-SNAPSHOT-ASOF-1`), never summed over time. */
export const PlannedEomStockCostMonthly = Norn.question({
  name: "plan.planned_eom_stock_cost_monthly", of: PlanLine,
  measure: Norn.metricId({ kind: "as_of", of: PlanLine, property: "planned_eom_stock_cost", time: "as_of" }),
  where: [],
  by: [{ property: "plan_version_id" }, { property: "store_id" }, { property: "currency" }, { property: "as_of", grain: "month" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Approved OTB budget envelopes (active) per plan version, store, category, brand, currency, and ISO month. */
export const OtbApprovedBudgetMonthly = Norn.question({
  name: "plan.otb_approved_budget_monthly", of: OtbEnvelope,
  measure: Norn.metricId({ kind: "sum", of: OtbEnvelope, property: "approved_budget_cost", time: "period_start" }),
  where: [{ property: "status", operator: "eq", value: "active" }],
  by: [
    { property: "plan_version_id" }, { property: "store_id" }, { property: "category" },
    { property: "brand" }, { property: "currency" }, { property: "period_start", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Recognized receipts at cost against OTB envelopes per envelope and currency. */
export const OtbRecognizedReceiptsMonthly = Norn.question({
  name: "plan.otb_recognized_receipts_monthly", of: OtbPoBinding,
  measure: Norn.metricId({ kind: "sum", of: OtbPoBinding, property: "recognized_receipt_cost", time: "as_of" }),
  where: [],
  by: [{ property: "envelope_id" }, { property: "currency" }, { property: "as_of", grain: "month" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Unreceived authorized PO value against OTB envelopes per envelope and currency. */
export const OtbUnreceivedPoMonthly = Norn.question({
  name: "plan.otb_unreceived_po_monthly", of: OtbPoBinding,
  measure: Norn.metricId({ kind: "sum", of: OtbPoBinding, property: "unreceived_authorized_cost", time: "as_of" }),
  where: [],
  by: [{ property: "envelope_id" }, { property: "currency" }, { property: "as_of", grain: "month" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Actual POS net revenue bucketed by entity, store, category, currency, and ISO month (supports `AsOfReader` knowledge cutoffs for `Actual - Plan`). */
export const ActualPosRevenueMonthly = Norn.question({
  name: "plan.actual_pos_revenue_monthly", of: PosSale,
  measure: Norn.metricId({ kind: "sum", of: PosSale, property: "net_revenue", time: "sold_at" }),
  where: [],
  by: [
    { link: "store", property: "entity_id" }, { property: "store_id" },
    { link: "sku", property: "category" }, { property: "currency" },
    { property: "sold_at", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Actual POS sold units bucketed by entity, store, category, unit, and ISO month. */
export const ActualPosUnitsMonthly = Norn.question({
  name: "plan.actual_pos_units_monthly", of: PosSale,
  measure: Norn.metricId({ kind: "sum", of: PosSale, property: "sold_qty", time: "sold_at" }),
  where: [],
  by: [
    { link: "store", property: "entity_id" }, { property: "store_id" },
    { link: "sku", property: "category" }, { property: "unit" },
    { property: "sold_at", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

/**
 * Published (including superseded) plan-grain forecast sales retail pinning
 * plan version, model version, and forecast revision (`status != 'draft'`), so
 * pinned historical forecast queries remain reproducible after supersession.
 */
export const PublishedForecastSalesMonthly = Norn.question({
  name: "plan.published_forecast_sales_monthly", of: PublishedForecast,
  measure: Norn.metricId({ kind: "sum", of: PublishedForecast, property: "forecast_sales_retail", time: "period_start" }),
  where: [{ property: "status", operator: "ne", value: "draft" }],
  by: [
    { property: "plan_version_id" }, { property: "model_version" }, { property: "forecast_revision" },
    { property: "entity_id" }, { property: "store_id" }, { property: "category" }, { property: "brand" },
    { property: "currency" }, { property: "period_start", grain: "month" },
  ],
  asOf: window, population: { kind: "authorized_objects" },
})

export const PlanOtbDashboard = Norn.dashboard({
  title: "Merchandise planning & Open-to-Buy",
  tiles: [
    { title: "Planned retail sales, monthly", question: PlannedSalesRetailMonthly, view: "bar" },
    { title: "Planned purchase budget at cost, monthly", question: PlannedReceiptsCostMonthly, view: "bar" },
    { title: "Planned EOM stock at cost, monthly", question: PlannedEomStockCostMonthly, view: "line" },
    { title: "Approved OTB budget, monthly", question: OtbApprovedBudgetMonthly, view: "bar" },
    { title: "Actual POS revenue, monthly", question: ActualPosRevenueMonthly, view: "bar" },
    { title: "Published forecast sales, monthly", question: PublishedForecastSalesMonthly, view: "line" },
  ],
})
