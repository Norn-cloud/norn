import { Norn } from "@norn/authoring"
import { Sku } from "../supply-lab/supply.ts"
import { PosSale, Store } from "../supply-lab/retail.ts"
import { OtbEnvelope, OtbPoBinding, PlanLine, PublishedForecast } from "./plan.ts"

// Chasm-trap-safe planning, OTB, and variance transforms (`LineFill` pattern):
// every transform aggregates its fact stream to the target grain FIRST, then
// joins once onto the right-hand entity's unique key so multi-row facts never
// multiply budgets or plan lines. Every monetary or physical measure preserves
// its unit/currency partition (`METRIC-UNIT-PARTITION-1`).

/**
 * Open-to-Buy (OTB) position per active envelope:
 *   `remaining_otb_cost = approved_budget_cost - recognized_receipts_cost - unreceived_authorized_po_cost`
 * Aggregates `OtbPoBinding` to `(envelope_id, currency)` first (`LineFill`
 * pattern), then joins once onto `OtbEnvelope` at its primary key.
 */
export const OtbPosition = Norn.transform({
  id: "plan.otb_position", title: "Open-to-Buy remaining envelope position", version: "0.1.0",
  inputs: { binding: OtbPoBinding, envelope: OtbEnvelope },
  nodes: {
    binding: { op: "scan", input: "binding" },
    envelope: { op: "scan", input: "envelope" },
    per_envelope: {
      op: "aggregate", input: "binding",
      groupBy: [{ column: "envelope_id" }, { column: "currency" }],
      measures: [
        { name: "recognized_receipts_cost", fn: "sum", column: "recognized_receipt_cost", label: "Recognized receipts at cost" },
        { name: "unreceived_authorized_po_cost", fn: "sum", column: "unreceived_authorized_cost", label: "Unreceived authorized PO value" },
        { name: "cancelled_released_cost", fn: "sum", column: "cancelled_released_cost", label: "Cancelled released balance" },
        { name: "otb_consumed_cost", fn: "sum", column: "otb_consumed_cost", label: "Total OTB consumed" },
        { name: "po_line_count", fn: "count" },
      ],
    },
    joined: {
      op: "join", left: "per_envelope", right: "envelope",
      on: { key: { left: ["envelope_id"], right: "primary" } }, kind: "inner", prefix: "env_",
    },
    position: {
      op: "project", input: "joined", columns: [
        { name: "envelope_id", expression: "input.envelope_id" },
        { name: "plan_version_id", expression: "input.env_plan_version_id" },
        { name: "entity_id", expression: "input.env_entity_id" },
        { name: "store_id", expression: "input.env_store_id" },
        { name: "category", expression: "input.env_category" },
        { name: "brand", expression: "input.env_brand" },
        { name: "plan_month", expression: "input.env_plan_month" },
        { name: "envelope_kind", expression: "input.env_envelope_kind" },
        { name: "currency", expression: "input.currency" },
        { name: "approved_budget_cost", expression: "input.env_approved_budget_cost" },
        { name: "recognized_receipts_cost", expression: "input.recognized_receipts_cost" },
        { name: "unreceived_authorized_po_cost", expression: "input.unreceived_authorized_po_cost" },
        { name: "cancelled_released_cost", expression: "input.cancelled_released_cost" },
        { name: "remaining_otb_cost", expression: "input.env_approved_budget_cost - input.recognized_receipts_cost - input.unreceived_authorized_po_cost" },
        { name: "po_line_count", expression: "input.po_line_count" },
      ],
    },
  },
  output: "position",
})

/**
 * Expected cash payment timing tracked separately from OTB consumption month:
 * buckets active/fulfilled/partially-received OTB PO obligations by
 * `expected_payment_date` ISO month per `(entity_id, store_id, currency)`.
 */
export const ExpectedCashOutflowMonthly = Norn.transform({
  id: "plan.expected_cash_outflow_monthly", title: "Expected PO cash payment schedule", version: "0.1.0",
  inputs: { binding: OtbPoBinding },
  nodes: {
    binding: { op: "scan", input: "binding" },
    monthly: {
      op: "aggregate", input: "binding",
      groupBy: [
        { column: "entity_id" }, { column: "store_id" }, { column: "currency" },
        { column: "expected_payment_date", grain: "month", label: "Payment month" },
      ],
      measures: [
        { name: "expected_cash_outflow_cost", fn: "sum", column: "otb_consumed_cost", label: "Expected cash outflow" },
        { name: "recognized_payable_cost", fn: "sum", column: "recognized_receipt_cost", label: "Recognized payable" },
        { name: "open_po_commitment_cost", fn: "sum", column: "unreceived_authorized_cost", label: "Unreceived PO commitment" },
        { name: "binding_count", fn: "count" },
      ],
    },
  },
  output: "monthly",
})

/**
 * Actual retail POS sales aggregated to merchandise plan grain
 * `(entity_id, store_id, category, unit, currency, month)` BEFORE joining to
 * any `PlanLine` (`LineFill` chasm-trap-safe pattern), with explicit missing-cost
 * withholding (`cost_complete`).
 */
