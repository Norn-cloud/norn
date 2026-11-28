import { Norn } from "@norn/authoring"
import { Sku } from "../supply-lab/supply.ts"
import { PosSale, Store } from "../supply-lab/retail.ts"
import {
  ActivePlanLine, OtbEnvelope, OtbPoBinding, PlanLine, PublishedForecast, SkuProfile,
} from "./plan.ts"

// Chasm-trap-safe planning, OTB, and variance transforms (`LineFill` pattern):
// every transform aggregates its fact stream to the target grain FIRST, then
// joins once onto the right-hand entity's unique key so multi-row facts never
// multiply budgets or plan lines. Every monetary or physical measure preserves
// its unit/currency partition (`METRIC-UNIT-PARTITION-1`).

/**
 * Open-to-Buy (OTB) position over the active/approved `plan.OtbEnvelope`
 * population (excluding draft, pending_override, and closed envelopes):
 *   `remaining_otb_cost = approved_budget_cost - recognized_receipts_cost - unreceived_authorized_po_cost`
 * - Aggregates `OtbPoBinding` across the entire merchandise grain
 *   `(entity_id, store_id, category, brand, plan_month, currency)` so
 *   outstanding commitments from superseded revisions ALWAYS count against the
 *   governing active envelope's cap.
 * - Enforces complete binding coverage: Branch 2 (`zero_po`) ONLY admits active
 *   envelopes with `binding_count == 0`, and Branch 1 (`with_bindings`)
 *   withholds `remaining_otb_cost` as `null` (`bindings_complete = false`) if
 *   fewer bindings are visible than `env_binding_count`, so partial visibility
 *   can never fabricate full remaining budget.
 */
