import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Supply Lab (Phase 1): a synthetic supply-chain and demand-forecasting pack
// that stresses Norn's data plane. Catalog items (Sku) are never stock; a
// stock position (InventorySnapshot) is a semi-additive balance versioned by
// the instant it was counted; movements, ordered and received quantities are
// additive flows. Every quantity is held in `unit` (METRIC-UNIT-PARTITION-1),
// and a position also in its `quantity_basis`, since on-hand, available and
// reserved quantities never add. Forecasting composes the `ml` and `tracking`
// packs: a forecast row names an `ml.ModelVersion`, whose promotion law
// (`ml.promotion_policy`) this pack reuses rather than restates.

const Unit = Schema.Literals(["each", "kg", "l"])
// Every quantity is held in one of the catalog's units: a native call with
// another unit is refused at its input, and a feed row with one is quarantined
// by its mapping (MAP-ENUM-TEXT-1), so an unknown unit is never summed.
const UnitLabels = { each: { en: "Each", ar: "قطعة" }, kg: { en: "Kilogram", ar: "كيلوغرام" }, l: { en: "Litre", ar: "لتر" } }

// ------------------------------------------------------------------ catalog

const WarehouseProps = Schema.Struct({
  warehouse_id: Schema.String,
  name: Schema.String,
  region: Schema.Literals(["delta", "canal", "upper_egypt", "red_sea"]),
  latitude: Norn.decimal(6),
  longitude: Norn.decimal(6),
  status: Schema.Literals(["active", "closed"]),
})

export const Warehouse = Norn.type({
  name: "supply.Warehouse", title: "Warehouse", version: "0.1.0",
  label: { en: "Warehouse", ar: "مستودع" },
  properties: WarehouseProps,
  propertyLabels: { warehouse_id: { en: "Warehouse code", ar: "رمز المستودع" }, region: { en: "Region", ar: "المنطقة" } },
  valueLabels: { status: { active: { en: "Active", ar: "نشط" }, closed: { en: "Closed", ar: "مغلق" } } },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["warehouse_id"] }],
  defaults: { status: "active" },
  lifecycles: [{ property: "status", initial: ["active"], terminal: ["closed"], immutable: ["closed"], endings: { active: "resting", closed: "completed" }, transitions: [
    { id: "close", from: ["active"], to: "closed" },
  ] }],
})

/** A catalog item: what a thing is, never how much of it is held anywhere. */
export const Sku = Norn.type({
  name: "supply.Sku", title: "Stock-keeping unit", version: "0.1.0",
  label: { en: "SKU", ar: "صنف" },
  properties: Schema.Struct({
    sku_id: Schema.String,
    category: Schema.Literals(["dry_goods", "chilled", "pharma", "spares"]),
    unit_of_measure: Unit,
    shelf_life_days: Schema.Int,
  }),
  valueLabels: { unit_of_measure: UnitLabels },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["sku_id"] }],
})

// ---------------------------------------------------------- stock positions

// What every count carries, native or imported. Its law is the one both a
// native count (refused) and an imported WMS scan (quarantined) answer to.
const CountFacts = Schema.Struct({
  // A stock: summable across positions at one instant, never over time.
  on_hand_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit", "quantity_basis"] }),
  unit: Unit,
  quantity_basis: Schema.Literals(["on_hand", "available", "reserved"]),
  // When the count applies (valid time); METRIC-SNAPSHOT-ASOF-1 orders a position's versions by it.
  as_of: Norn.timestamp("utc"),
})

export const CountEvidence = Norn.interfaceType({
  name: "supply.CountEvidence", title: "Stock count evidence", version: "0.1.0",
  label: { en: "Stock count", ar: "جرد المخزون" },
  properties: CountFacts,
})

export const OnHandNonNegative = Norn.law("on_hand_non_negative", {
  on: CountEvidence,
  must: (self) => self.on_hand_qty >= 0,
  because: "A counted stock position is never negative; a shortfall is a movement or an adjustment, not a negative count.",
})
export const OnHandZeroHolds = Norn.example("an empty position holds", {
  of: OnHandNonNegative,
  given: { on_hand_qty: 0, unit: "each", quantity_basis: "on_hand", as_of: "2026-07-06T08:00:00Z" },
  expect: "holds",
})
export const OnHandNegativeViolated = Norn.example("a negative count is refused", {
  of: OnHandNonNegative,
  given: { on_hand_qty: -50, unit: "each", quantity_basis: "on_hand", as_of: "2026-07-06T08:00:00Z" },
  expect: "violated",
})

