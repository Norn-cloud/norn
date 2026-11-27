import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { CountEvidence, ReceiptEvidence, Sku } from "./supply.ts"

// Resource Planning Lab (S1: Retail Truth Slice): two-entity, multi-store retail
// definitions layered additively onto `supply-lab`. Legal ownership (`LegalEntity`)
// stays separate from physical custody (`custodian_id` on `Store` and `StockLot`),
// holding lots carry their receipt date (`received_at`) for aging cohorts, and
// missing cost is represented explicitly (`cost_status: "missing_cost"`) so
// margin and GMROI totals withhold instead of summing zero.

const Unit = Schema.Literals(["each", "kg", "l"])
const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const CostStatus = Schema.Literals(["costed", "missing_cost"])

const UnitLabels = {
  each: { en: "Each", ar: "قطعة" },
  kg: { en: "Kilogram", ar: "كيلوغرام" },
  l: { en: "Litre", ar: "لتر" },
}
const CostStatusLabels = {
  costed: { en: "Costed", ar: "مسعّر بالتكلفة" },
  missing_cost: { en: "Missing cost", ar: "تكلفة مفقودة" },
}

// ------------------------------------------------ legal entities & stores

const LegalEntityProps = Schema.Struct({
  entity_id: Schema.String,
  legal_name: Schema.String,
  entity_kind: Schema.Literals(["sole_proprietorship", "partnership", "corporation"]),
  book_code: Schema.String,
  functional_currency: Currency,
  status: Schema.Literals(["active", "closed"]),
})

/**
 * A legal entity and accounting book boundary (D02). Each legal entity owns its
 * own cell and functional currency so books never commingle.
 */
export const LegalEntity = Norn.type({
  name: "supply.LegalEntity", title: "Legal entity", version: "0.1.0",
  label: { en: "Legal entity", ar: "كيان قانوني" },
  properties: LegalEntityProps,
  propertyLabels: {
    entity_id: { en: "Entity code", ar: "رمز الكيان" },
    book_code: { en: "Book code", ar: "رمز الدفتر" },
    functional_currency: { en: "Functional currency", ar: "العملة الوظيفية" },
  },
  valueLabels: {
    entity_kind: {
      sole_proprietorship: { en: "Sole proprietorship", ar: "منشأة فردية" },
      partnership: { en: "Partnership", ar: "شركة تضامن" },
      corporation: { en: "Corporation", ar: "شركة مساهمة" },
    },
    status: { active: { en: "Active", ar: "نشط" }, closed: { en: "Closed", ar: "مغلق" } },
  },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["entity_id"] }],
  defaults: { status: "active" },
  lifecycles: [{
    property: "status", initial: ["active"], terminal: ["closed"], immutable: ["closed"],
    endings: { active: "resting", closed: "completed" },
    transitions: [{ id: "close", from: ["active"], to: "closed" }],
  }],
})

const StoreProps = Schema.Struct({
  store_id: Schema.String,
  name: Schema.String,
  entity_id: Schema.String,
  // Physical custodian (e.g. mall operator or branch manager), kept distinct from `entity_id` legal owner.
  custodian_id: Schema.String,
  location_kind: Schema.Literals(["mall_store", "standalone", "warehouse_outlet"]),
  mall_code: Schema.NullOr(Schema.String),
  currency: Currency,
  status: Schema.Literals(["active", "closed"]),
})

/**
 * A retail store location partition bound to its owning `LegalEntity`/book,
 * keeping physical custody (`custodian_id`) separate from legal ownership.
 */
export const Store = Norn.type({
  name: "supply.Store", title: "Retail store", version: "0.1.0",
  label: { en: "Retail store", ar: "متجر تجزئة" },
  properties: StoreProps,
  propertyLabels: {
    store_id: { en: "Store code", ar: "رمز المتجر" },
    entity_id: { en: "Owner legal entity", ar: "الكيان القانوني المالك" },
    custodian_id: { en: "Custodian", ar: "أمين العهدة" },
    mall_code: { en: "Mall code", ar: "رمز المركز التجاري" },
  },
  valueLabels: {
    location_kind: {
      mall_store: { en: "Mall store", ar: "متجر داخل مول" },
      standalone: { en: "Standalone store", ar: "متجر مستقل" },
      warehouse_outlet: { en: "Warehouse outlet", ar: "منفذ مستودع" },
    },
    status: { active: { en: "Active", ar: "نشط" }, closed: { en: "Closed", ar: "مغلق" } },
  },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["store_id"] }],
  defaults: { status: "active" },
  links: [
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true }, inverse: "stores" },
  ],
  lifecycles: [{
    property: "status", initial: ["active"], terminal: ["closed"], immutable: ["closed"],
    endings: { active: "resting", closed: "completed" },
    transitions: [{ id: "close", from: ["active"], to: "closed" }],
  }],
})

