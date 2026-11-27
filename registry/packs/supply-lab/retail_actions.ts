import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Sku } from "./supply.ts"
import { LegalEntity, PosSale, StockLot, Store, StoreCount } from "./retail.ts"

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

// -------------------------------------------------- holding lots & aging

const StockLotInput = Schema.Struct({
  lot_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  owner_entity_id: Schema.String,
  custodian_id: Schema.String,
  received_qty: Norn.decimal(6),
  remaining_qty: Norn.decimal(6),
  unit: Unit,
  received_at: Norn.timestamp("utc"),
  as_of: Norn.timestamp("utc"),
  cost_status: CostStatus,
  unit_cost: Schema.NullOr(Norn.decimal(4)),
  stock_value: Norn.decimal(6),
  currency: Currency,
})
const StockLotRef = Schema.Struct({ lot_id: Schema.String })

/** Open a store holding lot with its receipt date, custody, and cost status. */
export const RecordStockLot = Norn.action({
  id: "supply.stock_lot.record", title: "Record a store stock lot", version: "0.1.0",
  label: { en: "Record stock lot", ar: "تسجيل دفعة مخزون" },
  input: StockLotInput, output: StockLotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "owner_entity_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StockLotInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.slice.owner_entity.status === "active"
      && ctx.input.unit === ctx.slice.sku.unit_of_measure
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "owner_entity_id" },
  ],
  writes: [{ type: StockLot, change: "created" }],
  effect: { kind: "upsert_input_object", type: StockLot, objectIdField: "lot_id", writeMode: { kind: "create_only" } },
})

/** Update a holding lot's remaining balance/valuation while keeping `received_at` and `received_qty` unchanged. */
export const UpdateStockLotBalance = Norn.action({
  id: "supply.stock_lot.update_balance", title: "Update a stock lot balance", version: "0.1.0",
  label: { en: "Update lot balance", ar: "تحديث رصيد الدفعة" },
  input: StockLotInput, output: StockLotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "lot", type: StockLot, key: { property: { from: "input", property: "lot_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StockLotInput,
      slice: Schema.Struct({
        lot: Schema.Struct({
          store_id: Schema.String, sku_id: Schema.String, owner_entity_id: Schema.String,
          received_qty: Norn.decimal(6), unit: Schema.String, currency: Schema.String, received_at: Norn.timestamp("utc"),
        }),
        store: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
      && ctx.slice.lot.store_id === ctx.input.store_id
      && ctx.slice.lot.sku_id === ctx.input.sku_id
      && ctx.slice.lot.owner_entity_id === ctx.input.owner_entity_id
      && ctx.slice.lot.received_qty === ctx.input.received_qty
      && ctx.slice.lot.unit === ctx.input.unit
      && ctx.slice.lot.currency === ctx.input.currency
      && ctx.slice.lot.received_at === ctx.input.received_at,
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
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
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

const PosSaleInput = Schema.Struct({
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
  costed_ref: Schema.NullOr(Schema.String),
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})
const PosSaleRef = Schema.Struct({ sale_id: Schema.String })

/** Record a governed POS sale line; `gross_margin` and `gross_margin_rate` are derived by the kernel. */
export const RecordPosSale = Norn.action({
  id: "supply.pos_sale.record", title: "Record a POS sale line", version: "0.1.0",
  label: { en: "Record POS sale", ar: "تسجيل مبيعات نقطة البيع" },
  input: PosSaleInput, output: PosSaleRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: RetailAccessRef,
  slice: [
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  derive: [
    { field: "gross_margin", inputRequired: false, expression: Norn.expression(PosSaleInput, (x) => x.net_revenue - x.cogs_amount) },
    {
      field: "gross_margin_rate", inputRequired: false,
      expression: Norn.expression(PosSaleInput, (x) => (x.net_revenue > 0 ? (x.net_revenue - x.cogs_amount) / x.net_revenue : 0)),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PosSaleInput,
      slice: Schema.Struct({
        store: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        sku: Schema.Struct({ unit_of_measure: Schema.String }),
      }),
    }),
    (ctx) => ctx.slice.store.status === "active"
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

// ------------------------------------------------------------------- access

const org = { dimensions: ["org", "environment"] } as const
export const RetailAccess = Norn.access({
  id: "supply.retail.access", title: "Supply lab retail slice", version: "0.1.0",
  rules: [
    { ruleId: "catalog_legal_entity", actions: [RegisterLegalEntity], resourceType: LegalEntity, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "close_legal_entity", actions: [CloseLegalEntity], resourceType: LegalEntity, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "catalog_store", actions: [RegisterStore], resourceType: Store, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "close_store", actions: [CloseStore], resourceType: Store, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "stock_lot", actions: [RecordStockLot, UpdateStockLotBalance], resourceType: StockLot, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "store_count", actions: [RecordStoreCount, RecountStore], resourceType: StoreCount, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "pos_sale", actions: [RecordPosSale], resourceType: PosSale, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
  ],
})
