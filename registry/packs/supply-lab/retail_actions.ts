import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Sku } from "./supply.ts"
import { LegalEntity, PosSale, PosSaleEvidence, StockLot, Store, StoreCommercialRollup, StoreCount, UnitCost } from "./retail.ts"

const Scope = ["orgId", "environmentId"] as const
const Unit = Schema.Literals(["each", "kg", "l"])
const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const CostStatus = Schema.Literals(["costed", "missing_cost"])
const QuantityBasis = Schema.Literals(["on_hand", "available", "reserved"])
const RetailAccessRef = { artifactId: "supply.retail.access.policy", version: "0.1.0" } as const

// ------------------------------------------------ legal entities & stores

const LegalEntityInput = Schema.Struct({
  entity_id: Schema.String,
  legal_name: Schema.String,
  entity_kind: Schema.Literals(["sole_proprietorship", "partnership", "corporation"]),
  book_code: Schema.String,
  functional_currency: Currency,
})
const LegalEntityRef = Schema.Struct({ entity_id: Schema.String })

export const RegisterLegalEntity = Norn.action({
  id: "supply.legal_entity.register", title: "Register a legal entity", version: "0.1.0",
  label: { en: "Register legal entity", ar: "تسجيل كيان قانوني" },
  input: LegalEntityInput, output: LegalEntityRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  aggregates: [{
    name: "existing_books",
    type: LegalEntity,
    aggregate: { kind: "count" },
    maxItems: 10,
  }],
  preconditions: [Norn.expression(
    Schema.Struct({ aggregates: Schema.Struct({ existing_books: Schema.Int }) }),
    (ctx) => ctx.aggregates.existing_books === 0,
    { root: "context" },
  )],
  writes: [{ type: LegalEntity, change: "created" }],
  effect: { kind: "upsert_input_object", type: LegalEntity, objectIdField: "entity_id", writeMode: { kind: "create_only" } },
})

export const CloseLegalEntity = Norn.action({
  id: "supply.legal_entity.close", title: "Close a legal entity", version: "0.1.0",
  label: { en: "Close legal entity", ar: "إغلاق كيان قانوني" },
  input: LegalEntityRef, output: LegalEntityRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  writes: [{ type: LegalEntity, change: "modified" }],
  effect: { kind: "fire_transition", type: LegalEntity, objectIdField: "entity_id", lifecycle: "status", transition: "close" },
})

const StoreInput = Schema.Struct({
  store_id: Schema.String,
  name: Schema.String,
  entity_id: Schema.String,
  custodian_id: Schema.String,
  location_kind: Schema.Literals(["mall_store", "standalone", "warehouse_outlet"]),
  mall_code: Schema.NullOr(Schema.String),
  currency: Currency,
})
const StoreRef = Schema.Struct({ store_id: Schema.String })

/** Register a retail store bound to an active owning legal entity in that entity's currency. */
export const RegisterStore = Norn.action({
  id: "supply.store.register", title: "Register a retail store", version: "0.1.0",
  label: { en: "Register store", ar: "تسجيل متجر" },
  input: StoreInput, output: StoreRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [{ name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StoreInput,
      slice: Schema.Struct({ owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }) }),
    }),
    (ctx) => ctx.slice.owner_entity.status === "active" && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [{ link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" }],
  writes: [{ type: Store, change: "created" }],
  effect: { kind: "upsert_input_object", type: Store, objectIdField: "store_id", writeMode: { kind: "create_only" } },
})

export const CloseStore = Norn.action({
  id: "supply.store.close", title: "Close a retail store", version: "0.1.0",
  label: { en: "Close store", ar: "إغلاق متجر" },
  input: StoreRef, output: StoreRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  writes: [{ type: Store, change: "modified" }],
  effect: { kind: "fire_transition", type: Store, objectIdField: "store_id", lifecycle: "status", transition: "close" },
})

// -------------------------------------------------- governed unit costs