export const StoreMatchesOwnerCurrency = Norn.law("matches_owner_currency", {
  on: Store,
  must: (self) => self.currency === self.owner_entity.functional_currency,
  because: "A retail store must transact in its owning legal entity's functional currency.",
})

// -------------------------------------------------- holding lots & aging

const StockLotProps = Schema.Struct({
  lot_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  owner_entity_id: Schema.String,
  custodian_id: Schema.String,
  received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  remaining_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit"] }),
  // Kernel-derived cumulative depletion (`received_qty - remaining_qty`) for declared ratio-of-sums sell-through.
  depleted_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  // Lot sell-through rate: ratio of sums `sum(depleted_qty) / sum(received_qty)`, never a mean of line rates.
  sell_through_rate: Norn.decimal(6, { additivity: { kind: "ratio", numerator: "depleted_qty", denominator: "received_qty" } }),
  unit: Unit,
  // Immutable receipt date of the lot (drives aging cohorts and oldest-lot age).
  received_at: Norn.timestamp("utc"),
  // Valid time of the current remaining balance snapshot.
  as_of: Norn.timestamp("utc"),
  cost_status: CostStatus,
  unit_cost: Schema.NullOr(Norn.decimal(4, { additivity: { kind: "non_additive" } })),
  // Immutable receipt-time lot valuation (`received_qty * unit_cost`) so GMROI opening inventory never uses current cost.
  opening_stock_value: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  stock_value: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
  currency: Currency,
})

/**
 * One stock holding lot at a store: preserves original `received_at` for
 * aging, keeps legal ownership (`owner_entity`) distinct from custody
 * (`custodian_id`), and flags missing unit cost explicitly (`cost_status`).
 */
export const StockLot = Norn.type({
  name: "supply.StockLot", title: "Store stock lot", version: "0.1.0",
  label: { en: "Stock lot", ar: "دفعة مخزون" },
  properties: StockLotProps,
  valueLabels: { unit: UnitLabels, cost_status: CostStatusLabels },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["lot_id"] }],
  links: [
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true }, inverse: "lots" },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const StoreClosedRequiresNoStock = Norn.law("store_closed_requires_no_stock", {
  on: Store,
  inverses: { lots: StockLot },
  when: (self) => self.status === "closed",
  must: (self) => Norn.sum(self.lots.map((l) => l.remaining_qty)) === 0,
  because: "A retail store cannot close while stock lots still hold remaining quantity there.",
})

export const StockLotQuantitiesValid = Norn.law("lot_quantities_valid", {
  on: StockLot,
  must: (self) =>
    self.received_qty > 0
    && self.remaining_qty >= 0
    && self.remaining_qty <= self.received_qty
    && self.depleted_qty >= 0
    && (self.depleted_qty + self.remaining_qty) === self.received_qty,
  because: "A holding lot receives a positive quantity and conserves exact units (`depleted_qty + remaining_qty == received_qty`).",
})
export const StockLotValidHolds = Norn.example("a partly sold holding lot holds", {
  of: StockLotQuantitiesValid,
  given: {
    lot_id: "LOT-1", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 40, remaining_qty: 18, depleted_qty: 22, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-15T09:00:00Z",
    cost_status: "costed", unit_cost: 25, opening_stock_value: 1000, stock_value: 450, currency: "EGP",
  },
  expect: "holds",
})
export const StockLotNegativeRemainingViolated = Norn.example("a negative lot balance is refused", {
  of: StockLotQuantitiesValid,
  given: {
    lot_id: "LOT-2", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 40, remaining_qty: -2, depleted_qty: 42, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-15T09:00:00Z",
    cost_status: "costed", unit_cost: 25, opening_stock_value: 1000, stock_value: 0, currency: "EGP",
  },
  expect: "violated",
})
export const StockLotOverRemainingViolated = Norn.example("remaining above received is refused", {
  of: StockLotQuantitiesValid,
  given: {
    lot_id: "LOT-3", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 40, remaining_qty: 45, depleted_qty: 0, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-15T09:00:00Z",
    cost_status: "costed", unit_cost: 25, opening_stock_value: 1000, stock_value: 1125, currency: "EGP",
  },
  expect: "violated",
})

