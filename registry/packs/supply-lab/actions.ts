import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import {
  DemandForecast, DemandObservation, Inspection, InventorySnapshot, OrderLine, PurchaseOrder, ShipmentReceipt, Sku,
  StockMovement, Warehouse,
} from "./supply.ts"

// Every supply-lab writer. Each action names its access rule in `supply.access`;
// laws on the written types decide every committed call.

const Unit = Schema.Literals(["each", "kg", "l"])
const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const Access = { artifactId: "supply.access.policy", version: "0.1.0" } as const
const Scope = ["orgId", "environmentId"] as const

// ------------------------------------------------------------------ catalog

const WarehouseInput = Schema.Struct({
  warehouse_id: Schema.String, name: Schema.String,
  region: Schema.Literals(["delta", "canal", "upper_egypt", "red_sea"]),
  latitude: Norn.decimal(6), longitude: Norn.decimal(6),
})
const WarehouseRef = Schema.Struct({ warehouse_id: Schema.String })

export const RegisterWarehouse = Norn.action({
  id: "supply.warehouse.register", title: "Register a warehouse", version: "0.1.0",
  label: { en: "Register warehouse", ar: "تسجيل مستودع" },
  input: WarehouseInput, output: WarehouseRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: Warehouse, change: "created" }],
  effect: { kind: "upsert_input_object", type: Warehouse, objectIdField: "warehouse_id", writeMode: { kind: "create_only" } },
})

export const CloseWarehouse = Norn.action({
  id: "supply.warehouse.close", title: "Close a warehouse", version: "0.1.0",
  label: { en: "Close warehouse", ar: "إغلاق مستودع" },
  input: WarehouseRef, output: WarehouseRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  aggregates: [{
    name: "live_stock",
    type: InventorySnapshot,
    filter: { warehouse_id: "warehouse_id" },
    aggregate: { kind: "sum", property: "on_hand_qty" },
    maxItems: 500,
  }],
  preconditions: [Norn.expression(
    Schema.Struct({ aggregates: Schema.Struct({ live_stock: Norn.decimal(6) }) }),
    (ctx) => ctx.aggregates.live_stock === 0,
    { root: "context" },
  )],
  writes: [{ type: Warehouse, change: "modified" }],
  effect: { kind: "fire_transition", type: Warehouse, objectIdField: "warehouse_id", lifecycle: "status", transition: "close" },
})

const SkuInput = Schema.Struct({
  sku_id: Schema.String, category: Schema.Literals(["dry_goods", "chilled", "pharma", "spares"]),
  unit_of_measure: Unit, shelf_life_days: Schema.Int,
})

export const RegisterSku = Norn.action({
  id: "supply.sku.register", title: "Register a SKU", version: "0.1.0",
  label: { en: "Register SKU", ar: "تسجيل صنف" },
  input: SkuInput, output: Schema.Struct({ sku_id: Schema.String }),
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: Sku, change: "created" }],
  effect: { kind: "upsert_input_object", type: Sku, objectIdField: "sku_id", writeMode: { kind: "create_only" } },
})

// ---------------------------------------------------------- stock positions

const CountInput = Schema.Struct({
  snapshot_id: Schema.String, warehouse_id: Schema.String, sku_id: Schema.String,
  on_hand_qty: Norn.decimal(6), unit: Unit, quantity_basis: Schema.Literals(["on_hand", "available", "reserved"]),
  as_of: Norn.timestamp("utc"), count_source: Schema.Literals(["wms_scan", "cycle_count", "audit"]), count_ref: Schema.String,
})
const SnapshotRef = Schema.Struct({ snapshot_id: Schema.String })
const PositionSlice = Schema.Struct({ warehouse_id: Schema.String, sku_id: Schema.String, unit: Schema.String, quantity_basis: Schema.String })

