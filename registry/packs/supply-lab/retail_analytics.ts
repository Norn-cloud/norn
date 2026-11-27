import { Norn } from "@norn/authoring"
import { PosSale, StockLot, Store } from "./retail.ts"

// Chasm-trap-safe retail commercial KPI transforms: aggregate each fact stream
// to `(store_id, sku_id, unit, currency)` (or receipt week) first, then join
// once onto `Store` at its primary key so store attributes never multiply rows.
// Missing cost (`costed_lines < sale_lines` or `costed_lots < lot_count`)
// surfaces explicitly (`cost_complete: false`) and withholds margin/ratio fields.

/**
 * Store sales, gross margin, and cost-coverage summary per `(store, sku, unit, currency)`.
 * Withholds `effective_gross_margin` and `gross_margin_rate` whenever any sale
 * line in the bucket lacks a cost reference (`costed_lines < sale_lines`).
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
        {
          name: "gross_margin_rate",
          expression: "(input.costed_lines == input.sale_lines and input.net_revenue > 0) ? input.gross_margin / input.net_revenue : null",
        },
      ],
    },
  },
  output: "summary",
})

/**
 * Holding lot receipt cohorts per `(store, sku, unit, currency, received_week)`:
 * tracks original lot receipt dates for inventory aging and flags missing unit
 * cost (`costed_lots < lot_count`) so valuation/GMROI can fail closed.
 */
export const StoreLotAging = Norn.transform({
  id: "supply.retail.store_lot_aging", title: "Store holding lot aging cohorts", version: "0.1.0",
  inputs: { lot: StockLot, store: Store },
  nodes: {
    lot: { op: "scan", input: "lot" },
    store: { op: "scan", input: "store" },
    cohorts: {
      op: "aggregate", input: "lot",
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