const UnitCostInput = Schema.Struct({
  sku_id: Schema.String,
  cost_ref: Schema.String,
  owner_entity_id: Schema.String,
  supplier_id: Schema.String,
  unit_cost: Norn.decimal(4),
  currency: Currency,
  effective_from: Norn.timestamp("utc"),
})
const UnitCostRef = Schema.Struct({ sku_id: Schema.String })

/** Register the active governed SKU unit cost record inside one owning legal entity's cell. */
export const RecordUnitCost = Norn.action({
  id: "supply.unit_cost.record", title: "Record a SKU unit cost", version: "0.1.0",
  label: { en: "Record unit cost", ar: "تسجيل تكلفة الوحدة" },
  input: UnitCostInput, output: UnitCostRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "owner_entity_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: UnitCostInput,
      slice: Schema.Struct({ owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }) }),
    }),
    (ctx) => ctx.slice.owner_entity.status === "active" && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "owner_entity_id" },
  ],
  writes: [{ type: UnitCost, change: "created" }],
  effect: { kind: "upsert_input_object", type: UnitCost, objectIdField: "sku_id", writeMode: { kind: "create_only" } },
})

// -------------------------------------------------- holding lots & aging

const CostedStockLotInput = Schema.Struct({
  lot_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  owner_entity_id: Schema.String,
  custodian_id: Schema.String,
  received_qty: Norn.decimal(6),
  remaining_qty: Norn.decimal(6),
  depleted_qty: Norn.decimal(6),
  sell_through_rate: Norn.decimal(6),
  unit: Unit,
  received_at: Norn.timestamp("utc"),
  as_of: Norn.timestamp("utc"),
  cost_status: CostStatus,
  unit_cost: Norn.decimal(4),
  opening_stock_value: Norn.decimal(6),
  stock_value: Norn.decimal(6),
  currency: Currency,
})
const UncostedStockLotInput = Schema.Struct({
  lot_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  owner_entity_id: Schema.String,
  custodian_id: Schema.String,
  received_qty: Norn.decimal(6),
  remaining_qty: Norn.decimal(6),
  depleted_qty: Norn.decimal(6),
  sell_through_rate: Norn.decimal(6),
  unit: Unit,
  received_at: Norn.timestamp("utc"),
  as_of: Norn.timestamp("utc"),
  cost_status: CostStatus,
  opening_stock_value: Norn.decimal(6),
  stock_value: Norn.decimal(6),
  currency: Currency,
})
const StockLotRef = Schema.Struct({ lot_id: Schema.String })

/**
 * Open a costed store holding lot with its receipt date and custody.
 * Enforces that the lot's legal owner matches its store's owning legal entity
 * and derives `depleted_qty`, immutable `opening_stock_value`, and `stock_value` in the kernel.
 */
export const RecordStockLot = Norn.action({
  id: "supply.stock_lot.record", title: "Record a costed store stock lot", version: "0.1.0",
  label: { en: "Record stock lot", ar: "تسجيل دفعة مخزون" },
  input: CostedStockLotInput, output: StockLotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "store", property: "entity_id" } } },
  ],
  derive: [
    { field: "depleted_qty", inputRequired: false, expression: Norn.expression(CostedStockLotInput, (x) => x.received_qty - x.remaining_qty) },
    { field: "sell_through_rate", inputRequired: false, expression: Norn.expression(CostedStockLotInput, () => 0) },
    { field: "opening_stock_value", inputRequired: false, expression: Norn.expression(CostedStockLotInput, (x) => x.received_qty * x.unit_cost) },
    { field: "stock_value", inputRequired: false, expression: Norn.expression(CostedStockLotInput, (x) => x.remaining_qty * x.unit_cost) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: CostedStockLotInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.slice.owner_entity.status === "active"
      && ctx.input.cost_status === "costed"
      && ctx.input.owner_entity_id === ctx.slice.store.entity_id
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: StockLot, change: "created" }],
  effect: { kind: "upsert_input_object", type: StockLot, objectIdField: "lot_id", writeMode: { kind: "create_only" } },
})