/** Open a stock position with its first count, in the SKU's own unit, at an active warehouse. */
export const OpenPosition = Norn.action({
  id: "supply.position.open", title: "Open a stock position", version: "0.1.0",
  label: { en: "Open stock position", ar: "فتح رصيد مخزون" },
  input: CountInput, output: SnapshotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "warehouse_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({ input: CountInput, slice: Schema.Struct({ warehouse: Schema.Struct({ status: Schema.String }), sku: Schema.Struct({ unit_of_measure: Schema.String }) }) }),
    (ctx) => ctx.slice.warehouse.status === "active" && ctx.input.unit === ctx.slice.sku.unit_of_measure,
    { root: "context" },
  )],
  createLinks: [
    { link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: InventorySnapshot, change: "created" }],
  effect: { kind: "upsert_input_object", type: InventorySnapshot, objectIdField: "snapshot_id", writeMode: { kind: "create_only" } },
})

/**
 * Record a later scan or cycle count of an open position: a new version of the
 * same position, never a second position, so an as-of question takes the last
 * count and never sums two. The position's warehouse, SKU, unit and basis never change.
 */
export const RecordCount = Norn.action({
  id: "supply.position.record_count", title: "Record a stock count", version: "0.1.0",
  label: { en: "Record stock count", ar: "تسجيل جرد" },
  input: CountInput, output: SnapshotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "position", type: InventorySnapshot, key: { property: { from: "input", property: "snapshot_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({ input: CountInput, slice: Schema.Struct({ position: PositionSlice }) }),
    (ctx) => ctx.input.count_source !== "audit" && ctx.slice.position.warehouse_id === ctx.input.warehouse_id && ctx.slice.position.sku_id === ctx.input.sku_id
      && ctx.slice.position.unit === ctx.input.unit && ctx.slice.position.quantity_basis === ctx.input.quantity_basis,
    { root: "context" },
  )],
  // Update-only: a count never creates a position, so only a `modified` write is declared.
  writes: [{ type: InventorySnapshot, change: "modified" }],
  effect: { kind: "upsert_input_object", type: InventorySnapshot, objectIdField: "snapshot_id", writeMode: { kind: "update_only" } },
})

/**
 * An audit's correction of an earlier count (RESTATE): appended as a newer
 * version at the corrected valid time, so the earlier knowledge-time answer stays
 * reproducible and the restated one is explicit.
 */
export const RestateCount = Norn.action({
  id: "supply.position.restate", title: "Restate a stock count after audit", version: "0.1.0",
  label: { en: "Restate count", ar: "تصحيح الجرد" },
  input: CountInput, output: SnapshotRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "position", type: InventorySnapshot, key: { property: { from: "input", property: "snapshot_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({ input: CountInput, slice: Schema.Struct({ position: PositionSlice }) }),
    (ctx) => ctx.input.count_source === "audit" && ctx.slice.position.warehouse_id === ctx.input.warehouse_id && ctx.slice.position.sku_id === ctx.input.sku_id
      && ctx.slice.position.unit === ctx.input.unit && ctx.slice.position.quantity_basis === ctx.input.quantity_basis,
    { root: "context" },
  )],
  // Update-only: a count never creates a position, so only a `modified` write is declared.
  writes: [{ type: InventorySnapshot, change: "modified" }],
  effect: { kind: "upsert_input_object", type: InventorySnapshot, objectIdField: "snapshot_id", writeMode: { kind: "update_only" } },
})

const MovementInput = Schema.Struct({
  movement_id: Schema.String, warehouse_id: Schema.String, sku_id: Schema.String,
  qty: Norn.decimal(6), unit: Unit, movement_kind: Schema.Literals(["receipt", "issue", "adjustment", "transfer"]),
  occurred_at: Norn.timestamp("utc"),
})

/** A movement is posted once, in its SKU's unit: no cross-unit mixing. */
export const RecordMovement = Norn.action({
  id: "supply.movement.record", title: "Record a stock movement", version: "0.1.0",
  label: { en: "Record movement", ar: "تسجيل حركة" },
  input: MovementInput, output: Schema.Struct({ movement_id: Schema.String }),
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "warehouse_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({ input: MovementInput, slice: Schema.Struct({ warehouse: Schema.Struct({ status: Schema.String }), sku: Schema.Struct({ unit_of_measure: Schema.String }) }) }),
    (ctx) => ctx.slice.warehouse.status === "active" && ctx.input.unit === ctx.slice.sku.unit_of_measure,
    { root: "context" },
  )],
  createLinks: [
    { link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: StockMovement, change: "created" }],
  effect: { kind: "upsert_input_object", type: StockMovement, objectIdField: "movement_id", writeMode: { kind: "create_only" } },
})