const SnapshotProps = Schema.Struct({
  snapshot_id: Schema.String, // "<warehouse>/<sku>/<basis>": one position, versioned by count
  warehouse_id: Schema.String,
  sku_id: Schema.String,
  on_hand_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit", "quantity_basis"] }),
  unit: Unit,
  quantity_basis: Schema.Literals(["on_hand", "available", "reserved"]),
  as_of: Norn.timestamp("utc"),
  count_source: Schema.Literals(["wms_scan", "cycle_count", "audit"]),
  count_ref: Schema.String,
})

export const InventorySnapshot = Norn.type({
  name: "supply.InventorySnapshot", title: "Inventory position", version: "0.1.0",
  label: { en: "Inventory position", ar: "رصيد المخزون" },
  implements: [CountEvidence],
  properties: SnapshotProps,
  valueLabels: {
    quantity_basis: { on_hand: { en: "On hand", ar: "متوفر فعليًا" }, available: { en: "Available", ar: "متاح" }, reserved: { en: "Reserved", ar: "محجوز" } },
  },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["snapshot_id"] }],
  links: [
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true }, inverse: "positions" },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
  ],
})

// LAW-LINK-1: a position's unit always matches its catalog SKU's unit of measure.
export const SnapshotMatchesSkuUnit = Norn.law("matches_sku_unit", {
  id: "supply.position.matches_sku_unit",
  on: InventorySnapshot,
  must: (self) => self.unit === self.sku.unit_of_measure,
  because: "A stock position must be counted in its catalog SKU's unit of measure.",
})

// LAW-LINK-MANY-1: a warehouse never closes while it still holds live stock.
export const WarehouseClosedRequiresNoStock = Norn.law("closed_requires_no_stock", {
  on: Warehouse,
  inverses: { positions: InventorySnapshot },
  when: (self) => self.status === "closed",
  must: (self) => Norn.sum(self.positions.map((p) => p.on_hand_qty)) === 0,
  because: "A warehouse cannot close while live inventory positions still hold non-zero stock there.",
})

/**
 * One WMS scan row as the scan feed sent it: evidence, never a position. A
 * governed count (`supply.position.record_count`) promotes it; a scan that
 * violates the count law is kept and quarantined, never dropped.
 */
export const WmsCount = Norn.type({
  name: "supply.WmsCount", title: "WMS scan row", version: "0.1.0",
  label: { en: "WMS scan", ar: "مسح نظام المستودع" },
  implements: [CountEvidence],
  properties: Schema.Struct({
    scan_id: Schema.String,
    warehouse_code: Schema.String,
    sku_code: Schema.String,
    on_hand_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit", "quantity_basis"] }),
    unit: Unit,
    quantity_basis: Schema.Literals(["on_hand", "available", "reserved"]),
    as_of: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["scan_id"] }],
})

const MovementProps = Schema.Struct({
  movement_id: Schema.String,
  warehouse_id: Schema.String,
  sku_id: Schema.String,
  // The same quantity concept as a position, declared additive at this use site (METRIC-ADDITIVITY-REF-1).
  qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  unit: Unit,
  movement_kind: Schema.Literals(["receipt", "issue", "adjustment", "transfer"]),
  occurred_at: Norn.timestamp("utc"),
})

export const StockMovement = Norn.type({
  name: "supply.StockMovement", title: "Stock movement", version: "0.1.0",
  label: { en: "Stock movement", ar: "حركة مخزون" },
  properties: MovementProps,
  valueLabels: { movement_kind: { receipt: { en: "Receipt", ar: "استلام" }, issue: { en: "Issue", ar: "صرف" }, adjustment: { en: "Adjustment", ar: "تسوية" }, transfer: { en: "Transfer", ar: "تحويل" } } },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["movement_id"] }],
  links: [
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
  ],
})

// LAW-LINK-1: a stock movement uses its SKU's unit of measure.
export const MovementMatchesSkuUnit = Norn.law("matches_sku_unit", {
  on: StockMovement,
  must: (self) => self.unit === self.sku.unit_of_measure,
  because: "A stock movement must record quantities in its catalog SKU's unit of measure.",
})