export const OtbPosition = Norn.transform({
  id: "plan.otb_position", title: "Open-to-Buy remaining envelope position", version: "0.1.0",
  inputs: { envelope: OtbEnvelope, binding: OtbPoBinding },
  nodes: {
    envelope: { op: "scan", input: "envelope" },
    active_envelope: { op: "filter", input: "envelope", predicate: "input.status == 'active'" },
    binding: { op: "scan", input: "binding" },
    // Branch 1: active envelopes joined to grain-wide PO commitments across all revisions.
    per_grain: {
      op: "aggregate", input: "binding",
      groupBy: [
        { column: "entity_id" }, { column: "store_id" }, { column: "category" },
        { column: "brand" }, { column: "plan_month" }, { column: "currency" },
      ],
      measures: [
        { name: "recognized_receipts_cost", fn: "sum", column: "recognized_receipt_cost", label: "Recognized receipts at cost" },
        { name: "unreceived_authorized_po_cost", fn: "sum", column: "unreceived_authorized_cost", label: "Unreceived authorized PO value" },
        { name: "cancelled_released_cost", fn: "sum", column: "cancelled_released_cost", label: "Cancelled released balance" },
        { name: "po_line_count", fn: "count" },
        { name: "max_binding_seq", fn: "max", column: "authorized_binding_seq" },
      ],
    },
    per_grain_active_key: {
      op: "project", input: "per_grain", columns: [
        { name: "entity_id", expression: "input.entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "category", expression: "input.category" },
        { name: "brand", expression: "input.brand" },
        { name: "plan_month", expression: "input.plan_month" },
        { name: "currency", expression: "input.currency" },
        { name: "active_grain_slot", expression: "'ACTIVE'" },
        { name: "recognized_receipts_cost", expression: "input.recognized_receipts_cost" },
        { name: "unreceived_authorized_po_cost", expression: "input.unreceived_authorized_po_cost" },
        { name: "cancelled_released_cost", expression: "input.cancelled_released_cost" },
        { name: "po_line_count", expression: "input.po_line_count" },
        { name: "max_binding_seq", expression: "input.max_binding_seq" },
      ],
    },
    joined_active: {
      op: "join", left: "per_grain_active_key", right: "active_envelope",
      on: {
        key: {
          left: ["entity_id", "store_id", "category", "brand", "plan_month", "currency", "active_grain_slot"],
          right: "active_grain",
        },
      },
      kind: "inner", prefix: "env_",
    },
    with_bindings: {
      op: "project", input: "joined_active", columns: [
        { name: "entity_id", expression: "input.entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "category", expression: "input.category" },
        { name: "brand", expression: "input.brand" },
        { name: "plan_month", expression: "input.plan_month" },
        { name: "currency", expression: "input.currency" },
        { name: "envelope_id", expression: "input.env_envelope_id" },
        { name: "plan_version_id", expression: "input.env_plan_version_id" },
        { name: "envelope_kind", expression: "input.env_envelope_kind" },
        { name: "approved_budget_cost", expression: "input.env_approved_budget_cost" },
        { name: "recognized_receipts_cost", expression: "input.recognized_receipts_cost + 0" },
        { name: "unreceived_authorized_po_cost", expression: "input.unreceived_authorized_po_cost + 0" },
        { name: "cancelled_released_cost", expression: "input.cancelled_released_cost + 0" },
        { name: "bindings_complete", expression: "input.po_line_count == input.max_binding_seq" },
        { name: "remaining_otb_cost", expression: "input.po_line_count == input.max_binding_seq ? (input.env_approved_budget_cost - input.recognized_receipts_cost - input.unreceived_authorized_po_cost) : null" },
        { name: "po_line_count", expression: "input.po_line_count + 0" },
        { name: "has_bindings", expression: "1" },
      ],
    },
    // Branch 2: active envelopes with zero visible PO bindings at the exact same
    // 6-column merchandise grain (deduped below in favour of `with_bindings`;
    // `VIEW-READ-1` + `HOST-READ-2` coverage gates fail closed whenever any
    // `binding` row is withheld).
    active_keys: {
      op: "aggregate", input: "active_envelope",
      groupBy: [
        { column: "entity_id" }, { column: "store_id" }, { column: "category" },
        { column: "brand" }, { column: "plan_month" }, { column: "currency" },
      ],
      measures: [{ name: "env_rows", fn: "count" }],
    },
    active_keys_slot: {
      op: "project", input: "active_keys", columns: [
        { name: "entity_id", expression: "input.entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "category", expression: "input.category" },
        { name: "brand", expression: "input.brand" },
        { name: "plan_month", expression: "input.plan_month" },
        { name: "currency", expression: "input.currency" },
        { name: "active_grain_slot", expression: "'ACTIVE'" },
      ],
    },
    active_full: {
      op: "join", left: "active_keys_slot", right: "active_envelope",
      on: {
        key: {
          left: ["entity_id", "store_id", "category", "brand", "plan_month", "currency", "active_grain_slot"],
          right: "active_grain",
        },
      },
      kind: "inner", prefix: "env_",
    },
    zero_po: {
      op: "project", input: "active_full", columns: [
        { name: "entity_id", expression: "input.entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "category", expression: "input.category" },
        { name: "brand", expression: "input.brand" },
        { name: "plan_month", expression: "input.plan_month" },
        { name: "currency", expression: "input.currency" },
        { name: "envelope_id", expression: "input.env_envelope_id" },
        { name: "plan_version_id", expression: "input.env_plan_version_id" },
        { name: "envelope_kind", expression: "input.env_envelope_kind" },
        { name: "approved_budget_cost", expression: "input.env_approved_budget_cost" },
        { name: "recognized_receipts_cost", expression: "input.env_approved_budget_cost - input.env_approved_budget_cost" },
        { name: "unreceived_authorized_po_cost", expression: "input.env_approved_budget_cost - input.env_approved_budget_cost" },
        { name: "cancelled_released_cost", expression: "input.env_approved_budget_cost - input.env_approved_budget_cost" },
        { name: "bindings_complete", expression: "true" },
        { name: "remaining_otb_cost", expression: "true ? (input.env_approved_budget_cost + 0) : null" },
        { name: "po_line_count", expression: "0" },
        { name: "has_bindings", expression: "0" },
      ],
    },
    combined: { op: "union", inputs: ["with_bindings", "zero_po"], branch: "source_branch" },
    deduped: {
      op: "dedupe", input: "combined",
      key: ["entity_id", "store_id", "category", "brand", "plan_month", "currency"],
      orderBy: [{ column: "has_bindings", direction: "desc" }, { column: "source_branch", direction: "asc" }],
      keep: "first",
    },
    position: {
      op: "project", input: "deduped", columns: [
        { name: "entity_id", expression: "input.entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "category", expression: "input.category" },
        { name: "brand", expression: "input.brand" },
        { name: "plan_month", expression: "input.plan_month" },
        { name: "currency", expression: "input.currency" },
        { name: "envelope_id", expression: "input.envelope_id" },
        { name: "plan_version_id", expression: "input.plan_version_id" },
        { name: "envelope_kind", expression: "input.envelope_kind" },
        { name: "approved_budget_cost", expression: "input.approved_budget_cost" },
        { name: "recognized_receipts_cost", expression: "input.recognized_receipts_cost" },
        { name: "unreceived_authorized_po_cost", expression: "input.unreceived_authorized_po_cost" },
        { name: "cancelled_released_cost", expression: "input.cancelled_released_cost" },
        { name: "bindings_complete", expression: "input.bindings_complete" },
        { name: "remaining_otb_cost", expression: "input.remaining_otb_cost" },
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
 * Actual retail POS sales aggregated to FULL merchandise plan grain
 * `(entity_id, store_id, category, brand, unit, currency, month)` BEFORE joining
 * any `PlanLine` (`LineFill` chasm-trap-safe pattern), with explicit missing-cost
 * withholding (`cost_complete`).
 */
export const ActualSalesAtPlanGrain = Norn.transform({
  id: "plan.actual_sales_at_plan_grain", title: "Actual POS sales aggregated to full plan grain", version: "0.1.0",
  inputs: { sale: PosSale, store: Store, sku: Sku, sku_profile: SkuProfile },
  nodes: {
    sale: { op: "scan", input: "sale" },
    store: { op: "scan", input: "store" },
    sku: { op: "scan", input: "sku" },
    sku_profile: { op: "scan", input: "sku_profile" },
    sale_with_store: {
      op: "join", left: "sale", right: "store",
      on: { key: { left: ["store_id"], right: "primary" } }, kind: "inner", prefix: "st_",
    },
    sale_with_sku: {
      op: "join", left: "sale_with_store", right: "sku",
      on: { key: { left: ["sku_id"], right: "primary" } }, kind: "inner", prefix: "sk_",
    },
    sale_with_dims: {
      op: "join", left: "sale_with_sku", right: "sku_profile",
      on: { key: { left: ["sku_id"], right: "primary" } }, kind: "left", prefix: "sp_",
    },
    sale_resolved: {
      op: "project", input: "sale_with_dims", columns: [
        { name: "id", expression: "input.id" },
        { name: "st_entity_id", expression: "input.st_entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sk_category", expression: "input.sk_category" },
        { name: "sp_brand", expression: "input.sp_brand ?? '__unprofiled__'" },
        { name: "profile_ref", expression: "input.sp_brand" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "sold_at", expression: "input.sold_at" },
        { name: "sold_qty", expression: "input.sold_qty" },
        { name: "net_revenue", expression: "input.net_revenue" },
        { name: "cogs_amount", expression: "input.cogs_amount" },
        { name: "costed_ref", expression: "input.costed_ref" },
      ],
    },
    at_plan_grain: {
      op: "aggregate", input: "sale_resolved",
      groupBy: [
        { column: "st_entity_id" }, { column: "store_id" }, { column: "sk_category" },
        { column: "sp_brand" }, { column: "unit" }, { column: "currency" },
        { column: "sold_at", grain: "month", label: "Actual month" },
      ],
      measures: [
        { name: "actual_sales_qty", fn: "sum", column: "sold_qty", label: "Actual units sold" },
        { name: "actual_sales_retail", fn: "sum", column: "net_revenue", label: "Actual net revenue" },
        { name: "actual_sales_cost", fn: "sum", column: "cogs_amount", label: "Actual COGS" },
        { name: "sale_lines", fn: "count" },
        { name: "costed_lines", fn: "count", column: "costed_ref" },
        { name: "profiled_lines", fn: "count", column: "profile_ref" },
      ],
    },
    summary: {
      op: "project", input: "at_plan_grain", columns: [
        { name: "st_entity_id", expression: "input.st_entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sk_category", expression: "input.sk_category" },
        { name: "sp_brand", expression: "input.sp_brand" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "sold_at", expression: "input.sold_at" },
        { name: "actual_sales_qty", expression: "input.actual_sales_qty" },
        { name: "actual_sales_retail", expression: "input.actual_sales_retail" },
        { name: "profile_complete", expression: "input.profiled_lines == input.sale_lines" },
        { name: "cost_complete", expression: "input.costed_lines == input.sale_lines" },
        { name: "effective_actual_sales_cost", expression: "input.costed_lines == input.sale_lines ? input.actual_sales_cost : null" },
      ],
    },
  },
  output: "summary",
})

/**
 * Complete `Actual - Plan` variance at FULL plan grain
 * `(entity_id x store_id x category x brand x ISO month x currency x unit)`:
 * aggregates `PosSale` (left-joined with `SkuProfile` so unprofiled sales NEVER
 * vanish silently) to plan grain FIRST, then left-joins once onto
 * `ActivePlanLine` and `PlanLine`, explicitly pinning `plan_version_id`,
 * `plan_line_id`, surfacing `profile_complete` and `plan_matched`, and
 * withholding variance whenever a SKU is unprofiled, unmatched, or lacks cost.
 */
export const ActualVsPlanVariance = Norn.transform({
  id: "plan.actual_vs_plan_variance", title: "Actual minus Plan variance at full plan grain", version: "0.1.0",
  inputs: {
    sale: PosSale, store: Store, sku: Sku, sku_profile: SkuProfile,
    baseline: ActivePlanLine, plan_line: PlanLine,
  },
  nodes: {
    sale: { op: "scan", input: "sale" },
    store: { op: "scan", input: "store" },
    sku: { op: "scan", input: "sku" },
    sku_profile: { op: "scan", input: "sku_profile" },
    baseline: { op: "scan", input: "baseline" },
    plan_line: { op: "scan", input: "plan_line" },
    sale_with_store: {
      op: "join", left: "sale", right: "store",
      on: { key: { left: ["store_id"], right: "primary" } }, kind: "inner", prefix: "st_",
    },
    sale_with_sku: {
      op: "join", left: "sale_with_store", right: "sku",
      on: { key: { left: ["sku_id"], right: "primary" } }, kind: "inner", prefix: "sk_",
    },
    sale_with_dims: {
      op: "join", left: "sale_with_sku", right: "sku_profile",
      on: { key: { left: ["sku_id"], right: "primary" } }, kind: "left", prefix: "sp_",
    },
    sale_resolved: {
      op: "project", input: "sale_with_dims", columns: [
        { name: "id", expression: "input.id" },
        { name: "st_entity_id", expression: "input.st_entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sk_category", expression: "input.sk_category" },
        { name: "sp_brand", expression: "input.sp_brand ?? '__unprofiled__'" },
        { name: "profile_ref", expression: "input.sp_brand" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "sold_at", expression: "input.sold_at" },
        { name: "sold_qty", expression: "input.sold_qty" },
        { name: "net_revenue", expression: "input.net_revenue" },
        { name: "cogs_amount", expression: "input.cogs_amount" },
        { name: "costed_ref", expression: "input.costed_ref" },
      ],
    },
    at_plan_grain: {
      op: "aggregate", input: "sale_resolved",
      groupBy: [
        { column: "st_entity_id" }, { column: "store_id" }, { column: "sk_category" },
        { column: "sp_brand" }, { column: "sold_at", grain: "month", label: "Actual month" },
        { column: "currency" }, { column: "unit" },
      ],
      measures: [
        { name: "actual_sales_qty", fn: "sum", column: "sold_qty", label: "Actual units sold" },
        { name: "actual_sales_retail", fn: "sum", column: "net_revenue", label: "Actual net revenue" },
        { name: "actual_sales_cost", fn: "sum", column: "cogs_amount", label: "Actual COGS" },
        { name: "sale_lines", fn: "count" },
        { name: "costed_lines", fn: "count", column: "costed_ref" },
        { name: "profiled_lines", fn: "count", column: "profile_ref" },
      ],
    },
    with_baseline: {
      op: "join", left: "at_plan_grain", right: "baseline",
      on: {
        key: {
          left: ["st_entity_id", "store_id", "sk_category", "sp_brand", "sold_at", "currency", "unit"],
          right: "plan_grain_slot",
        },
      },
      kind: "left", prefix: "bl_",
    },
    with_plan: {
      op: "join", left: "with_baseline", right: "plan_line",
      on: { key: { left: ["bl_plan_line_id"], right: "primary" } },
      kind: "left", prefix: "pl_",
    },
    variance: {
      op: "project", input: "with_plan", columns: [
        { name: "st_entity_id", expression: "input.st_entity_id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sk_category", expression: "input.sk_category" },
        { name: "sp_brand", expression: "input.sp_brand" },
        { name: "sold_at", expression: "input.sold_at" },
        { name: "currency", expression: "input.currency" },
        { name: "unit", expression: "input.unit" },
        { name: "plan_version_id", expression: "input.pl_plan_version_id" },
        { name: "plan_line_id", expression: "input.pl_plan_line_id" },
        { name: "plan_month", expression: "input.pl_plan_month" },
        { name: "profile_complete", expression: "input.profiled_lines == input.sale_lines" },
        { name: "plan_matched", expression: "input.pl_plan_line_id != null" },
        { name: "planned_sales_retail", expression: "input.pl_planned_sales_retail" },
        { name: "actual_sales_retail", expression: "input.actual_sales_retail" },
        { name: "sales_retail_variance", expression: "input.pl_planned_sales_retail != null ? (input.actual_sales_retail - (input.pl_planned_sales_retail ?? input.actual_sales_retail)) : null" },
        { name: "planned_sales_cost", expression: "input.pl_planned_sales_cost" },
        { name: "cost_complete", expression: "input.costed_lines == input.sale_lines" },
        { name: "effective_actual_sales_cost", expression: "input.costed_lines == input.sale_lines ? input.actual_sales_cost : null" },
        { name: "sales_cost_variance", expression: "(input.pl_planned_sales_cost != null and input.costed_lines == input.sale_lines) ? (input.actual_sales_cost - (input.pl_planned_sales_cost ?? input.actual_sales_cost)) : null" },
        { name: "planned_sales_qty", expression: "input.pl_planned_sales_qty" },
        { name: "actual_sales_qty", expression: "input.actual_sales_qty" },
        { name: "sales_qty_variance", expression: "input.pl_planned_sales_qty != null ? (input.actual_sales_qty - (input.pl_planned_sales_qty ?? input.actual_sales_qty)) : null" },
      ],
    },
  },
  output: "variance",
})

/**
 * `Forecast - Plan` variance pinning `PlanVersion`, effective `plan_month`,
 * `model_version`, `forecast_revision`, and publication evidence (`status != 'draft'`).
 * Retains superseded forecast revisions so pinned historical queries remain
 * reproducible after a newer revision supersedes an earlier one.
 */
export const ForecastVsPlanVariance = Norn.transform({
  id: "plan.forecast_vs_plan_variance", title: "Forecast minus Plan variance", version: "0.1.0",
  inputs: { forecast: PublishedForecast, plan_line: PlanLine },
  nodes: {
    forecast: { op: "scan", input: "forecast" },
    plan_line: { op: "scan", input: "plan_line" },
    published_history: { op: "filter", input: "forecast", predicate: "input.status != 'draft'" },
    per_plan_line: {
      op: "aggregate", input: "published_history",
      groupBy: [
        { column: "plan_line_id" }, { column: "model_version" }, { column: "forecast_revision" },
        { column: "status" }, { column: "currency" }, { column: "unit" },
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
        { name: "model_version", expression: "input.model_version" },
        { name: "forecast_revision", expression: "input.forecast_revision" },
        { name: "status", expression: "input.status" },
        { name: "currency", expression: "input.currency" },
        { name: "unit", expression: "input.unit" },
        { name: "plan_version_id", expression: "input.pl_plan_version_id" },
        { name: "published_at", expression: "input.published_at" },
        { name: "entity_id", expression: "input.pl_entity_id" },
        { name: "store_id", expression: "input.pl_store_id" },
        { name: "category", expression: "input.pl_category" },
        { name: "brand", expression: "input.pl_brand" },
        { name: "plan_month", expression: "input.pl_plan_month" },
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