// ---------------------------------------------------------------- purchasing

const OrderInput = Schema.Struct({
  po_id: Schema.String, supplier_id: Schema.String, promised_at: Norn.timestamp("utc"), currency: Currency, created_by: Schema.String,
})
const OrderRef = Schema.Struct({ po_id: Schema.String })

export const CreateOrder = Norn.action({
  id: "supply.po.create", title: "Draft a purchase order", version: "0.1.0",
  label: { en: "Draft purchase order", ar: "إنشاء مسودة أمر شراء" },
  input: OrderInput, output: OrderRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  // The server stamps the drafter; a caller never supplies it.
  fromCaller: ["created_by"],
  writes: [{ type: PurchaseOrder, change: "created" }],
  effect: { kind: "upsert_input_object", type: PurchaseOrder, objectIdField: "po_id", writeMode: { kind: "create_only" } },
})

/** Separation of duties: the principal who drafted an order never submits it. */
export const SubmitOrder = Norn.action({
  id: "supply.po.submit", title: "Submit a purchase order", version: "0.1.0",
  label: { en: "Submit purchase order", ar: "إرسال أمر الشراء" },
  input: OrderRef, output: OrderRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PurchaseOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "po_id", lifecycle: "status", transition: "submit" },
  independentOf: ["created_by"],
})

export const ReceivePartial = Norn.action({
  id: "supply.po.receive_partial", title: "Mark a purchase order partially received", version: "0.1.0",
  label: { en: "Partially received", ar: "استلام جزئي" },
  input: OrderRef, output: OrderRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PurchaseOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "po_id", lifecycle: "status", transition: "receive_partial" },
})

export const FulfilOrder = Norn.action({
  id: "supply.po.fulfil", title: "Fulfil a purchase order", version: "0.1.0",
  label: { en: "Fulfil purchase order", ar: "إكمال أمر الشراء" },
  input: OrderRef, output: OrderRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PurchaseOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "po_id", lifecycle: "status", transition: "fulfil" },
})

export const CancelOrder = Norn.action({
  id: "supply.po.cancel", title: "Cancel a purchase order", version: "0.1.0",
  label: { en: "Cancel purchase order", ar: "إلغاء أمر الشراء" },
  input: OrderRef, output: OrderRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PurchaseOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "po_id", lifecycle: "status", transition: "cancel" },
})

const LineInput = Schema.Struct({
  line_id: Schema.String, po_id: Schema.String, sku_id: Schema.String, promised_at: Norn.timestamp("utc"),
  ordered_qty: Norn.decimal(6), received_qty: Norn.decimal(6), fill_rate: Norn.decimal(6),
  unit: Unit, unit_price: Norn.decimal(4), currency: Currency,
})
const LineRef = Schema.Struct({ line_id: Schema.String })

/** A line is added to a draft order only, in its SKU's unit and its order's currency. */
export const AddLine = Norn.action({
  id: "supply.line.add", title: "Add a purchase order line", version: "0.1.0",
  label: { en: "Add order line", ar: "إضافة بند" },
  input: LineInput, output: LineRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({ input: LineInput, slice: Schema.Struct({ order: Schema.Struct({ status: Schema.String, currency: Schema.String }), sku: Schema.Struct({ unit_of_measure: Schema.String }) }) }),
    (ctx) => ctx.slice.order.status === "draft" && ctx.input.currency === ctx.slice.order.currency
      && ctx.input.unit === ctx.slice.sku.unit_of_measure && ctx.input.received_qty === 0 && ctx.input.fill_rate === 0,
    { root: "context" },
  )],
  createLinks: [
    { link: "order", targetType: PurchaseOrder, targetIdField: "po_id" },
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: OrderLine, change: "created" }],
  effect: { kind: "upsert_input_object", type: OrderLine, objectIdField: "line_id", writeMode: { kind: "create_only" } },
})