/**
 * Open an explicitly uncosted store holding lot (`cost_status: "missing_cost"`,
 * `unit_cost: null`, `opening_stock_value: 0`, `stock_value: 0`) so valuation and GMROI fail closed.
 */
export const RecordUncostedStockLot = Norn.action({
  id: "supply.stock_lot.record_uncosted", title: "Record an uncosted store stock lot", version: "0.1.0",
  label: { en: "Record uncosted stock lot", ar: "تسجيل دفعة مخزون بدون تكلفة" },
  input: UncostedStockLotInput, output: StockLotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "store", property: "entity_id" } } },
  ],
  derive: [
    { field: "depleted_qty", inputRequired: false, expression: Norn.expression(UncostedStockLotInput, (x) => x.received_qty - x.remaining_qty) },
    { field: "sell_through_rate", inputRequired: false, expression: Norn.expression(UncostedStockLotInput, () => 0) },
    { field: "opening_stock_value", inputRequired: false, expression: Norn.expression(UncostedStockLotInput, () => 0) },
    { field: "stock_value", inputRequired: false, expression: Norn.expression(UncostedStockLotInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: UncostedStockLotInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.slice.owner_entity.status === "active"
      && ctx.input.cost_status === "missing_cost"
      && ctx.input.owner_entity_id === ctx.slice.store.entity_id
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: StockLot, change: "created" }],
  effect: { kind: "upsert_input_object", type: StockLot, objectIdField: "lot_id", writeMode: { kind: "create_only" } },
})

const UpdateStockLotBalanceInput = Schema.Struct({
  lot_id: Schema.String,
  remaining_qty: Norn.decimal(6),
  depleted_qty: Norn.decimal(6),
  as_of: Norn.timestamp("utc"),
  unit_cost: Norn.decimal(4),
  stock_value: Norn.decimal(6),
})

/**
 * Update a costed holding lot's remaining balance and valuation; `received_at`,
 * `received_qty`, `opening_stock_value`, `store_id`, `sku_id`, `owner_entity_id`,
 * `unit`, and `currency` are omitted from the update schema so they remain
 * structurally immutable.
 */
export const UpdateStockLotBalance = Norn.action({
  id: "supply.stock_lot.update_balance", title: "Update a stock lot balance", version: "0.1.0",
  label: { en: "Update lot balance", ar: "تحديث رصيد الدفعة" },
  input: UpdateStockLotBalanceInput, output: StockLotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "lot", type: StockLot, key: { property: { from: "input", property: "lot_id" } } },
    { name: "store", type: Store, key: { property: { from: "lot", property: "store_id" } } },
  ],
  derive: [
    { field: "stock_value", inputRequired: false, expression: Norn.expression(UpdateStockLotBalanceInput, (x) => x.remaining_qty * x.unit_cost) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: UpdateStockLotBalanceInput,
      slice: Schema.Struct({
        lot: Schema.Struct({ cost_status: Schema.String, received_qty: Norn.decimal(6) }),
        store: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.slice.lot.cost_status === "costed"
      && (ctx.input.depleted_qty + ctx.input.remaining_qty) === ctx.slice.lot.received_qty,
    { root: "context" },
  )],
  writes: [{ type: StockLot, change: "modified" }],
  effect: { kind: "upsert_input_object", type: StockLot, objectIdField: "lot_id", writeMode: { kind: "update_only" } },
})

// --------------------------------------------------- store physical counts

const StoreCountInput = Schema.Struct({
  store_count_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  on_hand_qty: Norn.decimal(6),
  unit: Unit,
  quantity_basis: QuantityBasis,
  as_of: Norn.timestamp("utc"),
  count_source: Schema.Literals(["pos_cycle_count", "store_audit", "handheld_scan"]),
  count_ref: Schema.String,
})
const StoreCountRef = Schema.Struct({ store_count_id: Schema.String })

