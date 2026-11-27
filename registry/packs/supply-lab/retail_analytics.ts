import { Norn } from "@norn/authoring"
import { PosSale, StockLot, Store, StoreCommercialRollup, StoreCount } from "./retail.ts"

// Chasm-trap-safe retail commercial KPI transforms: aggregate each fact stream
// to `(store_id, sku_id, unit, currency)` (or receipt week) first, then join
// once onto `Store` at its primary key so store attributes never multiply rows.
// Missing cost (`costed_lines < sale_lines` or `costed_lots < lot_count`)
// surfaces explicitly (`cost_complete: false`) and withholds currency-bound
// margin/COGS fields without breaking `VAL-EXACT-1` currency typing.

/**
 * Store sales, gross margin, and cost-coverage summary per `(store, sku, unit, currency)`.
 * Withholds `effective_cogs` and `effective_gross_margin` whenever any sale
 * line in the bucket lacks a verified cost reference (`costed_lines < sale_lines`).
 */
export const StoreSalesPerformance = Norn.transform({
  id: "supply.retail.store_sales_performance", title: "Store sales and gross margin", version: "0.1.0",
  inputs: { sale: PosSale, store: Store },
  nodes: {
    sale: { op: "scan", input: "sale" },
    store: { op: "scan", input: "store" },
    per_store_sku: {
      op: "aggregate", input: "sale",
      groupBy: [{ column: "store_id" }, { column: "sku_id" }, { column: "unit" }, { column: "currency" }],
      measures: [
        { name: "sold_qty", fn: "sum", column: "sold_qty", label: "Units sold" },
        { name: "net_revenue", fn: "sum", column: "net_revenue", label: "Net revenue" },
        { name: "cogs_amount", fn: "sum", column: "cogs_amount", label: "COGS" },
        { name: "gross_margin", fn: "sum", column: "gross_margin", label: "Gross margin" },
        { name: "sale_lines", fn: "count" },
        { name: "costed_lines", fn: "count", column: "costed_ref" },
      ],
    },
    joined: {
      op: "join", left: "per_store_sku", right: "store",
      on: { key: { left: ["store_id"], right: "primary" } }, kind: "inner", prefix: "st_",
    },
    summary: {
      op: "project", input: "joined", columns: [
        { name: "store_id", expression: "input.store_id" },
        { name: "entity_id", expression: "input.st_entity_id" },
        { name: "custodian_id", expression: "input.st_custodian_id" },
        { name: "mall_code", expression: "input.st_mall_code" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "sold_qty", expression: "input.sold_qty" },
        { name: "net_revenue", expression: "input.net_revenue" },
        { name: "sale_lines", expression: "input.sale_lines" },
        { name: "costed_lines", expression: "input.costed_lines" },
        { name: "cost_complete", expression: "input.costed_lines == input.sale_lines" },
        { name: "effective_cogs", expression: "input.costed_lines == input.sale_lines ? input.cogs_amount : null" },
        { name: "effective_gross_margin", expression: "input.costed_lines == input.sale_lines ? input.gross_margin : null" },
      ],
    },
  },
  output: "summary",
})

/**
 * Active holding lot receipt cohorts per `(store, sku, unit, currency, received_week)`:
 * filters strictly to live lots (`remaining_qty > 0`) so fully depleted lots do
 * not pollute inventory aging, preserves original lot receipt dates, and flags
 * missing unit cost (`costed_lots < lot_count`) so valuation/GMROI fail closed.
 */
export const StoreLotAging = Norn.transform({
  id: "supply.retail.store_lot_aging", title: "Store holding lot aging cohorts", version: "0.1.0",
  inputs: { lot: StockLot, store: Store },
  nodes: {
    lot: { op: "scan", input: "lot" },
    active_lots: { op: "filter", input: "lot", predicate: "input.remaining_qty > 0" },
    store: { op: "scan", input: "store" },
    cohorts: {
      op: "aggregate", input: "active_lots",
      groupBy: [
        { column: "store_id" }, { column: "sku_id" }, { column: "unit" }, { column: "currency" },
        { column: "received_at", grain: "week", label: "Receipt week" },
      ],
      measures: [
        { name: "received_qty", fn: "sum", column: "received_qty", label: "Received" },
        { name: "lot_count", fn: "count" },
        { name: "costed_lots", fn: "count", column: "unit_cost" },
        { name: "oldest_received_at", fn: "min", column: "received_at" },
        { name: "latest_as_of", fn: "max", column: "as_of" },
      ],
    },
    joined: {
      op: "join", left: "cohorts", right: "store",
      on: { key: { left: ["store_id"], right: "primary" } }, kind: "inner", prefix: "st_",
    },
    aging: {
      op: "project", input: "joined", columns: [
        { name: "store_id", expression: "input.store_id" },
        { name: "entity_id", expression: "input.st_entity_id" },
        { name: "custodian_id", expression: "input.st_custodian_id" },
        { name: "mall_code", expression: "input.st_mall_code" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "received_at", expression: "input.received_at" },
        { name: "received_qty", expression: "input.received_qty" },
        { name: "lot_count", expression: "input.lot_count" },
        { name: "costed_lots", expression: "input.costed_lots" },
        { name: "cost_complete", expression: "input.costed_lots == input.lot_count" },
        { name: "oldest_received_at", expression: "input.oldest_received_at" },
        { name: "latest_as_of", expression: "input.latest_as_of" },
      ],
    },
  },
  output: "aging",
})