/**
 * Record a line's cumulative accepted quantity (the ERP's Purchase Order Item
 * `received_qty`) while its order is open; the stored row ratio is derived, never
 * typed by the caller.
 */
export const RecordLineReceived = Norn.action({
  id: "supply.line.record_received", title: "Record a line's received quantity", version: "0.1.0",
  label: { en: "Record received quantity", ar: "تسجيل الكمية المستلمة" },
  input: LineInput, output: LineRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
  ],
  derive: [{ field: "fill_rate", inputRequired: false, expression: Norn.expression(LineInput, (x) => x.received_qty / x.ordered_qty) }],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: LineInput,
      slice: Schema.Struct({
        line: Schema.Struct({ po_id: Schema.String, sku_id: Schema.String, unit: Schema.String, ordered_qty: Norn.decimal(6) }),
        order: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) => (ctx.slice.order.status === "submitted" || ctx.slice.order.status === "partially_received")
      && ctx.slice.line.po_id === ctx.input.po_id && ctx.slice.line.sku_id === ctx.input.sku_id
      && ctx.slice.line.unit === ctx.input.unit && ctx.slice.line.ordered_qty === ctx.input.ordered_qty,
    { root: "context" },
  )],
  // Update-only: a receipt never creates a line, so only a `modified` write is declared.
  writes: [{ type: OrderLine, change: "modified" }],
  effect: { kind: "upsert_input_object", type: OrderLine, objectIdField: "line_id", writeMode: { kind: "update_only" } },
})

const ReceiptInput = Schema.Struct({
  receipt_id: Schema.String, supplier_id: Schema.String, warehouse_id: Schema.String, line_id: Schema.String,
  received_qty: Norn.decimal(6), damaged_qty: Norn.decimal(6), unit: Unit, received_at: Norn.timestamp("utc"),
})
const BlindReceiptInput = Schema.Struct({
  receipt_id: Schema.String, supplier_id: Schema.String, warehouse_id: Schema.String, line_id: Schema.NullOr(Schema.String),
  received_qty: Norn.decimal(6), damaged_qty: Norn.decimal(6), unit: Unit, received_at: Norn.timestamp("utc"),
})
const ReceiptRef = Schema.Struct({ receipt_id: Schema.String })

/** An inspected delivery against a line of an active order, in the line's unit, at an active warehouse. */
export const RecordReceipt = Norn.action({
  id: "supply.receipt.record", title: "Record a shipment receipt", version: "0.1.0",
  label: { en: "Record receipt", ar: "تسجيل استلام" },
  input: ReceiptInput, output: ReceiptRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "line", property: "po_id" } } },
    { name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "warehouse_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: ReceiptInput,
      slice: Schema.Struct({
        line: Schema.Struct({ unit: Schema.String }),
        order: Schema.Struct({ status: Schema.String }),
        warehouse: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) => (ctx.slice.order.status === "submitted" || ctx.slice.order.status === "partially_received")
      && ctx.slice.warehouse.status === "active"
      && ctx.input.unit === ctx.slice.line.unit,
    { root: "context" },
  )],
  createLinks: [
    { link: "order_line", targetType: OrderLine, targetIdField: "line_id" },
    { link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" },
  ],
  writes: [{ type: ShipmentReceipt, change: "created" }],
  effect: { kind: "upsert_input_object", type: ShipmentReceipt, objectIdField: "receipt_id", writeMode: { kind: "create_only" } },
})

/** A blind receipt: goods arrived with no matching order line. It stays unlinked evidence. */
export const RecordBlindReceipt = Norn.action({
  id: "supply.receipt.record_blind", title: "Record a blind receipt", version: "0.1.0",
  label: { en: "Record blind receipt", ar: "تسجيل استلام بدون أمر" },
  input: BlindReceiptInput, output: ReceiptRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "warehouse_id" } } }],
  preconditions: [Norn.expression(BlindReceiptInput, (input) => input.line_id === null)],
  createLinks: [{ link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" }],
  writes: [{ type: ShipmentReceipt, change: "created" }],
  effect: { kind: "upsert_input_object", type: ShipmentReceipt, objectIdField: "receipt_id", writeMode: { kind: "create_only" } },
})