export const MovementPositive = Norn.law("movement_positive", {
  on: StockMovement,
  must: (self) => self.movement_kind === "adjustment" || self.qty > 0,
  because: "A receipt, issue or transfer moves a positive quantity; only an adjustment may carry a sign.",
})
export const MovementIssueHolds = Norn.example("a positive issue holds", {
  of: MovementPositive,
  given: { movement_id: "MV-1", warehouse_id: "WH-A", sku_id: "SKU-1", qty: 12, unit: "each", movement_kind: "issue", occurred_at: "2026-07-07T10:00:00Z" },
  expect: "holds",
})
export const MovementNegativeIssueViolated = Norn.example("a negative issue is refused", {
  of: MovementPositive,
  given: { movement_id: "MV-2", warehouse_id: "WH-A", sku_id: "SKU-1", qty: -50, unit: "each", movement_kind: "issue", occurred_at: "2026-07-07T10:00:00Z" },
  expect: "violated",
})
export const MovementNegativeAdjustmentHolds = Norn.example("a negative adjustment holds", {
  of: MovementPositive,
  given: { movement_id: "MV-3", warehouse_id: "WH-A", sku_id: "SKU-1", qty: -4, unit: "each", movement_kind: "adjustment", occurred_at: "2026-07-07T10:00:00Z" },
  expect: "holds",
})

// ---------------------------------------------------------------- purchasing

const OrderStatus = Schema.Literals(["draft", "submitted", "partially_received", "fulfilled", "cancelled"])

/**
 * Shared order-commitment lifecycle contract (`INTERFACE-LIFECYCLE-1` + `INTERFACE-DISJOINT-1`):
 * an order commitment is never a stock snapshot (`CountEvidence`) or a delivery receipt (`ReceiptEvidence`).
 */
export const OrderLifecycle = Norn.interfaceType({
  name: "supply.OrderLifecycle", title: "Order commitment lifecycle", version: "0.1.0",
  label: { en: "Order lifecycle", ar: "دورة حياة الطلب" },
  needsReview: ["ar"],
  disjointWith: [CountEvidence],
  properties: Schema.Struct({ status: OrderStatus }),
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["fulfilled", "cancelled"], immutable: ["fulfilled", "cancelled"], endings: { fulfilled: "completed", cancelled: "cancelled" }, transitions: [
    { id: "submit", from: ["draft"], to: "submitted" },
    { id: "receive_partial", from: ["submitted"], to: "partially_received" },
    { id: "fulfil", from: ["submitted", "partially_received"], to: "fulfilled" },
    { id: "cancel", from: ["draft", "submitted"], to: "cancelled" },
  ] }],
})

const PurchaseOrderProps = Schema.Struct({
  po_id: Schema.String,
  supplier_id: Schema.String, // the canonical supplier code (ERP vendor codes map onto it)
  promised_at: Norn.timestamp("utc"),
  currency: Schema.Literals(["EGP", "USD", "EUR"]),
  created_by: Schema.String,
  status: OrderStatus,
})

export const PurchaseOrder = Norn.type({
  name: "supply.PurchaseOrder", title: "Purchase order", version: "0.1.0",
  label: { en: "Purchase order", ar: "أمر شراء" },
  implements: [OrderLifecycle],
  properties: PurchaseOrderProps,
  valueLabels: { status: {
    draft: { en: "Draft", ar: "مسودة" }, submitted: { en: "Submitted", ar: "مُرسل" },
    partially_received: { en: "Partially received", ar: "مستلم جزئيًا" }, fulfilled: { en: "Fulfilled", ar: "مكتمل" }, cancelled: { en: "Cancelled", ar: "ملغى" },
  } },
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["po_id"] }],
  // The drafter is the verified principal that created the order; nobody may rewrite it.
  protected: ["created_by"],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["fulfilled", "cancelled"], immutable: ["fulfilled", "cancelled"], endings: { fulfilled: "completed", cancelled: "cancelled" }, transitions: [
    { id: "submit", from: ["draft"], to: "submitted" },
    { id: "receive_partial", from: ["submitted"], to: "partially_received" },
    { id: "fulfil", from: ["submitted", "partially_received"], to: "fulfilled" },
    { id: "cancel", from: ["draft", "submitted"], to: "cancelled" },
  ] }],
})