export const ActualSalesAtPlanGrain = Norn.transform({
  id: "plan.actual_sales_at_plan_grain", title: "Actual POS sales aggregated to plan grain", version: "0.1.0",
  inputs: { sale: PosSale, store: Store, sku: Sku },
  nodes: {
    sale: { op: "scan", input: "sale" },
    store: { op: "scan", input: "store" },
    sku: { op: "scan", input: "sku" },
    sale_with_store: {
      op: "join", left: "sale", right: "store",
      on: { key: { left: ["store_id"], right: "primary" } }, kind: "inner", prefix: "st_",
    },
    sale_with_dims: {
      op: "join", left: "sale_with_store", right: "sku",
      on: { key: { left: ["sku_id"], right: "primary" } }, kind: "inner", prefix: "sk_",
    },
    at_plan_grain: {
      op: "aggregate", input: "sale_with_dims",
      groupBy: [
        { column: "st_entity_id" }, { column: "store_id" }, { column: "sk_category" },
        { column: "unit" }, { column: "currency" },
        { column: "sold_at", grain: "month", label: "Actual month" },
      ],
      measures: [
        { name: "actual_sales_qty", fn: "sum", column: "sold_qty", label: "Actual units sold" },
        { name: "actual_sales_retail", fn: "sum", column: "net_revenue", label: "Actual net revenue" },
        { name: "actual_sales_cost", fn: "sum", column: "cogs_amount", label: "Actual COGS" },
        { name: "sale_lines", fn: "count" },
        { name: "costed_lines", fn: "count", column: "costed_ref" },
      ],
    },
    summary: {
      op: "project", input: "at_plan_grain", columns: [
        { name: "entity_id", expression: "input.st_entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "category", expression: "input.sk_category" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "sold_at", expression: "input.sold_at" },
        { name: "actual_sales_qty", expression: "input.actual_sales_qty" },
        { name: "actual_sales_retail", expression: "input.actual_sales_retail" },
        { name: "cost_complete", expression: "input.costed_lines == input.sale_lines" },
        { name: "effective_actual_sales_cost", expression: "input.costed_lines == input.sale_lines ? input.actual_sales_cost : null" },
      ],
    },
  },
  output: "summary",
})

/**
 * `Forecast - Plan` variance pinning `PlanVersion`, effective `plan_month`,
 * `model_version`, and `forecast_revision`. Aggregates published forecast rows
 * to `plan_line_id` grain FIRST (`LineFill` pattern), then joins once onto
 * `PlanLine` at its primary key.
 */
export const ForecastVsPlanVariance = Norn.transform({
  id: "plan.forecast_vs_plan_variance", title: "Forecast minus Plan variance", version: "0.1.0",
  inputs: { forecast: PublishedForecast, plan_line: PlanLine },
  nodes: {
    forecast: { op: "scan", input: "forecast" },
    plan_line: { op: "scan", input: "plan_line" },
    published_only: { op: "filter", input: "forecast", predicate: "input.status == 'published'" },
    per_plan_line: {
      op: "aggregate", input: "published_only",
      groupBy: [
        { column: "plan_line_id" }, { column: "model_version" }, { column: "forecast_revision" },
        { column: "currency" }, { column: "unit" },
      ],
      measures: [
        { name: "forecast_sales_retail", fn: "sum", column: "forecast_sales_retail", label: "Forecast sales retail" },
        { name: "forecast_sales_cost", fn: "sum", column: "forecast_sales_cost", label: "Forecast sales cost" },
        { name: "forecast_sales_qty", fn: "sum", column: "forecast_sales_qty", label: "Forecast sales qty" },
        { name: "forecast_fine_metal_g", fn: "sum", column: "forecast_fine_metal_g", label: "Forecast fine metal (g)" },
        { name: "published_at", fn: "max", column: "published_at" },
      ],
    },
    joined: {
      op: "join", left: "per_plan_line", right: "plan_line",
      on: { key: { left: ["plan_line_id"], right: "primary" } }, kind: "inner", prefix: "pl_",
    },
    variance: {
      op: "project", input: "joined", columns: [
        { name: "plan_line_id", expression: "input.plan_line_id" },
        { name: "plan_version_id", expression: "input.pl_plan_version_id" },
        { name: "model_version", expression: "input.model_version" },
        { name: "forecast_revision", expression: "input.forecast_revision" },
        { name: "published_at", expression: "input.published_at" },
        { name: "entity_id", expression: "input.pl_entity_id" },
        { name: "store_id", expression: "input.pl_store_id" },
        { name: "category", expression: "input.pl_category" },
        { name: "brand", expression: "input.pl_brand" },
        { name: "plan_month", expression: "input.pl_plan_month" },
        { name: "currency", expression: "input.currency" },
        { name: "unit", expression: "input.unit" },
        { name: "planned_sales_retail", expression: "input.pl_planned_sales_retail" },
        { name: "forecast_sales_retail", expression: "input.forecast_sales_retail" },
        { name: "sales_retail_variance", expression: "input.forecast_sales_retail - input.pl_planned_sales_retail" },
        { name: "planned_sales_cost", expression: "input.pl_planned_sales_cost" },
        { name: "forecast_sales_cost", expression: "input.forecast_sales_cost" },
        { name: "sales_cost_variance", expression: "input.forecast_sales_cost - input.pl_planned_sales_cost" },
        { name: "planned_sales_qty", expression: "input.pl_planned_sales_qty" },
        { name: "forecast_sales_qty", expression: "input.forecast_sales_qty" },
        { name: "sales_qty_variance", expression: "input.forecast_sales_qty - input.pl_planned_sales_qty" },
        { name: "planned_fine_metal_g", expression: "input.pl_planned_fine_metal_g" },
        { name: "forecast_fine_metal_g", expression: "input.forecast_fine_metal_g" },
        { name: "fine_metal_g_variance", expression: "input.forecast_fine_metal_g - input.pl_planned_fine_metal_g" },
      ],
    },
  },
  output: "variance",
})