export const RecordStoreCount = Norn.action({
  id: "supply.store_count.record", title: "Open a store physical count position", version: "0.1.0",
  label: { en: "Record store count", ar: "تسجيل جرد متجر" },
  input: StoreCountInput, output: StoreCountRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StoreCountInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active" && ctx.input.unit === ctx.slice.sku.unit_of_measure,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: StoreCount, change: "created" }],
  effect: { kind: "upsert_input_object", type: StoreCount, objectIdField: "store_count_id", writeMode: { kind: "create_only" } },
})

export const RecountStore = Norn.action({
  id: "supply.store_count.recount", title: "Record a subsequent store physical count", version: "0.1.0",
  label: { en: "Recount store", ar: "إعادة جرد المتجر" },
  input: StoreCountInput, output: StoreCountRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "count", type: StoreCount, key: { property: { from: "input", property: "store_count_id" } } },
    { name: "store", type: Store, key: { property: { from: "count", property: "store_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StoreCountInput,
      slice: Schema.Struct({
        count: Schema.Struct({ store_id: Schema.String, sku_id: Schema.String, unit: Schema.String, quantity_basis: Schema.String }),
        store: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.slice.count.store_id === ctx.input.store_id
      && ctx.slice.count.sku_id === ctx.input.sku_id
      && ctx.slice.count.unit === ctx.input.unit
      && ctx.slice.count.quantity_basis === ctx.input.quantity_basis,
    { root: "context" },
  )],
  writes: [{ type: StoreCount, change: "modified" }],
  effect: { kind: "upsert_input_object", type: StoreCount, objectIdField: "store_count_id", writeMode: { kind: "update_only" } },
})

// ------------------------------------------------------------- POS sales

const CostedPosSaleInput = Schema.Struct({
  sale_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  sold_qty: Norn.decimal(6),
  unit: Unit,
  net_revenue: Norn.decimal(6),
  applied_unit_cost: Norn.decimal(4),
  cogs_amount: Norn.decimal(6),
  gross_margin: Norn.decimal(6),
  gross_margin_rate: Norn.decimal(6),
  cost_status: CostStatus,
  costed_ref: Schema.String,
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})
const UncostedPosSaleInput = Schema.Struct({
  sale_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  sold_qty: Norn.decimal(6),
  unit: Unit,
  net_revenue: Norn.decimal(6),
  cogs_amount: Norn.decimal(6),
  gross_margin: Norn.decimal(6),
  gross_margin_rate: Norn.decimal(6),
  cost_status: CostStatus,
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})
const PosSaleRef = Schema.Struct({ sale_id: Schema.String })

/**
 * Record a costed POS sale line linked to the SKU's governed `UnitCost` record.
 * Validates `applied_unit_cost === cost.unit_cost` and `costed_ref === cost.cost_ref`,
 * then derives `cogs_amount = sold_qty * applied_unit_cost` and
 * `gross_margin = net_revenue - (sold_qty * applied_unit_cost)` in the kernel
 * (never trusting caller-supplied COGS, and never storing an inexact line ratio).
 */
export const RecordPosSale = Norn.action({
  id: "supply.pos_sale.record", title: "Record a costed POS sale line", version: "0.1.0",
  label: { en: "Record POS sale", ar: "تسجيل مبيعات نقطة البيع" },
  input: CostedPosSaleInput, output: PosSaleRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "cost", type: UnitCost, key: { property: { from: "input", property: "sku_id" } } },
  ],
  derive: [
    { field: "cogs_amount", inputRequired: false, expression: Norn.expression(CostedPosSaleInput, (x) => x.sold_qty * x.applied_unit_cost) },
    {
      field: "gross_margin", inputRequired: false,
      expression: Norn.expression(CostedPosSaleInput, (x) => x.net_revenue - (x.sold_qty * x.applied_unit_cost)),
    },
    { field: "gross_margin_rate", inputRequired: false, expression: Norn.expression(CostedPosSaleInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: CostedPosSaleInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
        cost: Schema.Struct({ currency: Schema.String, cost_ref: Schema.String, unit_cost: Norn.decimal(4) }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.input.cost_status === "costed"
      && ctx.input.costed_ref === ctx.slice.cost.cost_ref
      && ctx.input.applied_unit_cost === ctx.slice.cost.unit_cost
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.currency === ctx.slice.cost.currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "cost_record", targetType: UnitCost, targetIdField: "sku_id" },
  ],
  writes: [{ type: PosSale, change: "created" }],
  effect: { kind: "upsert_input_object", type: PosSale, objectIdField: "sale_id", writeMode: { kind: "create_only" } },
})

/**
 * Record an explicitly uncosted POS sale line (`cost_status: "missing_cost"`,
 * `costed_ref: null`, `applied_unit_cost: null`, `cogs_amount: 0`) so missing cost
 * causes margin and GMROI buckets to fail closed (withheld).
 */
export const RecordUncostedPosSale = Norn.action({
  id: "supply.pos_sale.record_uncosted", title: "Record an uncosted POS sale line", version: "0.1.0",
  label: { en: "Record uncosted POS sale", ar: "تسجيل مبيعات بدون تكلفة" },
  input: UncostedPosSaleInput, output: PosSaleRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  derive: [
    { field: "cogs_amount", inputRequired: false, expression: Norn.expression(UncostedPosSaleInput, () => 0) },
    { field: "gross_margin", inputRequired: false, expression: Norn.expression(UncostedPosSaleInput, () => 0) },
    { field: "gross_margin_rate", inputRequired: false, expression: Norn.expression(UncostedPosSaleInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: UncostedPosSaleInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.input.cost_status === "missing_cost"
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: PosSale, change: "created" }],
  effect: { kind: "upsert_input_object", type: PosSale, objectIdField: "sale_id", writeMode: { kind: "create_only" } },
})

const PromotedPosSaleInput = Schema.Struct({
  sale_id: Schema.String,
  source_evidence_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  sold_qty: Norn.decimal(6),
  unit: Unit,
  net_revenue: Norn.decimal(6),
  applied_unit_cost: Norn.decimal(4),
  cogs_amount: Norn.decimal(6),
  gross_margin: Norn.decimal(6),
  gross_margin_rate: Norn.decimal(6),
  cost_status: CostStatus,
  costed_ref: Schema.String,
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})

/**
 * Promote a verified `PosSaleEvidence` row into a governed costed `PosSale` with
 * a 1-to-1 `source_evidence` link, structurally preventing double-promotion of
 * the same POS feed line under any sale ID or action key.
 */
export const PromotePosSaleFromEvidence = Norn.action({
  id: "supply.pos_sale.promote_from_evidence", title: "Promote verified POS evidence to governed sale", version: "0.1.0",
  label: { en: "Promote POS sale evidence", ar: "ترقية بيانات مبيعات نقطة البيع" },
  input: PromotedPosSaleInput, output: PosSaleRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "evidence", type: PosSaleEvidence, key: { property: { from: "input", property: "source_evidence_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "cost", type: UnitCost, key: { property: { from: "input", property: "sku_id" } } },
  ],
  derive: [
    { field: "cogs_amount", inputRequired: false, expression: Norn.expression(PromotedPosSaleInput, (x) => x.sold_qty * x.applied_unit_cost) },
    {
      field: "gross_margin", inputRequired: false,
      expression: Norn.expression(PromotedPosSaleInput, (x) => x.net_revenue - (x.sold_qty * x.applied_unit_cost)),
    },
    { field: "gross_margin_rate", inputRequired: false, expression: Norn.expression(PromotedPosSaleInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PromotedPosSaleInput,
      slice: Schema.Struct({
        evidence: Schema.Struct({
          store_code: Schema.String, sku_code: Schema.String,
          sold_qty: Norn.decimal(6), unit: Schema.String,
          net_revenue: Norn.decimal(6), currency: Schema.String,
        }),
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
        cost: Schema.Struct({ currency: Schema.String, cost_ref: Schema.String, unit_cost: Norn.decimal(4) }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.input.cost_status === "costed"
      && ctx.input.store_id === ctx.slice.evidence.store_code
      && ctx.input.sku_id === ctx.slice.evidence.sku_code
      && ctx.input.sold_qty === ctx.slice.evidence.sold_qty
      && ctx.input.unit === ctx.slice.evidence.unit
      && ctx.input.net_revenue === ctx.slice.evidence.net_revenue
      && ctx.input.currency === ctx.slice.evidence.currency
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.currency === ctx.slice.cost.currency
      && ctx.input.costed_ref === ctx.slice.cost.cost_ref
      && ctx.input.applied_unit_cost === ctx.slice.cost.unit_cost,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "cost_record", targetType: UnitCost, targetIdField: "sku_id" },
    { link: "source_evidence", targetType: PosSaleEvidence, targetIdField: "source_evidence_id" },
  ],
  writes: [{ type: PosSale, change: "created" }],
  effect: { kind: "upsert_input_object", type: PosSale, objectIdField: "sale_id", writeMode: { kind: "create_only" } },
})

// ----------------------------------------- governed store commercial rollups

const CountStatus = Schema.Literals(["verified", "unverified"])

const StoreCommercialRollupInput = Schema.Struct({
  rollup_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  period_week: Schema.String,
  unit: Unit,
  currency: Currency,
  cost_status: CostStatus,
  count_status: CountStatus,
  on_hand_count_id: Schema.String,
  count_ref: Schema.String,
  count_as_of: Norn.timestamp("utc"),
  costed_ref: Schema.String,
  book_epoch: Norn.timestamp("utc"),
  period_start: Norn.timestamp("utc"),
  period_end: Norn.timestamp("utc"),
  sales_weeks: Schema.Int,
  on_hand_qty: Norn.decimal(6),
  avg_weekly_sales_qty: Norn.decimal(6),
  weeks_of_supply: Norn.decimal(6),
  period_gross_margin: Norn.decimal(6),
  opening_stock_value: Norn.decimal(6),
  closing_stock_value: Norn.decimal(6),
  average_inventory_value: Norn.decimal(6, { rounding: "half_even" }),
  gmroi: Norn.decimal(6),
})
const WithheldStoreCommercialRollupInput = Schema.Struct({
  rollup_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  period_week: Schema.String,
  unit: Unit,
  currency: Currency,
  cost_status: CostStatus,
  count_status: CountStatus,
  period_start: Norn.timestamp("utc"),
  period_end: Norn.timestamp("utc"),
  sales_weeks: Schema.Int,
  on_hand_qty: Norn.decimal(6),
  avg_weekly_sales_qty: Norn.decimal(6),
  weeks_of_supply: Norn.decimal(6),
  period_gross_margin: Norn.decimal(6),
  opening_stock_value: Norn.decimal(6),
  closing_stock_value: Norn.decimal(6),
  average_inventory_value: Norn.decimal(6, { rounding: "half_even" }),
  gmroi: Norn.decimal(6),
})
const StoreCommercialRollupRef = Schema.Struct({ rollup_id: Schema.String })

/**
 * Record a verified and costed store-week commercial KPI rollup bound at
 * snapshot time to governed `(store_id, sku_id)` `on_hand` `StoreCount`,
 * `UnitCost`, and windowed `PosSale` + `StockLot` kernel aggregates (including
 * carried-in opening lots received at or before `period_end`, refusing any
 * post-cutoff lot balance updates `as_of > period_end`, deriving `sales_weeks`
 * strictly from `[period_start, period_week]`, and refusing missing-cost rows).
 */
export const RecordStoreCommercialRollup = Norn.action({
  id: "supply.store_kpi.record", title: "Record verified store commercial KPI rollup", version: "0.1.0",
  label: { en: "Record store KPI rollup", ar: "تسجيل ملخص مؤشرات المتجر" },
  input: StoreCommercialRollupInput, output: StoreCommercialRollupRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "on_hand_count", type: StoreCount, key: { property: { from: "input", property: "on_hand_count_id" } } },
    { name: "cost", type: UnitCost, key: { property: { from: "input", property: "sku_id" } } },
  ],
  aggregates: [
    {
      name: "total_sold_qty", type: PosSale,
      filter: { store_id: "store_id", sku_id: "sku_id", unit: "unit" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: true },
      aggregate: { kind: "sum", property: "sold_qty" }, maxItems: 100,
    },
    {
      name: "total_gross_margin", type: PosSale,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: true },
      aggregate: { kind: "sum", property: "gross_margin" }, maxItems: 100,
    },
    {
      name: "all_sales_count", type: PosSale,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: true },
      aggregate: { kind: "count" }, maxItems: 100,
    },
    {
      name: "costed_sales_count", type: PosSale,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency", cost_status: "cost_status" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: true },
      aggregate: { kind: "count" }, maxItems: 100,
    },
    {
      name: "total_opening_stock_value", type: StockLot,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency" },
      window: { property: "received_at", start: "book_epoch", end: "period_end", startInclusive: true, endInclusive: true },
      aggregate: { kind: "sum", property: "opening_stock_value" }, maxItems: 100,
    },
    {
      name: "total_closing_stock_value", type: StockLot,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency" },
      aggregate: { kind: "sum", property: "stock_value" }, maxItems: 100,
    },
    {
      name: "all_lots_count", type: StockLot,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency" },
      aggregate: { kind: "count" }, maxItems: 100,
    },
    {
      name: "lots_as_of_by_period_end", type: StockLot,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency" },
      window: { property: "as_of", start: "book_epoch", end: "period_end", startInclusive: true, endInclusive: true },
      aggregate: { kind: "count" }, maxItems: 100,
    },
    {
      name: "costed_lots_count", type: StockLot,
      filter: { store_id: "store_id", sku_id: "sku_id", currency: "currency", cost_status: "cost_status" },
      aggregate: { kind: "count" }, maxItems: 100,
    },
  ],
  derive: [
    { field: "weeks_of_supply", inputRequired: false, expression: Norn.expression(StoreCommercialRollupInput, () => 0) },
    {
      field: "average_inventory_value", inputRequired: false,
      expression: Norn.expression(StoreCommercialRollupInput, (x) => (x.opening_stock_value + x.closing_stock_value) * 0.5),
    },
    { field: "gmroi", inputRequired: false, expression: Norn.expression(StoreCommercialRollupInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StoreCommercialRollupInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
        on_hand_count: Schema.Struct({
          store_id: Schema.String, sku_id: Schema.String,
          unit: Schema.String, quantity_basis: Schema.String,
          as_of: Norn.timestamp("utc"),
          count_ref: Schema.String, on_hand_qty: Norn.decimal(6),
        }),
        cost: Schema.Struct({ currency: Schema.String, cost_ref: Schema.String }),
      }),
      aggregates: Schema.Struct({
        total_sold_qty: Norn.decimal(6),
        total_gross_margin: Norn.decimal(6),
        all_sales_count: Schema.Int,
        costed_sales_count: Schema.Int,
        total_opening_stock_value: Norn.decimal(6),
        total_closing_stock_value: Norn.decimal(6),
        all_lots_count: Schema.Int,
        lots_as_of_by_period_end: Schema.Int,
        costed_lots_count: Schema.Int,
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.input.book_epoch === "1970-01-01T00:00:00Z"
      && ctx.input.count_status === "verified"
      && ctx.input.cost_status === "costed"
      && ctx.input.sales_weeks >= 1
      && Norn.secondsBetween(ctx.input.period_start, ctx.input.period_week + "T00:00:00Z")
        === (ctx.input.sales_weeks - 1) * 604800
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.store_id === ctx.slice.on_hand_count.store_id
      && ctx.input.sku_id === ctx.slice.on_hand_count.sku_id
      && ctx.input.unit === ctx.slice.on_hand_count.unit
      && ctx.slice.on_hand_count.quantity_basis === "on_hand"
      && Norn.parseTimestamp(ctx.input.count_as_of) === Norn.parseTimestamp(ctx.slice.on_hand_count.as_of)
      && ctx.input.count_ref === ctx.slice.on_hand_count.count_ref
      && ctx.input.on_hand_qty === ctx.slice.on_hand_count.on_hand_qty
      && ctx.input.currency === ctx.slice.cost.currency
      && ctx.input.costed_ref === ctx.slice.cost.cost_ref
      && ctx.input.avg_weekly_sales_qty * ctx.input.sales_weeks === ctx.aggregates.total_sold_qty
      && ctx.input.period_gross_margin === ctx.aggregates.total_gross_margin
      && ctx.aggregates.costed_sales_count === ctx.aggregates.all_sales_count
      && ctx.input.opening_stock_value === ctx.aggregates.total_opening_stock_value
      && ctx.input.closing_stock_value === ctx.aggregates.total_closing_stock_value
      && ctx.aggregates.lots_as_of_by_period_end === ctx.aggregates.all_lots_count
      && ctx.aggregates.costed_lots_count === ctx.aggregates.all_lots_count,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "on_hand_count", targetType: StoreCount, targetIdField: "on_hand_count_id" },
    { link: "cost_record", targetType: UnitCost, targetIdField: "sku_id" },
  ],
  writes: [{ type: StoreCommercialRollup, change: "created" }],
  effect: { kind: "upsert_input_object", type: StoreCommercialRollup, objectIdField: "rollup_id", writeMode: { kind: "create_only" } },
})

/**
 * Record an explicitly withheld store commercial KPI rollup (`unverified` and
 * `missing_cost`): derives zero numerators and denominators so declared WOS and
 * GMROI questions evaluate directly to `null` (withheld) on the read path.
 */
export const RecordWithheldStoreCommercialRollup = Norn.action({
  id: "supply.store_kpi.record_withheld", title: "Record withheld store commercial KPI rollup", version: "0.1.0",
  label: { en: "Record withheld KPI rollup", ar: "تسجيل ملخص مؤشرات محجوب" },
  input: WithheldStoreCommercialRollupInput, output: StoreCommercialRollupRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  derive: [
    { field: "on_hand_qty", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "avg_weekly_sales_qty", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "weeks_of_supply", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "period_gross_margin", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "opening_stock_value", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "closing_stock_value", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "average_inventory_value", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
    { field: "gmroi", inputRequired: false, expression: Norn.expression(WithheldStoreCommercialRollupInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: WithheldStoreCommercialRollupInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.input.count_status === "unverified"
      && ctx.input.cost_status === "missing_cost"
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: StoreCommercialRollup, change: "created" }],
  effect: { kind: "upsert_input_object", type: StoreCommercialRollup, objectIdField: "rollup_id", writeMode: { kind: "create_only" } },
})

// ------------------------------------------------------------------- access

const org = { dimensions: ["org", "environment"] } as const
export const RetailAccess = Norn.access({
  id: "supply.retail.access", title: "Supply lab retail slice", version: "0.1.0",
  rules: [
    { ruleId: "catalog_legal_entity", actions: [RegisterLegalEntity], resourceType: LegalEntity, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "close_legal_entity", actions: [CloseLegalEntity], resourceType: LegalEntity, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "catalog_store", actions: [RegisterStore], resourceType: Store, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "close_store", actions: [CloseStore], resourceType: Store, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "catalog_unit_cost", actions: [RecordUnitCost], resourceType: UnitCost, scope: org, allOf: [{ role: { anyOf: ["data_engineer", "planner"] } }] },
    { ruleId: "stock_lot", actions: [RecordStockLot, RecordUncostedStockLot, UpdateStockLotBalance], resourceType: StockLot, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "store_count", actions: [RecordStoreCount, RecountStore], resourceType: StoreCount, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "pos_sale", actions: [RecordPosSale, RecordUncostedPosSale, PromotePosSaleFromEvidence], resourceType: PosSale, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "store_kpi", actions: [RecordStoreCommercialRollup, RecordWithheldStoreCommercialRollup], resourceType: StoreCommercialRollup, scope: org, allOf: [{ role: { anyOf: ["planner", "warehouse_clerk"] } }] },
  ],
})