const OrderLineProps = Schema.Struct({
  line_id: Schema.String,
  po_id: Schema.String,
  sku_id: Schema.String,
  // The line's own promised date (ERP schedule date); fill rate and linked receipts bucket by it.
  promised_at: Norn.timestamp("utc"),
  ordered_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  // Cumulative accepted receipts against the line, as the ERP keeps it (Purchase Order Item `received_qty`).
  received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  // Supplier fill rate is a ratio of sums, never an average of line ratios.
  fill_rate: Norn.decimal(6, { additivity: { kind: "ratio", numerator: "received_qty", denominator: "ordered_qty" } }),
  unit: Unit,
  unit_price: Norn.decimal(4, { currency: "currency", additivity: { kind: "non_additive" } }),
  currency: Schema.Literals(["EGP", "USD", "EUR"]),
})

export const OrderLine = Norn.type({
  name: "supply.OrderLine", title: "Purchase order line", version: "0.1.0",
  label: { en: "Order line", ar: "بند أمر الشراء" },
  properties: OrderLineProps,
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["line_id"] }],
  links: [
    { id: "order", target: PurchaseOrder, cardinality: { to: "one", from: "many", required: true }, inverse: "lines" },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
  ],
})

// LAW-LINK-1: an order line keeps its order's currency and its SKU's unit.
export const OrderLineMatchesOrderAndSku = Norn.law("matches_order_and_sku", {
  on: OrderLine,
  must: (self) => self.currency === self.order.currency && self.unit === self.sku.unit_of_measure,
  because: "An order line must use its purchase order's currency and its SKU's unit of measure.",
})

export const OrderedPositive = Norn.law("ordered_positive", {
  on: OrderLine,
  must: (self) => self.ordered_qty > 0 && self.received_qty >= 0,
  because: "A line orders a positive quantity and never records a negative cumulative receipt.",
})
export const OrderedPositiveHolds = Norn.example("a positive line holds", {
  of: OrderedPositive,
  given: { line_id: "PO-1/1", po_id: "PO-1", sku_id: "SKU-1", promised_at: "2026-07-10T00:00:00Z", ordered_qty: 100, received_qty: 0, fill_rate: 0, unit: "each", unit_price: 2.5, currency: "EGP" },
  expect: "holds",
})
export const OrderedZeroViolated = Norn.example("a zero-quantity line is refused", {
  of: OrderedPositive,
  given: { line_id: "PO-1/2", po_id: "PO-1", sku_id: "SKU-1", promised_at: "2026-07-10T00:00:00Z", ordered_qty: 0, received_qty: 0, fill_rate: 0, unit: "each", unit_price: 2.5, currency: "EGP" },
  expect: "violated",
})

const ReceiptFacts = Schema.Struct({
  received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  damaged_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  unit: Unit,
  received_at: Norn.timestamp("utc"),
})

/** What every receipt carries, native or imported from the supplier feed. */
export const ReceiptEvidence = Norn.interfaceType({
  name: "supply.ReceiptEvidence", title: "Receipt evidence", version: "0.1.0",
  label: { en: "Receipt", ar: "استلام" },
  disjointWith: [CountEvidence, OrderLifecycle],
  properties: ReceiptFacts,
})

export const DamagedWithinReceived = Norn.law("damaged_within_received", {
  on: ReceiptEvidence,
  must: (self) => self.received_qty >= 0 && self.damaged_qty >= 0 && self.damaged_qty <= self.received_qty,
  because: "A receipt never records a negative quantity, and never more damaged units than it received.",
})
export const DamagedWithinHolds = Norn.example("a partly damaged receipt holds", {
  of: DamagedWithinReceived,
  given: { received_qty: 60, damaged_qty: 5, unit: "each", received_at: "2026-07-09T09:00:00Z" },
  expect: "holds",
})
export const OverDamagedViolated = Norn.example("more damaged than received is refused", {
  of: DamagedWithinReceived,
  given: { received_qty: 10, damaged_qty: 12, unit: "each", received_at: "2026-07-09T09:00:00Z" },
  expect: "violated",
})
export const NegativeReceiptViolated = Norn.example("a negative receipt is refused", {
  of: DamagedWithinReceived,
  given: { received_qty: -50, damaged_qty: 0, unit: "each", received_at: "2026-07-09T09:00:00Z" },
  expect: "violated",
})