/**
 * Read-only per-legal-entity management summary transform (feeds the multi-cell
 * combined management view, never a write path or statutory consolidation):
 * joins `PosSale` onto `Store` via its required many-to-one `store` link
 * (fan-out 1), aggregates per `(entity_id, currency)`, and withholds
 * `effective_gross_margin` whenever any sale line in the entity lacks cost.
 */
export const EntityManagementSummary = Norn.transform({
  id: "supply.retail.entity_management_summary", title: "Per-entity retail management summary", version: "0.1.0",
  inputs: { sale: PosSale, store: Store },
  nodes: {
    sale: { op: "scan", input: "sale" },
    store: { op: "scan", input: "store" },
    sale_with_store: {
      op: "join", left: "sale", right: "store",
      on: { key: { left: ["store_id"], right: "primary" } }, kind: "inner", prefix: "st_",
    },
    per_entity: {
      op: "aggregate", input: "sale_with_store",
      groupBy: [{ column: "st_entity_id" }, { column: "currency" }],
      measures: [
        { name: "net_revenue", fn: "sum", column: "net_revenue", label: "Entity net revenue" },
        { name: "cogs_amount", fn: "sum", column: "cogs_amount", label: "Entity COGS" },
        { name: "gross_margin", fn: "sum", column: "gross_margin", label: "Entity gross margin" },
        { name: "sale_lines", fn: "count" },
        { name: "costed_lines", fn: "count", column: "costed_ref" },
      ],
    },
    summary: {
      op: "project", input: "per_entity", columns: [
        { name: "st_entity_id", expression: "input.st_entity_id" },
        { name: "currency", expression: "input.currency" },
        { name: "net_revenue", expression: "input.net_revenue" },
        { name: "sale_lines", expression: "input.sale_lines" },
        { name: "costed_lines", expression: "input.costed_lines" },
        { name: "cost_complete", expression: "input.costed_lines == input.sale_lines" },
        { name: "effective_cogs", expression: "input.costed_lines == input.sale_lines ? input.cogs_amount : null" },
        { name: "effective_gross_margin", expression: "input.costed_lines == input.sale_lines ? input.gross_margin : null" },
      ],
    },
  },
  output: "summary",
})

/**
 * Active store-SKU stock positions derived from additive lot receipts minus
 * additive depletions (`remaining_qty == received_qty - depleted_qty > 0`,
 * `stock_value == opening_stock_value - depleted_stock_value`),
 * providing the canonical expected-grain population for physical count
 * reconciliation and commercial KPI rollup completeness gates.
 */
export const StoreActivePositions = Norn.transform({
  id: "supply.retail.store_active_positions", title: "Active store-SKU stock positions", version: "0.1.0",
  inputs: { lot: StockLot },
  nodes: {
    lot: { op: "scan", input: "lot" },
    per_position: {
      op: "aggregate", input: "lot",
      groupBy: [{ column: "store_id" }, { column: "sku_id" }, { column: "unit" }, { column: "currency" }],
      measures: [
        { name: "sum_received_qty", fn: "sum", column: "received_qty", label: "Total received" },
        { name: "sum_depleted_qty", fn: "sum", column: "depleted_qty", label: "Total depleted" },
        { name: "sum_opening_stock_value", fn: "sum", column: "opening_stock_value", label: "Total opening value" },
        { name: "sum_depleted_stock_value", fn: "sum", column: "depleted_stock_value", label: "Total depleted value" },
        { name: "lot_count", fn: "count" },
        { name: "costed_lots", fn: "count", column: "unit_cost" },
      ],
    },
    derived: {
      op: "project", input: "per_position", columns: [
        { name: "store_id", expression: "input.store_id" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "derived_on_hand_qty", expression: "input.sum_received_qty - input.sum_depleted_qty" },
        { name: "derived_stock_value", expression: "input.sum_opening_stock_value - input.sum_depleted_stock_value" },
        { name: "lot_count", expression: "input.lot_count" },
        { name: "costed_lots", expression: "input.costed_lots" },
        { name: "cost_complete", expression: "input.costed_lots == input.lot_count" },
      ],
    },
    active: { op: "filter", input: "derived", predicate: "input.derived_on_hand_qty > 0" },
  },
  output: "active",
})