export const StockLotCostValid = Norn.law("lot_cost_valid", {
  on: StockLot,
  must: (self) =>
    (self.cost_status === "costed" && self.unit_cost !== null && self.opening_stock_value >= 0 && self.stock_value >= 0)
    || (self.cost_status === "missing_cost" && self.unit_cost === null && self.opening_stock_value === 0 && self.stock_value === 0),
  because: "A costed holding lot carries a non-null unit cost and non-negative opening/closing values; an uncosted lot carries null unit cost and zero placeholder values so totals withhold.",
})
export const StockLotMissingCostHolds = Norn.example("an explicitly uncosted lot holds", {
  of: StockLotCostValid,
  given: {
    lot_id: "LOT-4", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 20, remaining_qty: 20, depleted_qty: 0, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-01T09:00:00Z",
    cost_status: "missing_cost", unit_cost: null, opening_stock_value: 0, stock_value: 0, currency: "EGP",
  },
  expect: "holds",
})
export const StockLotCostedWithoutPriceViolated = Norn.example("a costed lot with null unit cost is refused", {
  of: StockLotCostValid,
  given: {
    lot_id: "LOT-5", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 20, remaining_qty: 20, depleted_qty: 0, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-01T09:00:00Z",
    cost_status: "costed", unit_cost: null, opening_stock_value: 0, stock_value: 0, currency: "EGP",
  },
  expect: "violated",
})

export const StockLotUnitCostPositive = Norn.law("lot_unit_cost_positive", {
  on: StockLot,
  must: (self) => (self.unit_cost ?? 1) > 0,
  because: "When a holding lot carries a unit cost, that unit cost must be strictly positive.",
})
export const StockLotPositiveCostHolds = Norn.example("a lot with positive unit cost holds", {
  of: StockLotUnitCostPositive,
  given: {
    lot_id: "LOT-6", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 20, remaining_qty: 20, depleted_qty: 0, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-01T09:00:00Z",
    cost_status: "costed", unit_cost: 25, opening_stock_value: 500, stock_value: 500, currency: "EGP",
  },
  expect: "holds",
})
export const StockLotNegativeCostViolated = Norn.example("a lot with negative unit cost is refused", {
  of: StockLotUnitCostPositive,
  given: {
    lot_id: "LOT-7", store_id: "ST-1", sku_id: "SKU-1", owner_entity_id: "LE-1", custodian_id: "CUST-1",
    received_qty: 20, remaining_qty: 20, depleted_qty: 0, sell_through_rate: 0, unit: "each",
    received_at: "2026-08-01T09:00:00Z", as_of: "2026-08-01T09:00:00Z",
    cost_status: "costed", unit_cost: -10, opening_stock_value: 0, stock_value: 0, currency: "EGP",
  },
  expect: "violated",
})

export const StockLotMatchesStoreAndSku = Norn.law("lot_matches_store_and_sku", {
  on: StockLot,
  must: (self) => self.unit === self.sku.unit_of_measure && self.currency === self.store.currency,
  because: "A stock lot must use its catalog SKU's unit of measure and its store's functional currency.",
})

// --------------------------------------------------- store physical counts

const StoreCountProps = Schema.Struct({
  store_count_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  on_hand_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit", "quantity_basis"] }),
  unit: Unit,
  quantity_basis: Schema.Literals(["on_hand", "available", "reserved"]),
  as_of: Norn.timestamp("utc"),
  count_source: Schema.Literals(["pos_cycle_count", "store_audit", "handheld_scan"]),
  count_ref: Schema.String,
})

/**
 * Governed physical store count position implementing `CountEvidence` (inherits
 * `on_hand_non_negative`). Versioned by `as_of` for count-staleness checks.
 */