/** One inspected delivery. The link to its order line is optional: a blind receipt is evidence too. */
export const ShipmentReceipt = Norn.type({
  name: "supply.ShipmentReceipt", title: "Shipment receipt", version: "0.1.0",
  label: { en: "Shipment receipt", ar: "إيصال استلام شحنة" },
  implements: [ReceiptEvidence],
  properties: Schema.Struct({
    receipt_id: Schema.String,
    supplier_id: Schema.String,
    warehouse_id: Schema.String,
    line_id: Schema.NullOr(Schema.String),
    received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    damaged_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    unit: Unit,
    received_at: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["receipt_id"] }],
  links: [
    { id: "order_line", target: OrderLine, cardinality: { to: "one", from: "many" }, inverse: "receipts" },
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})

// LAW-LINK-1: a linked shipment receipt uses its order line's unit.
export const ReceiptMatchesOrderLineUnit = Norn.law("matches_order_line_unit", {
  on: ShipmentReceipt,
  when: (self): self is typeof self & { readonly order_line: NonNullable<typeof self.order_line> } => self.order_line !== null,
  must: (self) => self.unit === self.order_line.unit,
  because: "A receipt linked to an order line must use that line's unit of measure.",
})

// LAW-LINK-MANY-1: linked receipts against a line never exceed its ordered quantity,
// and cumulative reconciled received_qty never exceeds the linked physical receipts.
export const OrderLineReceiptsConsistent = Norn.law("receipts_consistent", {
  on: OrderLine,
  inverses: { receipts: ShipmentReceipt },
  when: (self) => self.receipts.length > 0,
  must: (self) =>
    Norn.sum(self.receipts.map((r) => r.received_qty)) <= self.ordered_qty
    && self.received_qty <= Norn.sum(self.receipts.map((r) => r.received_qty)),
  because: "Total linked receipt quantities on a line never exceed ordered_qty, and recorded received_qty never exceeds its linked receipts.",
})

/**
 * One quality inspection of a receipt: a receipt may be inspected several
 * times. An inspection has no time of its own; it is bucketed by its
 * receipt's `received_at` through the optional `receipt` link
 * (METRIC-GRAIN-LINK-1), and one with no receipt is counted as unlinked.
 */
export const Inspection = Norn.type({
  name: "supply.Inspection", title: "Receipt inspection", version: "0.1.0",
  label: { en: "Inspection", ar: "فحص الاستلام" },
  properties: Schema.Struct({
    inspection_id: Schema.String,
    receipt_id: Schema.NullOr(Schema.String),
    inspected_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    rejected_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    unit: Unit,
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["inspection_id"] }],
  links: [{ id: "receipt", target: ShipmentReceipt, cardinality: { to: "one", from: "many" }, inverse: "inspections" }],
})

// LAW-LINK-1: a linked inspection inspects no more than its receipt received, in the receipt's unit.
export const InspectionWithinReceipt = Norn.law("within_receipt", {
  on: Inspection,
  when: (self): self is typeof self & { readonly receipt: NonNullable<typeof self.receipt> } => self.receipt !== null,
  must: (self) => self.inspected_qty <= self.receipt.received_qty && self.unit === self.receipt.unit,
  because: "An inspection linked to a shipment receipt must use the receipt's unit and inspect no more than was received.",
})

// LAW-LINK-MANY-1: total inspected quantity across all inspections of a receipt never exceeds received_qty.
export const ReceiptInspectionsWithinReceived = Norn.law("inspections_within_received", {
  on: ShipmentReceipt,
  inverses: { inspections: Inspection },
  when: (self) => self.inspections.length > 0,
  must: (self) => Norn.sum(self.inspections.map((i) => i.inspected_qty)) <= self.received_qty,
  because: "The sum of inspected quantities across all inspections of a shipment receipt cannot exceed the quantity received.",
})

export const RejectedWithinInspected = Norn.law("rejected_within_inspected", {
  on: Inspection,
  must: (self) => self.rejected_qty >= 0 && self.rejected_qty <= self.inspected_qty,
  because: "An inspection never rejects more than it inspected, nor a negative quantity.",
})
export const RejectedWithinHolds = Norn.example("an inspection rejecting part of its sample holds", {
  of: RejectedWithinInspected,
  given: { inspection_id: "IN-1", receipt_id: "RC-1", inspected_qty: 50, rejected_qty: 2, unit: "each" },
  expect: "holds",
})
export const OverRejectedViolated = Norn.example("rejecting more than inspected is refused", {
  of: RejectedWithinInspected,
  given: { inspection_id: "IN-2", receipt_id: "RC-1", inspected_qty: 5, rejected_qty: 6, unit: "each" },
  expect: "violated",
})

/**
 * One Purchase Receipt Item row from the ERP supplier feed: evidence with the
 * supplier's vendor code mapped to the canonical supplier and its source row kept.
 */
export const SupplierReceiptLine = Norn.type({
  name: "supply.SupplierReceiptLine", title: "Supplier receipt row", version: "0.1.0",
  label: { en: "Supplier receipt row", ar: "سطر استلام المورد" },
  implements: [ReceiptEvidence],
  properties: Schema.Struct({
    receipt_line_id: Schema.String,
    supplier_id: Schema.String,
    po_item_ref: Schema.NullOr(Schema.String),
    received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    damaged_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    unit: Unit,
    received_at: Norn.timestamp("utc"),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["receipt_line_id"] }],
})

// ------------------------------------------------------------- forecasting

/** One SKU × warehouse × ISO week of observed demand: the forecasting model's input row. */
export const DemandObservation = Norn.type({
  name: "supply.DemandObservation", title: "Weekly demand observation", version: "0.1.0",
  label: { en: "Weekly demand", ar: "الطلب الأسبوعي" },
  properties: Schema.Struct({
    observation_id: Schema.String,
    sku_id: Schema.String,
    warehouse_id: Schema.String,
    week_start: Norn.timestamp("utc"),
    week_index: Schema.Int,
    issued_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
    unit: Unit,
    lead_time_days: Schema.Int,
    promotion: Schema.Boolean,
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["observation_id"] }],
  links: [
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
  ],
})

/** One prediction of `supply.demand_forecast`: written only from a production model version. */
export const DemandForecast = Norn.type({
  name: "supply.DemandForecast", title: "Demand forecast", version: "0.1.0",
  label: { en: "Demand forecast", ar: "توقع الطلب" },
  properties: Schema.Struct({
    forecast_id: Schema.String,
    subject: Schema.String, // the scored DemandObservation id
    model_version: Schema.String, // the ml.ModelVersion version_id
    sku_id: Schema.String,
    warehouse_id: Schema.String,
    week_start: Norn.timestamp("utc"),
    // `core.quantity` (decimal(6)): the forecast law compares it numerically (`>= 0`).
    predicted_qty: Norn.ref("core.quantity"),
    stockout_risk: Schema.Literals(["low", "medium", "high"]),
  }),
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["forecast_id"] }],
  links: [
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
    { id: "warehouse", target: Warehouse, cardinality: { to: "one", from: "many", required: true } },
    { id: "scored_by", target: "ml.ModelVersion", cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const ForecastNonNegative = Norn.law("forecast_non_negative", {
  on: DemandForecast,
  must: (self) => self.predicted_qty >= 0,
  because: "Demand is never negative; a model output below zero fails its output contract.",
})
export const ForecastNonNegativeHolds = Norn.example("a non-negative forecast holds", {
  of: ForecastNonNegative,
  given: { forecast_id: "F-1", subject: "OBS-1", model_version: "supply.demand_forecast@1", sku_id: "SKU-1", warehouse_id: "WH-A", week_start: "2026-10-05T00:00:00Z", predicted_qty: 140, stockout_risk: "low" },
  expect: "holds",
})
export const ForecastNegativeViolated = Norn.example("a negative forecast is refused", {
  of: ForecastNonNegative,
  given: { forecast_id: "F-2", subject: "OBS-1", model_version: "supply.demand_forecast@1", sku_id: "SKU-1", warehouse_id: "WH-A", week_start: "2026-10-05T00:00:00Z", predicted_qty: -3, stockout_risk: "low" },
  expect: "violated",
})
