/**
 * Shipped declarative combined-view coverage manifest for the S1 Retail Truth
 * Slice, lowered into `questions/retail.coverage.json` and consumed directly
 * by `norn_cell::combined_view::CellCoverageSpec`.
 */
export interface RetailCoverageManifest {
  readonly requiredFeeds: readonly string[]
  readonly feedMaxAgeMs: number
  readonly evidenceTypes: readonly string[]
  readonly questions: readonly string[]
  readonly coverageTransforms: ReadonlyArray<{
    readonly name: string
    readonly transformId: string
    readonly requiredTrueColumn: string
  }>
  readonly reconciliationGates: ReadonlyArray<{
    readonly name: string
    readonly expectedTransformId: string
    readonly observedTransformId: string
    readonly keyColumns: readonly string[]
    readonly expectedValueColumn: string
    readonly observedValueColumn: string
    readonly comparison: "eq" | "lte"
    readonly observedTimestampColumn: string | null
    readonly maxAgeMs: number | null
    readonly expectedFilters: ReadonlyArray<readonly [string, string]>
    readonly observedFilters: ReadonlyArray<readonly [string, string]>
  }>
  readonly expectedGrainGates: ReadonlyArray<{
    readonly name: string
    readonly expectedTransformId: string
    readonly expectedColumns: readonly string[]
    readonly questionName: string
    readonly bucketIndices: readonly number[]
    readonly requireNonNullValue: boolean
    readonly requiredBucketFilters: ReadonlyArray<readonly [number, string]>
  }>
}

export const RetailCombinedViewCoverage: RetailCoverageManifest = {
  requiredFeeds: [
    "supply.pos_sales",
    "supply.store_counts",
    "supply.retail_receipts",
    "supply.unit_costs",
  ],
  feedMaxAgeMs: 60_000,
  evidenceTypes: [
    "supply.PosSaleEvidence",
    "supply.StoreCountEvidence",
    "supply.RetailReceiptEvidence",
    "supply.UnitCostEvidence",
  ],
  questions: [
    "supply.retail.store_revenue_weekly",
    "supply.retail.store_gross_margin_weekly",
    "supply.retail.store_gross_margin_rate_weekly",
    "supply.retail.store_sell_through_weekly",
    "supply.retail.store_weeks_of_supply_weekly",
    "supply.retail.store_gmroi_weekly",
    "supply.retail.pos_feed_revenue_weekly",
  ],
  coverageTransforms: [
    {
      name: "supply.retail.entity_management_summary",
      transformId: "supply.retail.entity_management_summary",
      requiredTrueColumn: "cost_complete",
    },
    {
      name: "supply.retail.store_lot_aging",
      transformId: "supply.retail.store_lot_aging",
      requiredTrueColumn: "cost_complete",
    },
    {
      name: "supply.retail.store_active_positions",
      transformId: "supply.retail.store_active_positions",
      requiredTrueColumn: "cost_complete",
    },
  ],
  reconciliationGates: [
    {
      name: "supply.retail.on_hand_count_reconciliation",
      expectedTransformId: "supply.retail.store_active_positions",
      observedTransformId: "supply.retail.store_on_hand_count_positions",
      keyColumns: ["store_id", "sku_id", "unit"],
      expectedValueColumn: "derived_on_hand_qty",
      observedValueColumn: "on_hand_qty",
      comparison: "eq",
      observedTimestampColumn: "as_of",
      maxAgeMs: 7 * 24 * 60 * 60 * 1000,
      expectedFilters: [],
      observedFilters: [],
    },
    {
      name: "supply.retail.available_count_reconciliation",
      expectedTransformId: "supply.retail.store_active_positions",
      observedTransformId: "supply.retail.store_available_count_positions",
      keyColumns: ["store_id", "sku_id", "unit"],
      expectedValueColumn: "derived_on_hand_qty",
      observedValueColumn: "on_hand_qty",
      comparison: "lte",
      observedTimestampColumn: "as_of",
      maxAgeMs: 7 * 24 * 60 * 60 * 1000,
      expectedFilters: [],
      observedFilters: [],
    },
    {
      name: "supply.retail.rollup_sales_reconciliation",
      expectedTransformId: "supply.retail.store_sales_weekly_operands",
      observedTransformId: "supply.retail.store_rollup_sales_operands",
      keyColumns: ["store_id", "sku_id", "unit", "currency"],
      expectedValueColumn: "sold_qty",
      observedValueColumn: "implied_sold_qty",
      comparison: "eq",
      observedTimestampColumn: null,
      maxAgeMs: null,
      expectedFilters: [["period_week", "2026-08-31"]],
      observedFilters: [["period_week", "2026-08-31"]],
    },
    {
      name: "supply.retail.rollup_margin_reconciliation",
      expectedTransformId: "supply.retail.store_sales_weekly_operands",
      observedTransformId: "supply.retail.store_rollup_sales_operands",
      keyColumns: ["store_id", "sku_id", "unit", "currency"],
      expectedValueColumn: "effective_gross_margin",
      observedValueColumn: "period_gross_margin",
      comparison: "eq",
      observedTimestampColumn: null,
      maxAgeMs: null,
      expectedFilters: [["period_week", "2026-08-31"]],
      observedFilters: [["period_week", "2026-08-31"]],
    },
    {
      name: "supply.retail.rollup_on_hand_reconciliation",
      expectedTransformId: "supply.retail.store_active_positions",
      observedTransformId: "supply.retail.store_rollup_sales_operands",
      keyColumns: ["store_id", "sku_id", "unit", "currency"],
      expectedValueColumn: "derived_on_hand_qty",
      observedValueColumn: "on_hand_qty",
      comparison: "eq",
      observedTimestampColumn: null,
      maxAgeMs: null,
      expectedFilters: [],
      observedFilters: [["period_week", "2026-08-31"]],
    },
    {
      name: "supply.retail.rollup_closing_value_reconciliation",
      expectedTransformId: "supply.retail.store_active_positions",
      observedTransformId: "supply.retail.store_rollup_sales_operands",
      keyColumns: ["store_id", "sku_id", "unit", "currency"],
      expectedValueColumn: "derived_stock_value",
      observedValueColumn: "closing_stock_value",
      comparison: "eq",
      observedTimestampColumn: null,
      maxAgeMs: null,
      expectedFilters: [],
      observedFilters: [["period_week", "2026-08-31"]],
    },
  ],
  expectedGrainGates: [
    {
      name: "supply.retail.store_weeks_of_supply_coverage",
      expectedTransformId: "supply.retail.store_active_positions",
      expectedColumns: ["store_id", "sku_id"],
      questionName: "supply.retail.store_weeks_of_supply_weekly",
      bucketIndices: [1, 2],
      requireNonNullValue: true,
      requiredBucketFilters: [[3, "2026-08-31"]],
    },
    {
      name: "supply.retail.store_gmroi_coverage",
      expectedTransformId: "supply.retail.store_active_positions",
      expectedColumns: ["store_id", "sku_id"],
      questionName: "supply.retail.store_gmroi_weekly",
      bucketIndices: [1, 2],
      requireNonNullValue: true,
      requiredBucketFilters: [[3, "2026-08-31"]],
    },
  ],
}