export const StoreCount = Norn.type({
  name: "supply.StoreCount", title: "Store physical count", version: "0.1.0",
  label: { en: "Store count", ar: "جرد المتجر" },
  implements: [CountEvidence],
  properties: StoreCountProps,
  valueLabels: {
    unit: UnitLabels,
    quantity_basis: {
      on_hand: { en: "On hand", ar: "متوفر فعليًا" },
      available: { en: "Available", ar: "متاح" },
      reserved: { en: "Reserved", ar: "محجوز" },
    },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["store_count_id"] },
    { name: "position", properties: ["store_id", "sku_id", "quantity_basis"] },
  ],
  links: [
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const StoreCountMatchesSkuUnit = Norn.law("store_count_matches_sku_unit", {
  on: StoreCount,
  must: (self) => self.unit === self.sku.unit_of_measure,
  because: "A store physical count must record quantities in its catalog SKU's unit of measure.",
})

// ----------------------------------------------------- POS sales & margin

const SaleFacts = Schema.Struct({
  sold_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  unit: Unit,
  net_revenue: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})

/** Shared contract for every retail sale row, native or imported from POS. */
export const SaleEvidence = Norn.interfaceType({
  name: "supply.SaleEvidence", title: "POS sale evidence", version: "0.1.0",
  label: { en: "POS sale evidence", ar: "بيانات مبيعات نقطة البيع" },
  needsReview: ["ar"],
  disjointWith: [CountEvidence, ReceiptEvidence],
  properties: SaleFacts,
})

export const SalePositive = Norn.law("sale_positive", {
  on: SaleEvidence,
  must: (self) => self.sold_qty > 0 && self.net_revenue >= 0,
  because: "A POS sale line sells a positive quantity with non-negative net revenue; returns are modeled separately.",
})
export const SalePositiveHolds = Norn.example("a positive sale line holds", {
  of: SalePositive,
  given: { sold_qty: 3, unit: "each", net_revenue: 450, currency: "EGP", sold_at: "2026-08-10T14:00:00Z" },
  expect: "holds",
})
export const SaleZeroQtyViolated = Norn.example("a zero-quantity sale is refused", {
  of: SalePositive,
  given: { sold_qty: 0, unit: "each", net_revenue: 100, currency: "EGP", sold_at: "2026-08-10T14:00:00Z" },
  expect: "violated",
})
export const SaleNegativeRevenueViolated = Norn.example("negative net revenue is refused", {
  of: SalePositive,
  given: { sold_qty: 1, unit: "each", net_revenue: -50, currency: "EGP", sold_at: "2026-08-10T14:00:00Z" },
  expect: "violated",
})

const PosSaleProps = Schema.Struct({
  sale_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  sold_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  unit: Unit,
  net_revenue: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  applied_unit_cost: Schema.NullOr(Norn.decimal(4, { additivity: { kind: "non_additive" } })),
  cogs_amount: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  gross_margin: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  // Gross margin rate is a ratio of sums (`sum(gross_margin) / sum(net_revenue)`), never a mean of line rates.
  gross_margin_rate: Norn.decimal(6, { additivity: { kind: "ratio", numerator: "gross_margin", denominator: "net_revenue" } }),
  cost_status: CostStatus,
  // Non-null reference to the verified `UnitCost.cost_ref` when `cost_status === "costed"`; null when missing cost.
  costed_ref: Schema.NullOr(Schema.String),
  // Optional 1-to-1 reference to the originating `PosSaleEvidence.pos_line_id` when promoted from POS feed.
  source_evidence_id: Schema.NullOr(Schema.String),
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})

const CostFacts = Schema.Struct({
  unit_cost: Norn.decimal(4, { additivity: { kind: "non_additive" } }),
  currency: Currency,
  effective_from: Norn.timestamp("utc"),
})

/** Shared contract for unit-cost records and imported supplier unit-cost evidence. */
export const CostEvidence = Norn.interfaceType({
  name: "supply.CostEvidence", title: "Unit cost evidence", version: "0.1.0",
  label: { en: "Unit cost evidence", ar: "بيانات تكلفة الوحدة" },
  needsReview: ["ar"],
  disjointWith: [CountEvidence, ReceiptEvidence, SaleEvidence],
  properties: CostFacts,
})

export const UnitCostPositive = Norn.law("unit_cost_positive", {
  on: CostEvidence,
  must: (self) => self.unit_cost > 0,
  because: "Unit cost records and imported unit cost evidence must be strictly positive; zero or negative unit costs are refused or quarantined.",
})
export const UnitCostPositiveHolds = Norn.example("a positive unit cost holds", {
  of: UnitCostPositive,
  given: { unit_cost: 42.5, currency: "EGP", effective_from: "2026-08-01T00:00:00Z" },
  expect: "holds",
})
export const UnitCostZeroViolated = Norn.example("a zero unit cost is quarantined", {
  of: UnitCostPositive,
  given: { unit_cost: 0, currency: "EGP", effective_from: "2026-08-01T00:00:00Z" },
  expect: "violated",
})

/**
 * Governed active SKU unit-cost record keyed by `sku_id` inside one legal entity's
 * cell (D02), binding cost lookup to the exact SKU, currency, and effective date.
 */
export const UnitCost = Norn.type({
  name: "supply.UnitCost", title: "SKU unit cost", version: "0.1.0",
  label: { en: "SKU unit cost", ar: "تكلفة وحدة الصنف" },
  implements: [CostEvidence],
  properties: Schema.Struct({
    sku_id: Schema.String,
    cost_ref: Schema.String,
    owner_entity_id: Schema.String,
    supplier_id: Schema.String,
    unit_cost: Norn.decimal(4, { additivity: { kind: "non_additive" } }),
    currency: Currency,
    effective_from: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["sku_id"] }],
  links: [
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const UnitCostMatchesEntityCurrency = Norn.law("unit_cost_matches_entity_currency", {
  on: UnitCost,
  must: (self) => self.currency === self.owner_entity.functional_currency,
  because: "A governed unit cost record must be denominated in its owning legal entity's functional currency.",
})

/** Raw POS export line captured as evidence before governed promotion. */
export const PosSaleEvidence = Norn.type({
  name: "supply.PosSaleEvidence", title: "POS sale feed row", version: "0.1.0",
  label: { en: "POS sale row", ar: "سطر مبيعات نقطة البيع" },
  implements: [SaleEvidence],
  properties: Schema.Struct({
    pos_line_id: Schema.String,
    store_code: Schema.String,
    sku_code: Schema.String,
    sold_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    unit: Unit,
    net_revenue: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
    currency: Currency,
    sold_at: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["pos_line_id"] }],
})

/** Governed POS sale line bound to one store and SKU in the store's currency, with optional link to verified `UnitCost`. */
export const PosSale = Norn.type({
  name: "supply.PosSale", title: "POS sale line", version: "0.1.0",
  label: { en: "POS sale", ar: "مبيعات نقطة البيع" },
  implements: [SaleEvidence],
  properties: PosSaleProps,
  valueLabels: { unit: UnitLabels, cost_status: CostStatusLabels },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["sale_id"] }],
  links: [
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
    { id: "cost_record", target: UnitCost, cardinality: { to: "one", from: "many" } },
    { id: "source_evidence", target: PosSaleEvidence, cardinality: { to: "one", from: "one" } },
  ],
})

export const PosSaleCostValid = Norn.law("pos_sale_cost_valid", {
  on: PosSale,
  must: (self) =>
    (self.cost_status === "costed" && self.costed_ref !== null && self.applied_unit_cost !== null && self.cogs_amount >= 0)
    || (self.cost_status === "missing_cost" && self.costed_ref === null && self.applied_unit_cost === null && self.cogs_amount === 0),
  because: "A costed sale links a cost reference and applied unit cost with non-negative COGS; an uncosted sale carries null cost fields and zero placeholder COGS so margin totals withhold.",
})
export const PosSaleCostedHolds = Norn.example("a costed POS sale holds", {
  of: PosSaleCostValid,
  given: {
    sale_id: "SALE-1", store_id: "ST-1", sku_id: "SKU-1", sold_qty: 2, unit: "each",
    net_revenue: 300, applied_unit_cost: 90, cogs_amount: 180, gross_margin: 120, gross_margin_rate: 0,
    cost_status: "costed", costed_ref: "COST-1", source_evidence_id: null, currency: "EGP", sold_at: "2026-08-10T14:00:00Z",
  },
  expect: "holds",
})
export const PosSaleMissingCostHolds = Norn.example("an explicitly uncosted POS sale holds", {
  of: PosSaleCostValid,
  given: {
    sale_id: "SALE-2", store_id: "ST-1", sku_id: "SKU-1", sold_qty: 1, unit: "each",
    net_revenue: 150, applied_unit_cost: null, cogs_amount: 0, gross_margin: 0, gross_margin_rate: 0,
    cost_status: "missing_cost", costed_ref: null, source_evidence_id: null, currency: "EGP", sold_at: "2026-08-10T14:00:00Z",
  },
  expect: "holds",
})
export const PosSaleUncostedWithCogsViolated = Norn.example("missing_cost sale with nonzero COGS is refused", {
  of: PosSaleCostValid,
  given: {
    sale_id: "SALE-3", store_id: "ST-1", sku_id: "SKU-1", sold_qty: 1, unit: "each",
    net_revenue: 150, applied_unit_cost: null, cogs_amount: 50, gross_margin: 100, gross_margin_rate: 0,
    cost_status: "missing_cost", costed_ref: null, source_evidence_id: null, currency: "EGP", sold_at: "2026-08-10T14:00:00Z",
  },
  expect: "violated",
})

export const PosSaleMatchesStoreAndSku = Norn.law("pos_sale_matches_store_and_sku", {
  on: PosSale,
  must: (self) => self.unit === self.sku.unit_of_measure && self.currency === self.store.currency,
  because: "A POS sale must record quantities in its catalog SKU's unit of measure and amounts in its store's functional currency.",
})

export const PosSaleCostedRequiresCostLink = Norn.law("pos_sale_costed_requires_cost_link", {
  on: PosSale,
  must: (self) =>
    (self.cost_status === "costed" && self.cost_record !== null)
    || (self.cost_status === "missing_cost" && self.cost_record === null),
  because: "Every costed POS sale must link a governed UnitCost record, and an uncosted sale must carry no UnitCost link.",
})

export const PosSaleMatchesCostRecord = Norn.law("pos_sale_matches_cost_record", {
  on: PosSale,
  when: (self): self is typeof self & { readonly cost_record: NonNullable<typeof self.cost_record> } => self.cost_record !== null,
  must: (self) =>
    self.cost_status === "costed"
    && self.sku_id === self.cost_record.sku_id
    && self.currency === self.cost_record.currency
    && (self.costed_ref ?? "") === self.cost_record.cost_ref
    && (self.applied_unit_cost ?? -1) === self.cost_record.unit_cost
    && self.cost_record.effective_from <= self.sold_at,
  because: "A POS sale linked to a UnitCost record must be costed, match that UnitCost record's SKU, currency, reference, and unit cost, and occur on or after its effective date.",
})

export const PosSaleMatchesSourceEvidence = Norn.law("pos_sale_matches_source_evidence", {
  on: PosSale,
  when: (self): self is typeof self & { readonly source_evidence: NonNullable<typeof self.source_evidence> } => self.source_evidence !== null,
  must: (self) =>
    (self.source_evidence_id ?? "") === self.source_evidence.pos_line_id
    && self.store_id === self.source_evidence.store_code
    && self.sku_id === self.source_evidence.sku_code
    && self.sold_qty === self.source_evidence.sold_qty
    && self.unit === self.source_evidence.unit
    && self.net_revenue === self.source_evidence.net_revenue
    && self.currency === self.source_evidence.currency
    && self.sold_at === self.source_evidence.sold_at,
  because: "A POS sale promoted from POS evidence must preserve that evidence's exact line identity, store, SKU, quantity, unit, net revenue, currency, and timestamp, and link 1-to-1 so no source line can be promoted twice.",
})

// ----------------------------------------- governed store commercial rollups

const CountStatus = Schema.Literals(["verified", "unverified"])

const StoreCommercialRollupProps = Schema.Struct({
  rollup_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  // Canonical ISO week key (e.g. `"2026-08-31"`) enforcing one rollup per store-week.
  period_week: Schema.String,
  unit: Unit,
  currency: Currency,
  cost_status: CostStatus,
  count_status: CountStatus,
  on_hand_count_id: Schema.NullOr(Schema.String),
  count_ref: Schema.NullOr(Schema.String),
  count_as_of: Schema.NullOr(Norn.timestamp("utc")),
  costed_ref: Schema.NullOr(Schema.String),
  period_end: Norn.timestamp("utc"),
  on_hand_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  avg_weekly_sales_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  // Weeks of supply: ratio of sums `sum(on_hand_qty) / sum(avg_weekly_sales_qty)`.
  weeks_of_supply: Norn.decimal(6, { additivity: { kind: "ratio", numerator: "on_hand_qty", denominator: "avg_weekly_sales_qty" } }),
  period_gross_margin: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  opening_stock_value: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  closing_stock_value: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  average_inventory_value: Norn.decimal(6, { rounding: "half_even", additivity: { kind: "additive" }, unitSource: ["currency"] }),
  // GMROI: ratio of sums `sum(period_gross_margin) / sum(average_inventory_value)`.
  gmroi: Norn.decimal(6, { additivity: { kind: "ratio", numerator: "period_gross_margin", denominator: "average_inventory_value" } }),
})

/**
 * Governed store-week commercial KPI rollup unique at `(store_id, period_week)`
 * grain so a store-week cannot publish both a verified partition and a withheld
 * partition; bound to governed `StoreCount` and `UnitCost` evidence while
 * keeping future recounts unblocked.
 */
export const StoreCommercialRollup = Norn.type({
  name: "supply.StoreCommercialRollup", title: "Store commercial KPI rollup", version: "0.1.0",
  label: { en: "Store KPI rollup", ar: "ملخص مؤشرات المتجر" },
  properties: StoreCommercialRollupProps,
  valueLabels: {
    unit: UnitLabels,
    cost_status: CostStatusLabels,
    count_status: { verified: { en: "Verified", ar: "موثق" }, unverified: { en: "Unverified", ar: "غير موثق" } },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["rollup_id"] },
    { name: "store_week_grain", properties: ["store_id", "period_week"] },
  ],
  links: [
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
    { id: "on_hand_count", target: StoreCount, cardinality: { to: "one", from: "many" } },
    { id: "cost_record", target: UnitCost, cardinality: { to: "one", from: "many" } },
  ],
})

export const StoreCommercialRollupValid = Norn.law("store_commercial_rollup_valid", {
  on: StoreCommercialRollup,
  must: (self) =>
    (
      (self.count_status === "verified" && self.on_hand_count_id !== null && self.count_ref !== null && self.count_as_of !== null && self.on_hand_qty >= 0 && self.avg_weekly_sales_qty >= 0)
      || (self.count_status === "unverified" && self.on_hand_count_id === null && self.count_ref === null && self.count_as_of === null && self.on_hand_qty === 0 && self.avg_weekly_sales_qty === 0)
    )
    && (
      (self.cost_status === "costed" && self.costed_ref !== null && self.opening_stock_value >= 0 && self.closing_stock_value >= 0 && self.average_inventory_value >= 0)
      || (self.cost_status === "missing_cost" && self.costed_ref === null && self.period_gross_margin === 0 && self.opening_stock_value === 0 && self.closing_stock_value === 0 && self.average_inventory_value === 0)
    ),
  because: "A store-week KPI rollup zeroes WOS inputs when unverified and GMROI inputs when missing_cost so both ratios evaluate to null (withheld) directly on the read path.",
})

export const StoreCommercialRollupCountNotFuture = Norn.law("store_commercial_rollup_count_not_future", {
  on: StoreCommercialRollup,
  when: (self): self is typeof self & { readonly count_as_of: NonNullable<typeof self.count_as_of> } => self.count_as_of !== null,
  must: (self) => self.count_as_of <= self.period_end,
  because: "A verified commercial KPI rollup cannot cite a physical count dated after its period_end cutoff.",
})

export const StoreCommercialRollupRequiresEvidenceLinks = Norn.law("store_commercial_rollup_requires_evidence_links", {
  on: StoreCommercialRollup,
  must: (self) =>
    ((self.count_status === "verified" && self.on_hand_count !== null) || (self.count_status === "unverified" && self.on_hand_count === null))
    && ((self.cost_status === "costed" && self.cost_record !== null) || (self.cost_status === "missing_cost" && self.cost_record === null)),
  because: "A verified commercial KPI rollup must link its governed on-hand StoreCount and a costed rollup must link its governed UnitCost.",
})

export const StoreCommercialRollupMatchesCount = Norn.law("store_commercial_rollup_matches_count", {
  on: StoreCommercialRollup,
  when: (self): self is typeof self & { readonly on_hand_count: NonNullable<typeof self.on_hand_count> } => self.on_hand_count !== null,
  must: (self) =>
    (self.on_hand_count_id ?? "") === self.on_hand_count.store_count_id
    && self.store_id === self.on_hand_count.store_id
    && self.sku_id === self.on_hand_count.sku_id
    && self.unit === self.on_hand_count.unit
    && self.on_hand_count.quantity_basis === "on_hand",
  because: "A verified commercial KPI rollup must bind to an on-hand StoreCount for the same store, SKU, and unit while allowing subsequent period recounts.",
})

export const StoreCommercialRollupMatchesCost = Norn.law("store_commercial_rollup_matches_cost", {
  on: StoreCommercialRollup,
  when: (self): self is typeof self & { readonly cost_record: NonNullable<typeof self.cost_record> } => self.cost_record !== null,
  must: (self) =>
    self.sku_id === self.cost_record.sku_id
    && self.currency === self.cost_record.currency
    && (self.costed_ref ?? "") === self.cost_record.cost_ref
    && self.cost_record.effective_from <= self.period_end,
  because: "A costed commercial KPI rollup must match its linked UnitCost's SKU, currency, reference, and effective_from <= period_end.",
})

export const StoreCommercialRollupMatchesStoreAndSku = Norn.law("store_commercial_rollup_matches_store_and_sku", {
  on: StoreCommercialRollup,
  must: (self) => self.unit === self.sku.unit_of_measure && self.currency === self.store.currency,
  because: "A store commercial KPI rollup must use its SKU unit of measure and its store's functional currency.",
})

// ------------------------------------------------- messy feed evidence types

/** Raw store physical count scan row captured as evidence before governed promotion. */
export const StoreCountEvidence = Norn.type({
  name: "supply.StoreCountEvidence", title: "Store count feed row", version: "0.1.0",
  label: { en: "Store count row", ar: "سطر جرد المتجر" },
  implements: [CountEvidence],
  properties: Schema.Struct({
    count_id: Schema.String,
    store_code: Schema.String,
    sku_code: Schema.String,
    on_hand_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit", "quantity_basis"] }),
    unit: Unit,
    quantity_basis: Schema.Literals(["on_hand", "available", "reserved"]),
    as_of: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["count_id"] }],
})

/** Raw retail supplier receipt row with optional unit cost, captured as evidence. */
export const RetailReceiptEvidence = Norn.type({
  name: "supply.RetailReceiptEvidence", title: "Retail receipt feed row", version: "0.1.0",
  label: { en: "Retail receipt row", ar: "سطر استلام التجزئة" },
  implements: [ReceiptEvidence],
  properties: Schema.Struct({
    receipt_line_id: Schema.String,
    store_code: Schema.String,
    sku_code: Schema.String,
    supplier_id: Schema.String,
    received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    damaged_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    unit: Unit,
    unit_cost: Schema.NullOr(Norn.decimal(4, { currency: "currency", additivity: { kind: "non_additive" } })),
    currency: Currency,
    received_at: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["receipt_line_id"] }],
})

export const RetailReceiptCostValid = Norn.law("retail_receipt_cost_valid", {
  on: RetailReceiptEvidence,
  must: (self) => (self.unit_cost ?? 1) > 0,
  because: "A retail receipt row either omits unit cost (missing cost) or states a strictly positive unit cost.",
})
export const RetailReceiptPositiveCostHolds = Norn.example("a receipt row with positive cost holds", {
  of: RetailReceiptCostValid,
  given: {
    receipt_line_id: "RR-1", store_code: "ST-1", sku_code: "SKU-1", supplier_id: "SUP-NILE",
    received_qty: 20, damaged_qty: 0, unit: "each", unit_cost: 12.5, currency: "EGP", received_at: "2026-08-01T08:00:00Z",
  },
  expect: "holds",
})
export const RetailReceiptMissingCostHolds = Norn.example("a receipt row with null cost holds as missing cost", {
  of: RetailReceiptCostValid,
  given: {
    receipt_line_id: "RR-1b", store_code: "ST-1", sku_code: "SKU-1", supplier_id: "SUP-NILE",
    received_qty: 20, damaged_qty: 0, unit: "each", unit_cost: null, currency: "EGP", received_at: "2026-08-01T08:00:00Z",
  },
  expect: "holds",
})
export const RetailReceiptNegativeCostViolated = Norn.example("a receipt row with negative unit cost is quarantined", {
  of: RetailReceiptCostValid,
  given: {
    receipt_line_id: "RR-2", store_code: "ST-1", sku_code: "SKU-1", supplier_id: "SUP-NILE",
    received_qty: 20, damaged_qty: 0, unit: "each", unit_cost: -5, currency: "EGP", received_at: "2026-08-01T08:00:00Z",
  },
  expect: "violated",
})

/** Raw supplier unit-cost feed row captured as evidence. */
export const UnitCostEvidence = Norn.type({
  name: "supply.UnitCostEvidence", title: "Unit cost feed row", version: "0.1.0",
  label: { en: "Unit cost row", ar: "سطر تكلفة الوحدة" },
  implements: [CostEvidence],
  properties: Schema.Struct({
    cost_id: Schema.String,
    sku_code: Schema.String,
    supplier_id: Schema.String,
    unit_cost: Norn.decimal(4, { additivity: { kind: "non_additive" } }),
    currency: Currency,
    effective_from: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["cost_id"] }],
})