const ReceiptTime = Schema.Struct({ receipt_id: Schema.String, received_at: Norn.timestamp("utc") })

/**
 * Correct when a delivery was received (an audit found the dock log wrong):
 * an update, never a new receipt, so its inspections move with it to the
 * corrected week (METRIC-GRAIN-LINK-1).
 */
export const CorrectReceiptTime = Norn.action({
  id: "supply.receipt.correct_time", title: "Correct a receipt's received time", version: "0.1.0",
  label: { en: "Correct receipt time", ar: "تصحيح وقت الاستلام" },
  input: ReceiptTime, output: ReceiptRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "receipt", type: ShipmentReceipt, key: { property: { from: "input", property: "receipt_id" } } }],
  writes: [{ type: ShipmentReceipt, change: "modified" }],
  effect: { kind: "upsert_input_object", type: ShipmentReceipt, objectIdField: "receipt_id", writeMode: { kind: "update_only" } },
})

const InspectionInput = Schema.Struct({
  inspection_id: Schema.String, receipt_id: Schema.String,
  inspected_qty: Norn.decimal(6), rejected_qty: Norn.decimal(6), unit: Unit,
})
const OrphanInspectionInput = Schema.Struct({
  inspection_id: Schema.String, receipt_id: Schema.NullOr(Schema.String),
  inspected_qty: Norn.decimal(6), rejected_qty: Norn.decimal(6), unit: Unit,
})
const InspectionRef = Schema.Struct({ inspection_id: Schema.String })

/** Inspect a recorded receipt, in the receipt's unit. */
export const RecordInspection = Norn.action({
  id: "supply.inspection.record", title: "Record a receipt inspection", version: "0.1.0",
  label: { en: "Record inspection", ar: "تسجيل فحص" },
  input: InspectionInput, output: InspectionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "receipt", type: ShipmentReceipt, key: { property: { from: "input", property: "receipt_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({ input: InspectionInput, slice: Schema.Struct({ receipt: Schema.Struct({ unit: Schema.String }) }) }),
    (ctx) => ctx.input.unit === ctx.slice.receipt.unit,
    { root: "context" },
  )],
  createLinks: [{ link: "receipt", targetType: ShipmentReceipt, targetIdField: "receipt_id" }],
  writes: [{ type: Inspection, change: "created" }],
  effect: { kind: "upsert_input_object", type: Inspection, objectIdField: "inspection_id", writeMode: { kind: "create_only" } },
})

/** An inspection whose receipt is not yet known: evidence, counted as unlinked. */
export const RecordOrphanInspection = Norn.action({
  id: "supply.inspection.record_orphan", title: "Record an inspection without its receipt", version: "0.1.0",
  label: { en: "Record unmatched inspection", ar: "تسجيل فحص غير مطابق" },
  input: OrphanInspectionInput, output: InspectionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  preconditions: [Norn.expression(OrphanInspectionInput, (input) => input.receipt_id === null)],
  writes: [{ type: Inspection, change: "created" }],
  effect: { kind: "upsert_input_object", type: Inspection, objectIdField: "inspection_id", writeMode: { kind: "create_only" } },
})

// ------------------------------------------------------------- forecasting

const ObservationInput = Schema.Struct({
  observation_id: Schema.String, sku_id: Schema.String, warehouse_id: Schema.String, week_start: Norn.timestamp("utc"),
  week_index: Schema.Int, issued_qty: Norn.decimal(6), unit: Unit, lead_time_days: Schema.Int, promotion: Schema.Boolean,
})

export const RecordObservation = Norn.action({
  id: "supply.observation.record", title: "Record a weekly demand observation", version: "0.1.0",
  label: { en: "Record weekly demand", ar: "تسجيل الطلب الأسبوعي" },
  input: ObservationInput, output: Schema.Struct({ observation_id: Schema.String }),
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "warehouse_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({ input: ObservationInput, slice: Schema.Struct({ sku: Schema.Struct({ unit_of_measure: Schema.String }) }) }),
    (ctx) => ctx.input.unit === ctx.slice.sku.unit_of_measure,
    { root: "context" },
  )],
  createLinks: [
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" },
  ],
  writes: [{ type: DemandObservation, change: "created" }],
  effect: { kind: "upsert_input_object", type: DemandObservation, objectIdField: "observation_id", writeMode: { kind: "create_only" } },
})