/**
 * Week-bucketed POS sales and gross-margin operands per
 * `(store_id, sku_id, unit, currency, period_week)` so weekly commercial KPI
 * rollups reconcile strictly against sales inside their own effective ISO week
 * rather than all-time POS totals.
 */
export const StoreSalesWeeklyOperands = Norn.transform({
  id: "supply.retail.store_sales_weekly_operands", title: "Weekly store sales operands", version: "0.1.0",
  inputs: { sale: PosSale },
  nodes: {
    sale: { op: "scan", input: "sale" },
    per_week: {
      op: "aggregate", input: "sale",
      groupBy: [
        { column: "store_id" }, { column: "sku_id" }, { column: "unit" }, { column: "currency" },
        { column: "sold_at", grain: "week", label: "Sale week" },
      ],
      measures: [
        { name: "sold_qty", fn: "sum", column: "sold_qty", label: "Weekly units sold" },
        { name: "gross_margin", fn: "sum", column: "gross_margin", label: "Weekly gross margin" },
        { name: "sale_lines", fn: "count" },
        { name: "costed_lines", fn: "count", column: "costed_ref" },
      ],
    },
    operands: {
      op: "project", input: "per_week", columns: [
        { name: "store_id", expression: "input.store_id" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "period_week", expression: "input.sold_at" },
        { name: "sold_qty", expression: "input.sold_qty" },
        { name: "cost_complete", expression: "input.costed_lines == input.sale_lines" },
        { name: "effective_gross_margin", expression: "input.costed_lines == input.sale_lines ? input.gross_margin : null" },
      ],
    },
  },
  output: "operands",
})

/** Latest governed `on_hand` physical counts per `(store_id, sku_id, unit)`. */
export const StoreOnHandCountPositions = Norn.transform({
  id: "supply.retail.store_on_hand_count_positions", title: "Store on-hand physical counts", version: "0.1.0",
  inputs: { count: StoreCount },
  nodes: {
    count: { op: "scan", input: "count" },
    on_hand_only: { op: "filter", input: "count", predicate: "input.quantity_basis == \"on_hand\"" },
    projected: {
      op: "project", input: "on_hand_only", columns: [
        { name: "id", expression: "input.id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "on_hand_qty", expression: "input.on_hand_qty" },
        { name: "as_of", expression: "input.as_of" },
      ],
    },
  },
  output: "projected",
})

/** Latest governed `available` physical counts per `(store_id, sku_id, unit)`. */
export const StoreAvailableCountPositions = Norn.transform({
  id: "supply.retail.store_available_count_positions", title: "Store available physical counts", version: "0.1.0",
  inputs: { count: StoreCount },
  nodes: {
    count: { op: "scan", input: "count" },
    available_only: { op: "filter", input: "count", predicate: "input.quantity_basis == \"available\"" },
    projected: {
      op: "project", input: "available_only", columns: [
        { name: "id", expression: "input.id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "on_hand_qty", expression: "input.on_hand_qty" },
        { name: "as_of", expression: "input.as_of" },
      ],
    },
  },
  output: "projected",
})

/**
 * Verified and costed 1-week commercial KPI rollup operands (sales, margin,
 * closing quantity, and closing valuation) per `(store_id, sku_id, unit,
 * currency, period_week)` so the shipped combined view fails closed whenever
 * late POS facts, lot corrections, or recounts make a published rollup stale.
 */
export const StoreRollupSalesOperands = Norn.transform({
  id: "supply.retail.store_rollup_sales_operands", title: "Verified store rollup sales operands", version: "0.1.0",
  inputs: { rollup: StoreCommercialRollup },
  nodes: {
    rollup: { op: "scan", input: "rollup" },
    verified_only: {
      op: "filter", input: "rollup",
      predicate: "input.count_status == \"verified\" && input.cost_status == \"costed\" && input.sales_weeks == 1",
    },
    operands: {
      op: "project", input: "verified_only", columns: [
        { name: "id", expression: "input.id" },
        { name: "store_id", expression: "input.store_id" },
        { name: "sku_id", expression: "input.sku_id" },
        { name: "unit", expression: "input.unit" },
        { name: "currency", expression: "input.currency" },
        { name: "period_week", expression: "input.period_week" },
        { name: "implied_sold_qty", expression: "input.avg_weekly_sales_qty * input.sales_weeks" },
        { name: "period_gross_margin", expression: "input.period_gross_margin" },
        { name: "on_hand_qty", expression: "input.on_hand_qty" },
        { name: "closing_stock_value", expression: "input.closing_stock_value" },
      ],
    },
  },
  output: "operands",
})