const ForecastInput = Schema.Struct({
  forecast_id: Schema.String, subject: Schema.String, model_version: Schema.String, sku_id: Schema.String,
  warehouse_id: Schema.String, week_start: Norn.timestamp("utc"),
  // A registry value type (`core.quantity`, decimal(6)) types the input and its law alike.
  predicted_qty: Norn.ref("core.quantity"),
  stockout_risk: Schema.Literals(["low", "medium", "high"]),
})

/** A forecast row is written only from a production model version: never a candidate or retired one. */
export const RecordForecast = Norn.action({
  id: "supply.forecast.record", title: "Record a demand forecast", version: "0.1.0",
  label: { en: "Record forecast", ar: "تسجيل التوقع" },
  input: ForecastInput, output: Schema.Struct({ forecast_id: Schema.String }),
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "version", type: "ml.ModelVersion", key: { property: { from: "input", property: "model_version" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "warehouse", type: Warehouse, key: { property: { from: "input", property: "warehouse_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({ slice: Schema.Struct({ version: Schema.Struct({ status: Schema.String }) }) }),
    (ctx) => ctx.slice.version.status === "production",
    { root: "context" },
  )],
  createLinks: [
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" },
    { link: "scored_by", targetType: "ml.ModelVersion", targetIdField: "model_version" },
  ],
  writes: [{ type: DemandForecast, change: "created" }],
  effect: { kind: "upsert_input_object", type: DemandForecast, objectIdField: "forecast_id", writeMode: { kind: "create_only" } },
})

// ------------------------------------------------------------------- access

// Who may call each writer on a hosted cell, in the org and environment:
// planners draft orders and lines; warehouse clerks count, move and receive;
// data engineers own the catalog and the forecasting rows; operations
// managers submit, cancel, close and restate. Submit is also SoD-checked
// against the drafter (`independentOf`).
const org = { dimensions: ["org", "environment"] } as const
export const SupplyAccess = Norn.access({
  id: "supply.access", title: "Supply lab", version: "0.1.0",
  rules: [
    { ruleId: "catalog_warehouse", actions: [RegisterWarehouse], resourceType: Warehouse, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "catalog_sku", actions: [RegisterSku], resourceType: Sku, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "close_warehouse", actions: [CloseWarehouse], resourceType: Warehouse, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "count", actions: [OpenPosition, RecordCount], resourceType: InventorySnapshot, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "restate", actions: [RestateCount], resourceType: InventorySnapshot, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "move", actions: [RecordMovement], resourceType: StockMovement, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "draft_order", actions: [CreateOrder], resourceType: PurchaseOrder, scope: org, allOf: [{ role: { anyOf: ["planner"] } }] },
    { ruleId: "decide_order", actions: [SubmitOrder, CancelOrder], resourceType: PurchaseOrder, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "receive_order", actions: [ReceivePartial, FulfilOrder], resourceType: PurchaseOrder, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "draft_line", actions: [AddLine], resourceType: OrderLine, scope: org, allOf: [{ role: { anyOf: ["planner"] } }] },
    { ruleId: "receive_line", actions: [RecordLineReceived], resourceType: OrderLine, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "receive", actions: [RecordReceipt, RecordBlindReceipt], resourceType: ShipmentReceipt, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "correct_receipt", actions: [CorrectReceiptTime], resourceType: ShipmentReceipt, scope: org, allOf: [{ role: { anyOf: ["ops_manager"] } }] },
    { ruleId: "inspect", actions: [RecordInspection, RecordOrphanInspection], resourceType: Inspection, scope: org, allOf: [{ role: { anyOf: ["warehouse_clerk"] } }] },
    { ruleId: "observe_demand", actions: [RecordObservation], resourceType: DemandObservation, scope: org, allOf: [{ role: { anyOf: ["data_engineer"] } }] },
    { ruleId: "forecast", actions: [RecordForecast], resourceType: DemandForecast, scope: org, allOf: [{ role: { anyOf: ["data_engineer", "planner"] } }] },
  ],
})
