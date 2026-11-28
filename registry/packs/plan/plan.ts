import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Commitment } from "../../core/src/rea.ts"
import {
  CountEvidence, OrderLine, PurchaseOrder, ReceiptEvidence, Sku,
} from "../supply-lab/supply.ts"
import { LegalEntity, SaleEvidence, Store } from "../supply-lab/retail.ts"

// Resource Planning Lab (S2 — Versioned Plan Pack & Open-to-Buy):
// Bitemporal, versioned merchandise planning (`PlanVersion`, `PlanSpreadTarget`,
// `PlanLine`, `ActivePlanLine`), governed SKU/PO merchandise grain profiles
// (`SkuProfile`, `PoDestination`), internal purchase-at-cost budget envelopes
// (`OtbEnvelope`), atomic PO commitment bindings (`OtbPoBinding`), and
// published demand/sales forecasts (`PublishedForecast`) composed additively
// with `supply-lab` and `ml` — zero domain opcodes.
//
// Structural guarantees:
// - `PlanTarget` is `disjointWith` `core.Commitment` (budgets/envelopes are
//   internal targets; open `supply.OrderLine`s remain the commitments consuming
//   them) and `disjointWith` actual evidence interfaces (`CountEvidence`,
//   `ReceiptEvidence`, `SaleEvidence`), so plans can never share a type or flag
//   with actuals.
// - `PlanVersion` is bitemporal (D07) with lifecycle
//   `draft -> submitted -> approved -> locked -> superseded`, draft-only edits,
//   all top-down spreads sealed before approval/lock, immutable once `locked`,
//   and authenticated commit time for knowledge selection.
// - `PlanLine` lives strictly at `(entity x store x category x brand x ISO month)`
//   grain with strict `YYYY-MM` / `period_start` / EOM `as_of` alignment,
//   additive flow measures (sales, receipts, markdowns, UCUM fine-metal grams
//   `#1794`), and semi-additive EOM stock (`METRIC-ADDITIVITY-1`,
//   `METRIC-UNIT-PARTITION-1`).
// - `PlanSpreadTarget` enforces deterministic exact top-down spread conservation
//   (`LAW-LINK-MANY-1`) with explicit quantum-aligned rounding remainder rules.
// - `PurchaseOrderRequiresOtbBindings` blocks unbudgeted `supply.po.submit`
//   bypasses whenever the Plan pack is installed; `plan.otb.authorize_po_line`
//   performs the fenced OTB check, creates `OtbPoBinding`, and submits
//   `supply.PurchaseOrder` in one atomic checked transaction using governed
//   `OrderLine.unit_price`, `Sku.category`, `SkuProfile.brand`, and
//   `PoDestination` store/entity/month facts.

const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const PlanUnit = Schema.Literals(["each", "kg", "l", "g"])
const SupplyUnit = Schema.Literals(["each", "kg", "l"])
const SupplyCategory = Schema.Literals(["dry_goods", "chilled", "pharma", "spares"])
const RoundingRule = Schema.Literals(["largest_remainder", "last_bucket_remainder", "exact_no_remainder"])
const PlanVersionStatus = Schema.Literals(["draft", "submitted", "approved", "locked", "superseded"])
const SpreadStatus = Schema.Literals(["open", "sealed"])
const EnvelopeKind = Schema.Literals(["planned", "unplanned"])
const EnvelopeStatus = Schema.Literals(["draft", "pending_override", "active", "closed"])
const OtbBindingStatus = Schema.Literals(["authorized", "partially_received", "fulfilled", "cancelled"])
const ForecastStatus = Schema.Literals(["draft", "published", "superseded"])

const CoreUnitsDep = [{ artifactId: "core.units", version: "1.0.0", kind: "entity" as const }]

const PlanUnitLabels = {
  each: { en: "Each", ar: "قطعة" },
  kg: { en: "Kilogram", ar: "كيلوغرام" },
  l: { en: "Litre", ar: "لتر" },
  g: { en: "Gram", ar: "غرام" },
}

// ------------------------------------------------ structural disjointness

/**
 * Shared contract for planning targets and internal budget envelopes
 * (`INTERFACE-DISJOINT-1`):
 * - Strictly disjoint from `core.Commitment` (budgets are internal targets;
 *   open `supply.OrderLine`s remain the commitments consuming them).
 * - Strictly disjoint from actual retail/supply evidence (`CountEvidence`,
 *   `ReceiptEvidence`, `SaleEvidence`), so plans never mix with actuals.
 */
export const PlanTarget = Norn.interfaceType({
  name: "plan.PlanTarget", title: "Internal planning target", version: "0.1.0",
  label: { en: "Planning target", ar: "هدف التخطيط" },
  needsReview: ["ar"],
  disjointWith: [Commitment, CountEvidence, ReceiptEvidence, SaleEvidence],
  properties: Schema.Struct({
    entity_id: Schema.String,
    currency: Currency,
    plan_month: Schema.String,
  }),
})

// ----------------------------------------- governed SKU & PO grain metadata

const SkuProfileProps = Schema.Struct({
  sku_id: Schema.String,
  category: Schema.Literals(["dry_goods", "chilled", "pharma", "spares"]),
  brand: Schema.String,
  unit_of_measure: SupplyUnit,
})

/**
 * Governed merchandise brand & category profile for a `supply.Sku` (keyed
 * 1-to-1 by `sku_id`), so OTB authorization and `Actual - Plan` variance derive
 * `(category, brand)` strictly from governed catalog facts rather than caller
 * claims.
 */
export const SkuProfile = Norn.type({
  name: "plan.SkuProfile", title: "SKU merchandise profile", version: "0.1.0",
  label: { en: "SKU profile", ar: "ملف الصنف التجاري" },
  properties: SkuProfileProps,
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["sku_id"] }],
  links: [
    { id: "sku", target: Sku, cardinality: { to: "one", from: "one", required: true } },
  ],
})

export const SkuProfileMatchesSku = Norn.law("matches_sku", {
  on: SkuProfile,
  must: (self) =>
    self.sku_id === self.sku.sku_id
    && self.category === self.sku.category
    && self.unit_of_measure === self.sku.unit_of_measure
    && Norn.byteLength(self.brand) > 0,
  because: "A SKU merchandise profile must match its catalog SKU's identity, category, and unit of measure and carry a non-empty brand.",
})

const PoDestinationProps = Schema.Struct({
  po_id: Schema.String,
  store_id: Schema.String,
  entity_id: Schema.String,
  plan_month: Schema.String,
  period_month_date: Norn.date(),
  currency: Currency,
})

/**
 * Governed destination store, legal entity, and planning month binding for a
 * `supply.PurchaseOrder` (keyed 1-to-1 by `po_id`), so atomic OTB authorization
 * derives `(entity_id, store_id, plan_month, currency)` strictly from governed
 * order and store facts.
 */
export const PoDestination = Norn.type({
  name: "plan.PoDestination", title: "Purchase order destination grain", version: "0.1.0",
  label: { en: "PO destination", ar: "وجهة أمر الشراء" },
  properties: PoDestinationProps,
  needsReview: ["ar"],
  identity: [{ name: "primary", properties: ["po_id"] }],
  links: [
    { id: "order", target: PurchaseOrder, cardinality: { to: "one", from: "one", required: true } },
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const PoDestinationMatchesOrderAndStore = Norn.law("matches_order_and_store", {
  on: PoDestination,
  must: (self) =>
    self.po_id === self.order.po_id
    && self.store_id === self.store.store_id
    && self.entity_id === self.store.entity_id
    && self.entity_id === self.owner_entity.entity_id
    && self.currency === self.store.currency
    && self.currency === self.owner_entity.functional_currency
    && self.currency === self.order.currency
    && Norn.byteLength(self.plan_month) === 7
    && Norn.parseDate(self.period_month_date) === Norn.parseDate(self.plan_month + "-01")
    && Norn.parseTimestamp(self.order.promised_at) >= Norn.parseTimestamp(self.plan_month + "-01T00:00:00Z")
    && Norn.secondsBetween(self.plan_month + "-01T00:00:00Z", self.order.promised_at) < (
      (self.plan_month.endsWith("-04") || self.plan_month.endsWith("-06") || self.plan_month.endsWith("-09") || self.plan_month.endsWith("-11"))
        ? 30 * 86400
        : self.plan_month.endsWith("-02") ? 29 * 86400 : 31 * 86400
    ),
  because: "A purchase order destination must align with its order currency, store owning entity, and canonical YYYY-MM month containing promised_at.",
})

// ------------------------------------------------ bitemporal PlanVersion (D07)

const PlanVersionProps = Schema.Struct({
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  plan_code: Schema.String,
  revision_number: Schema.Int,
  currency: Currency,
  // Bitemporal window (D07): effective horizon vs authenticated commit clock time.
  effective_from: Norn.timestamp("utc"),
  effective_to: Norn.timestamp("utc"),
  recorded_at: Norn.timestamp("utc"),
  // Successor points to its locked predecessor; locked history is never mutated.
  supersedes_version_id: Schema.NullOr(Schema.String),
  created_by: Schema.String,
  status: PlanVersionStatus,
})

/**
 * Versioned, bitemporal merchandise plan container (D07).
 * Lifecycle: `draft -> submitted -> approved -> locked -> superseded`.
 * Edits are restricted strictly to `draft`; all top-down spreads must be sealed
 * before approval/lock; immutable once `locked`; superseding links successor ->
 * predecessor without mutating locked history.
 */
export const PlanVersion = Norn.type({
  name: "plan.PlanVersion", title: "Plan version", version: "0.1.0",
  label: { en: "Plan version", ar: "إصدار الخطة" },
  properties: PlanVersionProps,
  propertyLabels: {
    plan_version_id: { en: "Plan version ID", ar: "معرّف إصدار الخطة" },
    plan_code: { en: "Plan code", ar: "رمز الخطة" },
    revision_number: { en: "Revision", ar: "المراجعة" },
    effective_from: { en: "Effective from", ar: "ساري من" },
    effective_to: { en: "Effective to", ar: "ساري حتى" },
    recorded_at: { en: "Recorded at", ar: "وقت التسجيل" },
  },
  valueLabels: {
    status: {
      draft: { en: "Draft", ar: "مسودة" },
      submitted: { en: "Submitted", ar: "مُقدّم" },
      approved: { en: "Approved", ar: "معتمد" },
      locked: { en: "Locked", ar: "مقفل" },
      superseded: { en: "Superseded", ar: "مُستبدل" },
    },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["plan_version_id"] },
    { name: "entity_plan_revision", properties: ["entity_id", "plan_code", "revision_number"] },
  ],
  protected: ["created_by"],
  defaults: { status: "draft" },
  links: [
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
    { id: "supersedes", target: "plan.PlanVersion", cardinality: { to: "one", from: "one" } },
  ],
  lifecycles: [{
    property: "status",
    initial: ["draft"],
    terminal: ["superseded"],
    immutable: ["locked", "superseded"],
    endings: { locked: "resting", superseded: "completed" },
    transitions: [
      { id: "submit", from: ["draft"], to: "submitted" },
      { id: "approve", from: ["submitted"], to: "approved" },
      { id: "lock", from: ["approved"], to: "locked" },
      { id: "supersede", from: ["locked"], to: "superseded" },
    ],
  }],
})

export const PlanVersionValidWindow = Norn.law("valid_window", {
  on: PlanVersion,
  must: (self) => self.revision_number >= 1 && self.effective_from < self.effective_to,
  because: "A plan version carries a positive revision number and a strictly ordered bitemporal effective window.",
})
export const PlanVersionValidHolds = Norn.example("a valid bitemporal plan version holds", {
  of: PlanVersionValidWindow,
  given: {
    plan_version_id: "PV-2026-V1", entity_id: "LE-SOLE", plan_code: "FY2026-AUTUMN", revision_number: 1,
    currency: "EGP", effective_from: "2026-09-01T00:00:00Z", effective_to: "2026-10-01T00:00:00Z",
    recorded_at: "2026-08-20T09:00:00Z", supersedes_version_id: null, created_by: "planner:amal", status: "draft",
  },
  expect: "holds",
})
export const PlanVersionInvertedWindowViolated = Norn.example("an inverted effective window is refused", {
  of: PlanVersionValidWindow,
  given: {
    plan_version_id: "PV-2026-BAD", entity_id: "LE-SOLE", plan_code: "FY2026-AUTUMN", revision_number: 1,
    currency: "EGP", effective_from: "2026-10-01T00:00:00Z", effective_to: "2026-09-01T00:00:00Z",
    recorded_at: "2026-08-20T09:00:00Z", supersedes_version_id: null, created_by: "planner:amal", status: "draft",
  },
  expect: "violated",
})

export const PlanVersionMatchesOwnerCurrency = Norn.law("matches_owner_currency", {
  on: PlanVersion,
  must: (self) => self.currency === self.owner_entity.functional_currency,
  because: "A plan version must be denominated in its owning legal entity's functional currency.",
})

export const PlanVersionSupersedesDistinct = Norn.law("supersedes_distinct", {
  on: PlanVersion,
  when: (self) => self.supersedes_version_id !== null,
  must: (self) =>
    (self.supersedes_version_id ?? "") !== self.plan_version_id
    && self.revision_number >= 2,
  because: "A superseding plan version must name a distinct predecessor and advance revision_number past 1 without mutating locked history.",
})

// ---------------------------------------- top-down spread conservation parent

const PlanSpreadTargetProps = Schema.Struct({
  spread_target_id: Schema.String,
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String, // canonical ISO month "YYYY-MM"
  period_month_date: Norn.date(), // exact first calendar day "YYYY-MM-01"
  currency: Currency,
  rounding_rule: RoundingRule,
  bucket_count: Schema.Int,
  allocation_quantum_cost: Norn.decimal(6, { additivity: { kind: "non_additive" } }),
  target_remainder_quanta: Schema.Int,
  target_sales_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_sales_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_receipts_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_receipts_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_markdowns_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_markdowns_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_eom_stock_cost: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
  // Fine-metal grams target via UCUM (#1794) for jewelry categories.
  target_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" }, additivity: { kind: "additive" } }),
  // Explicit deterministic decimal rounding remainder distributed to child lines.
  target_rounding_remainder_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  status: SpreadStatus,
})

/**
 * Top-down merchandise plan target at `(plan_version, entity, category, brand,
 * ISO month)` grain. Sealing requires exact conservation across all
 * `bucket_count` child `PlanLine`s with deterministic quantum-aligned rounding
 * remainder allocation.
 */
export const PlanSpreadTarget = Norn.type({
  name: "plan.PlanSpreadTarget", title: "Top-down plan spread target", version: "0.1.0",
  label: { en: "Plan spread target", ar: "هدف توزيع الخطة" },
  dependencies: CoreUnitsDep,
  implements: [PlanTarget],
  properties: PlanSpreadTargetProps,
  valueLabels: {
    rounding_rule: {
      largest_remainder: { en: "Largest remainder", ar: "أكبر باقٍ" },
      last_bucket_remainder: { en: "Last bucket remainder", ar: "باقي الفئة الأخيرة" },
      exact_no_remainder: { en: "Exact (no remainder)", ar: "دقيق بلا باقٍ" },
    },
    status: { open: { en: "Open", ar: "مفتوح" }, sealed: { en: "Sealed", ar: "مختوم" } },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["spread_target_id"] },
    { name: "spread_grain", properties: ["plan_version_id", "entity_id", "category", "brand", "plan_month"] },
  ],
  defaults: { status: "open" },
  links: [
    { id: "plan_version", target: PlanVersion, cardinality: { to: "one", from: "many", required: true }, inverse: "spread_targets" },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [{
    property: "status",
    initial: ["open"],
    terminal: ["sealed"],
    immutable: ["sealed"],
    endings: { sealed: "completed" },
    transitions: [{ id: "seal", from: ["open"], to: "sealed" }],
  }],
})

// LAW-LINK-MANY-1: every top-down spread under a PlanVersion must be sealed before approval or lock.
export const PlanVersionSpreadsSealed = Norn.law("spreads_sealed_before_approval", {
  on: PlanVersion,
  inverses: { spread_targets: PlanSpreadTarget },
  when: (self) => self.status === "approved" || self.status === "locked" || self.status === "superseded",
  must: (self) => self.spread_targets.every((s) => s.status === "sealed"),
  because: "A plan version cannot be approved or locked while any top-down PlanSpreadTarget under it remains unsealed.",
})

export const SpreadTargetValid = Norn.law("spread_target_valid", {
  on: PlanSpreadTarget,
  must: (self) =>
    Norn.byteLength(self.plan_month) === 7
    && Norn.parseDate(self.period_month_date) === Norn.parseDate(self.plan_month + "-01")
    && self.bucket_count >= 1
    && self.allocation_quantum_cost > 0
    && self.target_remainder_quanta >= 0
    && self.target_remainder_quanta < self.bucket_count
    && self.target_sales_retail >= 0
    && self.target_sales_cost >= 0
    && self.target_receipts_cost >= 0
    && self.target_receipts_retail >= 0
    && self.target_markdowns_cost >= 0
    && self.target_markdowns_retail >= 0
    && self.target_eom_stock_cost >= 0
    && self.target_fine_metal_g >= self.target_fine_metal_g - self.target_fine_metal_g
    && self.target_rounding_remainder_cost === self.target_remainder_quanta * self.allocation_quantum_cost
    && (self.rounding_rule !== "exact_no_remainder" || self.target_remainder_quanta === 0),
  because: "A top-down spread target carries non-negative measures, a valid YYYY-MM ISO month, positive bucket count and quantum, and an exact integer-quanta remainder strictly below bucket_count (zero under exact_no_remainder).",
})
export const SpreadTargetValidHolds = Norn.example("a valid top-down spread target holds", {
  of: SpreadTargetValid,
  given: {
    spread_target_id: "SP-1", plan_version_id: "PV-2026-V1", entity_id: "LE-SOLE",
    category: "dry_goods", brand: "nile_foods", plan_month: "2026-09", period_month_date: "2026-09-01",
    currency: "EGP", rounding_rule: "largest_remainder", bucket_count: 2, allocation_quantum_cost: 0.01,
    target_remainder_quanta: 1,
    target_sales_retail: 10000, target_sales_cost: 6000,
    target_receipts_cost: 10000.01, target_receipts_retail: 16000, target_markdowns_cost: 200,
    target_markdowns_retail: 350, target_eom_stock_cost: 5000, target_fine_metal_g: 0,
    target_rounding_remainder_cost: 0.01, status: "open",
  },
  expect: "holds",
})
export const SpreadTargetExactWithRemainderViolated = Norn.example("exact_no_remainder with non-zero remainder is refused", {
  of: SpreadTargetValid,
  given: {
    spread_target_id: "SP-2", plan_version_id: "PV-2026-V1", entity_id: "LE-SOLE",
    category: "dry_goods", brand: "nile_foods", plan_month: "2026-09", period_month_date: "2026-09-01",
    currency: "EGP", rounding_rule: "exact_no_remainder", bucket_count: 2, allocation_quantum_cost: 0.01,
    target_remainder_quanta: 1,
    target_sales_retail: 10000, target_sales_cost: 6000,
    target_receipts_cost: 10000.01, target_receipts_retail: 16000, target_markdowns_cost: 200,
    target_markdowns_retail: 350, target_eom_stock_cost: 5000, target_fine_metal_g: 0,
    target_rounding_remainder_cost: 0.01, status: "open",
  },
  expect: "violated",
})
export const SpreadTargetRemainderAboveBoundViolated = Norn.example("remainder reaching bucket_count * quantum is refused", {
  of: SpreadTargetValid,
  given: {
    spread_target_id: "SP-3", plan_version_id: "PV-2026-V1", entity_id: "LE-SOLE",
    category: "dry_goods", brand: "nile_foods", plan_month: "2026-09", period_month_date: "2026-09-01",
    currency: "EGP", rounding_rule: "largest_remainder", bucket_count: 2, allocation_quantum_cost: 0.01,
    target_remainder_quanta: 2,
    target_sales_retail: 10000, target_sales_cost: 6000,
    target_receipts_cost: 10000.02, target_receipts_retail: 16000, target_markdowns_cost: 200,
    target_markdowns_retail: 350, target_eom_stock_cost: 5000, target_fine_metal_g: 0,
    target_rounding_remainder_cost: 0.02, status: "open",
  },
  expect: "violated",
})

// -------------------------------------------------- PlanLine (leaf plan grain)

const PlanLineProps = Schema.Struct({
  plan_line_id: Schema.String,
  plan_version_id: Schema.String,
  spread_target_id: Schema.NullOr(Schema.String),
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String, // canonical ISO month "YYYY-MM"
  period_month_date: Norn.date(), // exact first calendar day "YYYY-MM-01"
  period_start: Norn.timestamp("utc"), // first UTC instant of the ISO month
  as_of: Norn.timestamp("utc"), // exact end-of-month UTC instant for semi-additive EOM stock
  currency: Currency,
  unit: PlanUnit,
  // Additive flow measures:
  planned_sales_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  planned_sales_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  planned_sales_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  planned_receipts_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  planned_receipts_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  planned_receipts_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  planned_markdowns_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  planned_markdowns_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  // Jewelry fine-metal mass target via UCUM (#1794), strictly distinct from currency and pieces.
  planned_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" }, additivity: { kind: "additive" } }),
  // Deterministic top-down spread decomposition: planned_receipts_cost == spread_base_receipts_cost + spread_remainder_cost.
  spread_bucket_index: Schema.Int,
  spread_base_quanta: Schema.Int,
  spread_remainder_quanta: Schema.Int,
  spread_base_receipts_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  spread_remainder_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  // Semi-additive end-of-month stock measures (never summed across months):
  planned_eom_stock_cost: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
  planned_eom_stock_retail: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
  planned_eom_stock_qty: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["unit"] }),
})

/**
 * Atomic merchandise plan line at `(entity x store x category x brand x ISO month)`
 * grain within one `PlanVersion`. Strictly distinct from actual fact types
 * (`PlanTarget` disjoint from `SaleEvidence`, `CountEvidence`, `ReceiptEvidence`,
 * and `core.Commitment`).
 */
export const PlanLine = Norn.type({
  name: "plan.PlanLine", title: "Merchandise plan line", version: "0.1.0",
  label: { en: "Plan line", ar: "سطر الخطة" },
  dependencies: CoreUnitsDep,
  implements: [PlanTarget],
  properties: PlanLineProps,
  valueLabels: { unit: PlanUnitLabels },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["plan_line_id"] },
    { name: "plan_grain", properties: ["plan_version_id", "entity_id", "store_id", "category", "brand", "plan_month"] },
  ],
  links: [
    { id: "plan_version", target: PlanVersion, cardinality: { to: "one", from: "many", required: true }, inverse: "lines" },
    { id: "spread_target", target: PlanSpreadTarget, cardinality: { to: "one", from: "many" }, inverse: "plan_lines" },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const PlanLineMeasuresNonNegative = Norn.law("measures_non_negative", {
  on: PlanLine,
  must: (self) =>
    Norn.byteLength(self.plan_month) === 7
    && Norn.parseDate(self.period_month_date) === Norn.parseDate(self.plan_month + "-01")
    && Norn.parseTimestamp(self.period_start) === Norn.parseTimestamp(self.plan_month + "-01T00:00:00Z")
    && (
      (
        (self.plan_month.endsWith("-01") || self.plan_month.endsWith("-03") || self.plan_month.endsWith("-05")
          || self.plan_month.endsWith("-07") || self.plan_month.endsWith("-08") || self.plan_month.endsWith("-10")
          || self.plan_month.endsWith("-12"))
        && Norn.secondsBetween(self.period_start, self.as_of) === (31 * 86400) - 1
      )
      || (
        (self.plan_month.endsWith("-04") || self.plan_month.endsWith("-06")
          || self.plan_month.endsWith("-09") || self.plan_month.endsWith("-11"))
        && Norn.secondsBetween(self.period_start, self.as_of) === (30 * 86400) - 1
      )
      || (
        self.plan_month.endsWith("-02")
        && (
          Norn.secondsBetween(self.period_start, self.as_of) === (28 * 86400) - 1
          || Norn.secondsBetween(self.period_start, self.as_of) === (29 * 86400) - 1
        )
      )
    )
    && self.planned_sales_retail >= 0
    && self.planned_sales_cost >= 0
    && self.planned_sales_qty >= 0
    && self.planned_receipts_cost >= 0
    && self.planned_receipts_retail >= 0
    && self.planned_receipts_qty >= 0
    && self.planned_markdowns_cost >= 0
    && self.planned_markdowns_retail >= 0
    && self.planned_fine_metal_g >= self.planned_fine_metal_g - self.planned_fine_metal_g
    && self.spread_bucket_index >= 0
    && self.spread_base_quanta >= 0
    && self.spread_remainder_quanta >= 0
    && self.spread_base_receipts_cost >= 0
    && self.spread_remainder_cost >= 0
    && self.planned_receipts_cost === self.spread_base_receipts_cost + self.spread_remainder_cost
    && (self.spread_target_id !== null || (self.spread_bucket_index === 0 && self.spread_base_quanta === 0 && self.spread_remainder_quanta === 0 && self.spread_remainder_cost === 0))
    && self.planned_eom_stock_cost >= 0
    && self.planned_eom_stock_retail >= 0
    && self.planned_eom_stock_qty >= 0,
  because: "A plan line carries non-negative measures, exact spread decomposition, a valid YYYY-MM ISO month, matching period_start, and an exact end-of-month EOM stock snapshot timestamp.",
})
export const PlanLineValidHolds = Norn.example("a valid jewelry/retail plan line holds", {
  of: PlanLineMeasuresNonNegative,
  given: {
    plan_line_id: "PL-1", plan_version_id: "PV-2026-V1", spread_target_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "gold_jewelry", brand: "nile_gold",
    plan_month: "2026-09", period_month_date: "2026-09-01",
    period_start: "2026-09-01T00:00:00Z", as_of: "2026-09-30T23:59:59Z",
    currency: "EGP", unit: "g",
    planned_sales_retail: 50000, planned_sales_cost: 38000, planned_sales_qty: 120,
    planned_receipts_cost: 40000, planned_receipts_retail: 52000, planned_receipts_qty: 125,
    planned_markdowns_cost: 500, planned_markdowns_retail: 800, planned_fine_metal_g: 125.5,
    spread_bucket_index: 0, spread_base_quanta: 0, spread_remainder_quanta: 0, spread_base_receipts_cost: 40000, spread_remainder_cost: 0,
    planned_eom_stock_cost: 15000, planned_eom_stock_retail: 20000, planned_eom_stock_qty: 40,
  },
  expect: "holds",
})
export const PlanLineNegativeReceiptsViolated = Norn.example("a negative planned receipts budget is refused", {
  of: PlanLineMeasuresNonNegative,
  given: {
    plan_line_id: "PL-BAD", plan_version_id: "PV-2026-V1", spread_target_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", period_month_date: "2026-09-01",
    period_start: "2026-09-01T00:00:00Z", as_of: "2026-09-30T23:59:59Z",
    currency: "EGP", unit: "each",
    planned_sales_retail: 10000, planned_sales_cost: 6000, planned_sales_qty: 100,
    planned_receipts_cost: -100, planned_receipts_retail: 12000, planned_receipts_qty: 100,
    planned_markdowns_cost: 0, planned_markdowns_retail: 0, planned_fine_metal_g: 0,
    spread_bucket_index: 0, spread_base_quanta: 0, spread_remainder_quanta: 0, spread_base_receipts_cost: -100, spread_remainder_cost: 0,
    planned_eom_stock_cost: 3000, planned_eom_stock_retail: 5000, planned_eom_stock_qty: 30,
  },
  expect: "violated",
})
export const PlanLineMisalignedEomViolated = Norn.example("a misaligned EOM snapshot date is refused", {
  of: PlanLineMeasuresNonNegative,
  given: {
    plan_line_id: "PL-BAD-EOM", plan_version_id: "PV-2026-V1", spread_target_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", period_month_date: "2026-09-01",
    period_start: "2026-09-01T00:00:00Z", as_of: "2026-09-15T23:59:59Z",
    currency: "EGP", unit: "each",
    planned_sales_retail: 10000, planned_sales_cost: 6000, planned_sales_qty: 100,
    planned_receipts_cost: 8000, planned_receipts_retail: 12000, planned_receipts_qty: 100,
    planned_markdowns_cost: 0, planned_markdowns_retail: 0, planned_fine_metal_g: 0,
    spread_bucket_index: 0, spread_base_quanta: 0, spread_remainder_quanta: 0, spread_base_receipts_cost: 8000, spread_remainder_cost: 0,
    planned_eom_stock_cost: 3000, planned_eom_stock_retail: 5000, planned_eom_stock_qty: 30,
  },
  expect: "violated",
})

export const PlanLineMatchesVersionAndStore = Norn.law("matches_version_and_store", {
  on: PlanLine,
  must: (self) =>
    self.entity_id === self.plan_version.entity_id
    && self.currency === self.plan_version.currency
    && self.entity_id === self.store.entity_id
    && self.currency === self.store.currency
    && self.period_start >= self.plan_version.effective_from
    && self.period_start < self.plan_version.effective_to,
  because: "A plan line must match its plan version and store legal entity and currency, and fall inside its plan version's effective window.",
})

export const PlanLineMatchesSpreadTarget = Norn.law("matches_spread_target", {
  on: PlanLine,
  when: (self): self is typeof self & { readonly spread_target: NonNullable<typeof self.spread_target> } => self.spread_target !== null,
  must: (self) =>
    (self.spread_target_id ?? "") === self.spread_target.spread_target_id
    && self.plan_version_id === self.spread_target.plan_version_id
    && self.entity_id === self.spread_target.entity_id
    && self.category === self.spread_target.category
    && self.brand === self.spread_target.brand
    && self.plan_month === self.spread_target.plan_month
    && self.currency === self.spread_target.currency
    && self.spread_bucket_index >= 1
    && self.spread_bucket_index <= self.spread_target.bucket_count
    && self.spread_base_receipts_cost === self.spread_base_quanta * self.spread_target.allocation_quantum_cost
    && self.spread_remainder_cost === self.spread_remainder_quanta * self.spread_target.allocation_quantum_cost
    && (
      (self.spread_target.rounding_rule === "exact_no_remainder" && self.spread_remainder_quanta === 0)
      || (self.spread_target.rounding_rule === "largest_remainder"
        && (self.spread_remainder_quanta === 0 || self.spread_remainder_quanta === 1))
      || (self.spread_target.rounding_rule === "last_bucket_remainder"
        && (
          (self.spread_bucket_index < self.spread_target.bucket_count && self.spread_remainder_quanta === 0)
          || (self.spread_bucket_index === self.spread_target.bucket_count && self.spread_remainder_quanta === self.spread_target.target_remainder_quanta)
        ))
    ),
  because: "A child plan line linked to a top-down spread target must match its parent grain, keep spread_base_receipts_cost aligned to allocation_quantum_cost, and obey the declared deterministic rounding rule.",
})

// LAW-LINK-MANY-1: exact top-down spread conservation across child PlanLines.
export const SpreadTargetConservation = Norn.law("spread_conservation", {
  on: PlanSpreadTarget,
  inverses: { plan_lines: PlanLine },
  when: (self) => self.status === "sealed",
  must: (self) =>
    self.plan_lines.length === self.bucket_count
    && Norn.sum(self.plan_lines.map((l) => l.spread_bucket_index)) * 2 === self.bucket_count * (self.bucket_count + 1)
    && Norn.sum(self.plan_lines.map((l) => l.spread_base_receipts_cost)) + self.target_rounding_remainder_cost === self.target_receipts_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_sales_retail)) === self.target_sales_retail
    && Norn.sum(self.plan_lines.map((l) => l.planned_sales_cost)) === self.target_sales_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_receipts_cost)) === self.target_receipts_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_receipts_retail)) === self.target_receipts_retail
    && Norn.sum(self.plan_lines.map((l) => l.planned_markdowns_cost)) === self.target_markdowns_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_markdowns_retail)) === self.target_markdowns_retail
    && Norn.sum(self.plan_lines.map((l) => l.planned_eom_stock_cost)) === self.target_eom_stock_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_fine_metal_g)) === self.target_fine_metal_g
    && Norn.sum(self.plan_lines.map((l) => l.spread_remainder_cost)) === self.target_rounding_remainder_cost,
  because: "Child plan lines under a sealed top-down spread target must cover all declared buckets and sum exactly to every parent measure, quantum base, and explicit rounding remainder with zero silent drift.",
})

// ------------------------------------ active baseline slot for Actual - Plan

const ActivePlanLineProps = Schema.Struct({
  slot_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: SupplyCategory,
  brand: Schema.String,
  plan_month: Schema.String,
  period_month_date: Norn.date(),
  currency: Currency,
  unit: SupplyUnit,
})

/**
 * Governed active baseline pointer at unique `(entity x store x category x
 * brand x period_month_date x currency x unit)` grain, pinning the active
 * locked `PlanVersion` & `PlanLine` for chasm-trap-safe `Actual - Plan`
 * variance joins without duplicating actuals across plan revisions.
 */
export const ActivePlanLine = Norn.type({
  name: "plan.ActivePlanLine", title: "Active plan baseline slot", version: "0.1.0",
  label: { en: "Active plan baseline", ar: "سطر الخطة المعتمد النشط" },
  implements: [PlanTarget],
  properties: ActivePlanLineProps,
  valueLabels: {
    unit: {
      each: { en: "Each", ar: "قطعة" },
      kg: { en: "Kilogram", ar: "كيلوغرام" },
      l: { en: "Litre", ar: "لتر" },
    },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["slot_id"] },
    { name: "plan_grain_slot", properties: ["entity_id", "store_id", "category", "brand", "period_month_date", "currency", "unit"] },
  ],
  links: [
    { id: "plan_version", target: PlanVersion, cardinality: { to: "one", from: "many", required: true } },
    { id: "plan_line", target: PlanLine, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const ActivePlanLineMatchesLine = Norn.law("matches_plan_line", {
  on: ActivePlanLine,
  must: (self) =>
    self.plan_version_id === self.plan_version.plan_version_id
    && self.plan_version.status === "locked"
    && self.plan_line_id === self.plan_line.plan_line_id
    && self.plan_version_id === self.plan_line.plan_version_id
    && self.entity_id === self.plan_line.entity_id
    && self.store_id === self.plan_line.store_id
    && self.category === self.plan_line.category
    && self.brand === self.plan_line.brand
    && self.plan_month === self.plan_line.plan_month
    && self.period_month_date === self.plan_line.period_month_date
    && self.currency === self.plan_line.currency
    && self.unit === self.plan_line.unit,
  because: "An active plan baseline slot must pin a locked PlanVersion and match its PlanLine's exact grain, month date, currency, and unit.",
})

// ------------------------------------------------ Open-to-Buy (OTB) envelopes

const OtbEnvelopeProps = Schema.Struct({
  envelope_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.NullOr(Schema.String),
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_start: Norn.timestamp("utc"),
  currency: Currency,
  envelope_kind: EnvelopeKind,
  // Approved purchase-at-cost budget ceiling (internal target, NOT core.Commitment).
  approved_budget_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  override_reason: Schema.NullOr(Schema.String),
  requested_by: Schema.String,
  status: EnvelopeStatus,
})

/**
 * Approved purchase-at-cost Open-to-Buy (OTB) budget envelope.
 * Explicitly an internal planning target (`PlanTarget`, disjoint from
 * `core.Commitment`); open `supply.OrderLine`s remain the commitments consuming
 * it. Unbudgeted categories require an explicit `unplanned` envelope with
 * independent override approval (`approve_override`) before PO authorization.
 */
export const OtbEnvelope = Norn.type({
  name: "plan.OtbEnvelope", title: "Open-to-Buy budget envelope", version: "0.1.0",
  label: { en: "OTB envelope", ar: "مظروف الميزانية المتاحة للشراء" },
  implements: [PlanTarget],
  properties: OtbEnvelopeProps,
  valueLabels: {
    envelope_kind: {
      planned: { en: "Planned", ar: "مخطط" },
      unplanned: { en: "Unplanned override", ar: "استثناء غير مخطط" },
    },
    status: {
      draft: { en: "Draft", ar: "مسودة" },
      pending_override: { en: "Pending override approval", ar: "بانتظار اعتماد الاستثناء" },
      active: { en: "Active", ar: "نشط" },
      closed: { en: "Closed", ar: "مغلق" },
    },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["envelope_id"] },
    { name: "envelope_grain", properties: ["plan_version_id", "entity_id", "store_id", "category", "brand", "plan_month"] },
  ],
  protected: ["requested_by"],
  defaults: { status: "draft" },
  links: [
    { id: "plan_version", target: PlanVersion, cardinality: { to: "one", from: "many", required: true } },
    { id: "plan_line", target: PlanLine, cardinality: { to: "one", from: "many" } },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [{
    property: "status",
    initial: ["draft"],
    terminal: ["closed"],
    immutable: ["closed"],
    endings: { active: "resting", closed: "completed" },
    transitions: [
      { id: "activate_planned", from: ["draft"], to: "active" },
      { id: "submit_override", from: ["draft"], to: "pending_override" },
      { id: "approve_override", from: ["pending_override"], to: "active" },
      { id: "close", from: ["draft", "pending_override", "active"], to: "closed" },
    ],
  }],
})

export const OtbEnvelopeValid = Norn.law("envelope_valid", {
  on: OtbEnvelope,
  must: (self) =>
    Norn.byteLength(self.plan_month) === 7
    && Norn.parseTimestamp(self.period_start) === Norn.parseTimestamp(self.plan_month + "-01T00:00:00Z")
    && self.approved_budget_cost >= 0
    && (
      (self.envelope_kind === "planned" && self.plan_line_id !== null)
      || (self.envelope_kind === "unplanned" && self.plan_line_id === null && self.override_reason !== null && self.approved_budget_cost > 0)
    ),
  because: "A planned OTB envelope binds a plan line at a canonical YYYY-MM period_start; an unplanned envelope carries null plan_line_id, an explicit override reason, and a positive override budget.",
})
export const OtbPlannedEnvelopeHolds = Norn.example("a planned OTB envelope holds", {
  of: OtbEnvelopeValid,
  given: {
    envelope_id: "ENV-1", plan_version_id: "PV-2026-V1", plan_line_id: "PL-1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", currency: "EGP",
    envelope_kind: "planned", approved_budget_cost: 10000, override_reason: null,
    requested_by: "planner:amal", status: "active",
  },
  expect: "holds",
})
export const OtbUnplannedWithReasonHolds = Norn.example("an unplanned OTB envelope with override reason holds", {
  of: OtbEnvelopeValid,
  given: {
    envelope_id: "ENV-UNPLANNED-1", plan_version_id: "PV-2026-V1", plan_line_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "spares", brand: "unplanned",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", currency: "EGP",
    envelope_kind: "unplanned", approved_budget_cost: 2500, override_reason: "Emergency chiller spare parts",
    requested_by: "planner:amal", status: "pending_override",
  },
  expect: "holds",
})
export const OtbUnplannedWithoutReasonViolated = Norn.example("an unplanned OTB envelope without override reason is refused", {
  of: OtbEnvelopeValid,
  given: {
    envelope_id: "ENV-UNPLANNED-BAD", plan_version_id: "PV-2026-V1", plan_line_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "spares", brand: "unplanned",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", currency: "EGP",
    envelope_kind: "unplanned", approved_budget_cost: 2500, override_reason: null,
    requested_by: "planner:amal", status: "pending_override",
  },
  expect: "violated",
})

export const OtbEnvelopeMatchesVersionAndStore = Norn.law("matches_version_and_store", {
  on: OtbEnvelope,
  must: (self) =>
    self.entity_id === self.plan_version.entity_id
    && self.currency === self.plan_version.currency
    && self.entity_id === self.store.entity_id
    && self.currency === self.store.currency,
  because: "An OTB envelope must align with its plan version and store legal entity and currency.",
})

// ----------------------------------------- OTB PO commitment binding & guards

const OtbPoBindingProps = Schema.Struct({
  binding_id: Schema.String,
  envelope_id: Schema.String,
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  po_id: Schema.String,
  line_id: Schema.String,
  currency: Currency,
  unit: SupplyUnit,
  ordered_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  received_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  unit_cost: Norn.decimal(4, { additivity: { kind: "non_additive" } }),
  // Exact OTB cost decomposition:
  //   authorized_cost == recognized_receipt_cost + unreceived_authorized_cost + cancelled_released_cost
  //   otb_consumed_cost == recognized_receipt_cost + unreceived_authorized_cost
  // Partial receipts shift from unreceived -> recognized with net OTB conserved;
  // cancellations (strictly after PurchaseOrder reaches `cancelled`) release
  // exact unreceived remainder -> cancelled_released_cost.
  authorized_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  recognized_receipt_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  unreceived_authorized_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  cancelled_released_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  otb_consumed_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  // Authorization-time OTB cap evidence (checked by law + non-vacuity examples).
  envelope_cap_snapshot: Norn.decimal(6, { additivity: { kind: "non_additive" } }),
  prior_consumed_snapshot: Norn.decimal(6, { additivity: { kind: "non_additive" } }),
  // Expected cash payment timing tracked separately from OTB consumption month.
  expected_payment_date: Norn.timestamp("utc"),
  payment_terms_days: Schema.Int,
  as_of: Norn.timestamp("utc"),
  authorized_by: Schema.String,
  status: OtbBindingStatus,
})

/**
 * Binds one open `supply.OrderLine` commitment to its governing `OtbEnvelope`
 * at purchase-at-cost (derived strictly from `OrderLine.unit_price`,
 * `Sku.category`, `SkuProfile.brand`, and `PoDestination`), conserving OTB
 * across repeated partial receipts and releasing exact unreceived value only
 * after `supply.PurchaseOrder` is `cancelled`.
 */
export const OtbPoBinding = Norn.type({
  name: "plan.OtbPoBinding", title: "OTB purchase-order commitment binding", version: "0.1.0",
  label: { en: "OTB PO binding", ar: "ربط أمر الشراء بميزانية الشراء" },
  properties: OtbPoBindingProps,
  valueLabels: {
    status: {
      authorized: { en: "Authorized", ar: "مصرح به" },
      partially_received: { en: "Partially received", ar: "مستلم جزئيًا" },
      fulfilled: { en: "Fulfilled", ar: "مكتمل" },
      cancelled: { en: "Cancelled (released)", ar: "ملغى (محرر)" },
    },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["binding_id"] },
    { name: "order_line_binding", properties: ["line_id"] },
  ],
  defaults: { status: "authorized" },
  links: [
    { id: "envelope", target: OtbEnvelope, cardinality: { to: "one", from: "many", required: true }, inverse: "bindings" },
    { id: "order", target: PurchaseOrder, cardinality: { to: "one", from: "many", required: true }, inverse: "otb_bindings" },
    { id: "order_line", target: OrderLine, cardinality: { to: "one", from: "one", required: true } },
    { id: "sku", target: Sku, cardinality: { to: "one", from: "many", required: true } },
    { id: "sku_profile", target: SkuProfile, cardinality: { to: "one", from: "many", required: true } },
    { id: "po_destination", target: PoDestination, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [{
    property: "status",
    initial: ["authorized"],
    terminal: ["fulfilled", "cancelled"],
    immutable: ["fulfilled", "cancelled"],
    endings: { authorized: "resting", partially_received: "resting", fulfilled: "completed", cancelled: "cancelled" },
    transitions: [
      { id: "receive_partial", from: ["authorized"], to: "partially_received" },
      { id: "fulfil", from: ["authorized", "partially_received"], to: "fulfilled" },
      { id: "cancel", from: ["authorized", "partially_received"], to: "cancelled" },
    ],
  }],
})

// LAW-LINK-MANY-1: zero-bypass OTB gate on PurchaseOrder whenever the Plan pack is installed.
export const PurchaseOrderRequiresOtbBindings = Norn.law("requires_otb_bindings", {
  id: "plan.PurchaseOrder.requires_otb_bindings",
  on: PurchaseOrder,
  inverses: { lines: OrderLine, otb_bindings: OtbPoBinding },
  when: (self) => self.status !== "draft" && self.status !== "cancelled",
  must: (self) =>
    self.lines.length > 0
    && self.otb_bindings.length === self.lines.length,
  because: "When the Plan pack is installed, every submitted or received purchase order must have a valid OTB binding covering every order line, blocking unbudgeted supply.po.submit bypasses.",
})

// LAW-LINK-MANY-1: envelope-wide OTB cap across all active PO bindings.
export const OtbEnvelopeWithinCap = Norn.law("within_approved_cap", {
  on: OtbEnvelope,
  inverses: { bindings: OtbPoBinding },
  when: (self) => self.status === "active" && self.bindings.length > 0,
  must: (self) => Norn.sum(self.bindings.map((b) => b.otb_consumed_cost)) <= self.approved_budget_cost,
  because: "Total OTB consumed across authorized PO lines bound to an active envelope must never exceed its approved purchase-at-cost budget.",
})

export const OtbPoBindingConserved = Norn.law("otb_binding_conserved", {
  on: OtbPoBinding,
  must: (self) =>
    self.ordered_qty > 0
    && self.received_qty >= 0
    && self.received_qty <= self.ordered_qty
    && self.unit_cost > 0
    && self.payment_terms_days >= 0
    && self.authorized_cost > 0
    && self.authorized_cost === self.ordered_qty * self.unit_cost
    && self.recognized_receipt_cost >= 0
    && self.recognized_receipt_cost === self.received_qty * self.unit_cost
    && self.unreceived_authorized_cost >= 0
    && self.cancelled_released_cost >= 0
    && (self.recognized_receipt_cost + self.unreceived_authorized_cost + self.cancelled_released_cost) === self.authorized_cost
    && self.otb_consumed_cost === (self.recognized_receipt_cost + self.unreceived_authorized_cost)
    && (
      (self.status !== "cancelled" && self.cancelled_released_cost === 0)
      || (self.status === "cancelled" && self.unreceived_authorized_cost === 0)
    )
    && (self.status !== "fulfilled" || (self.unreceived_authorized_cost === 0 && self.cancelled_released_cost === 0)),
  because: "OTB binding conserves authorized cost across recognized receipts, unreceived authorized balance, and cancelled releases.",
})
export const OtbPartialReceiptConservedHolds = Norn.example("partial receipt shifts between unreceived and recognized with OTB conserved", {
  of: OtbPoBindingConserved,
  given: {
    binding_id: "OTB-1", envelope_id: "ENV-1", plan_version_id: "PV-2026-V1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", sku_id: "SKU-DRY-1", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", po_id: "PO-1", line_id: "PO-1/1", currency: "EGP", unit: "each",
    ordered_qty: 100, received_qty: 40, unit_cost: 60,
    authorized_cost: 6000, recognized_receipt_cost: 2400, unreceived_authorized_cost: 3600,
    cancelled_released_cost: 0, otb_consumed_cost: 6000,
    envelope_cap_snapshot: 10000, prior_consumed_snapshot: 0,
    expected_payment_date: "2026-10-25T00:00:00Z", payment_terms_days: 45,
    as_of: "2026-09-10T10:00:00Z", authorized_by: "ops:karim", status: "partially_received",
  },
  expect: "holds",
})
export const OtbCancelledReleaseHolds = Norn.example("cancelled PO releases unreceived balance while retaining recognized receipts", {
  of: OtbPoBindingConserved,
  given: {
    binding_id: "OTB-2", envelope_id: "ENV-1", plan_version_id: "PV-2026-V1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", sku_id: "SKU-DRY-1", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", po_id: "PO-1", line_id: "PO-1/1", currency: "EGP", unit: "each",
    ordered_qty: 100, received_qty: 40, unit_cost: 60,
    authorized_cost: 6000, recognized_receipt_cost: 2400, unreceived_authorized_cost: 0,
    cancelled_released_cost: 3600, otb_consumed_cost: 2400,
    envelope_cap_snapshot: 10000, prior_consumed_snapshot: 0,
    expected_payment_date: "2026-10-25T00:00:00Z", payment_terms_days: 45,
    as_of: "2026-09-12T10:00:00Z", authorized_by: "ops:karim", status: "cancelled",
  },
  expect: "holds",
})
export const OtbConservationDriftViolated = Norn.example("unbalanced OTB buckets are refused", {
  of: OtbPoBindingConserved,
  given: {
    binding_id: "OTB-BAD", envelope_id: "ENV-1", plan_version_id: "PV-2026-V1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", sku_id: "SKU-DRY-1", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", po_id: "PO-1", line_id: "PO-1/1", currency: "EGP", unit: "each",
    ordered_qty: 100, received_qty: 40, unit_cost: 60,
    authorized_cost: 6000, recognized_receipt_cost: 2400, unreceived_authorized_cost: 3000,
    cancelled_released_cost: 0, otb_consumed_cost: 5400,
    envelope_cap_snapshot: 10000, prior_consumed_snapshot: 0,
    expected_payment_date: "2026-10-25T00:00:00Z", payment_terms_days: 45,
    as_of: "2026-09-10T10:00:00Z", authorized_by: "ops:karim", status: "partially_received",
  },
  expect: "violated",
})

export const OtbAuthorizationWithinCap = Norn.law("authorization_within_cap", {
  on: OtbPoBinding,
  must: (self) =>
    self.prior_consumed_snapshot >= 0
    && self.envelope_cap_snapshot > 0
    && (self.prior_consumed_snapshot + self.otb_consumed_cost) <= self.envelope_cap_snapshot,
  because: "Authorizing a purchase order line against an OTB envelope must stay within the approved purchase-at-cost budget ceiling.",
})
// Non-vacuity examples proving the OTB cap law fires for both allowed and refused PO authorizations:
export const OtbAuthAllowedExample = Norn.example("allowed PO authorization within remaining OTB holds", {
  of: OtbAuthorizationWithinCap,
  given: {
    binding_id: "OTB-OK", envelope_id: "ENV-1", plan_version_id: "PV-2026-V1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", sku_id: "SKU-DRY-1", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", po_id: "PO-1", line_id: "PO-1/1", currency: "EGP", unit: "each",
    ordered_qty: 100, received_qty: 0, unit_cost: 60,
    authorized_cost: 6000, recognized_receipt_cost: 0, unreceived_authorized_cost: 6000,
    cancelled_released_cost: 0, otb_consumed_cost: 6000,
    envelope_cap_snapshot: 10000, prior_consumed_snapshot: 3500,
    expected_payment_date: "2026-10-25T00:00:00Z", payment_terms_days: 45,
    as_of: "2026-09-02T10:00:00Z", authorized_by: "ops:karim", status: "authorized",
  },
  expect: "holds",
})
export const OtbAuthBreachRefusedExample = Norn.example("PO authorization breaching remaining OTB is refused", {
  of: OtbAuthorizationWithinCap,
  given: {
    binding_id: "OTB-BREACH", envelope_id: "ENV-1", plan_version_id: "PV-2026-V1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", sku_id: "SKU-DRY-1", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", po_id: "PO-2", line_id: "PO-2/1", currency: "EGP", unit: "each",
    ordered_qty: 100, received_qty: 0, unit_cost: 60,
    authorized_cost: 6000, recognized_receipt_cost: 0, unreceived_authorized_cost: 6000,
    cancelled_released_cost: 0, otb_consumed_cost: 6000,
    envelope_cap_snapshot: 10000, prior_consumed_snapshot: 5000,
    expected_payment_date: "2026-10-25T00:00:00Z", payment_terms_days: 45,
    as_of: "2026-09-03T10:00:00Z", authorized_by: "ops:karim", status: "authorized",
  },
  expect: "violated",
})
export const OtbAuthZeroEnvelopeRefusedExample = Norn.example("PO authorization against zero-budget category is refused", {
  of: OtbAuthorizationWithinCap,
  given: {
    binding_id: "OTB-ZERO", envelope_id: "ENV-0", plan_version_id: "PV-2026-V1",
    entity_id: "LE-SOLE", store_id: "ST-CFC", sku_id: "SKU-SPARE-1", category: "spares", brand: "none",
    plan_month: "2026-09", po_id: "PO-3", line_id: "PO-3/1", currency: "EGP", unit: "each",
    ordered_qty: 10, received_qty: 0, unit_cost: 50,
    authorized_cost: 500, recognized_receipt_cost: 0, unreceived_authorized_cost: 500,
    cancelled_released_cost: 0, otb_consumed_cost: 500,
    envelope_cap_snapshot: 0, prior_consumed_snapshot: 0,
    expected_payment_date: "2026-10-25T00:00:00Z", payment_terms_days: 45,
    as_of: "2026-09-03T10:00:00Z", authorized_by: "ops:karim", status: "authorized",
  },
  expect: "violated",
})

export const OtbPoBindingMatchesEnvelopeAndOrder = Norn.law("matches_envelope_and_order", {
  on: OtbPoBinding,
  must: (self) =>
    self.currency === self.envelope.currency
    && self.currency === self.order_line.currency
    && self.po_id === self.order.po_id
    && self.po_id === self.order_line.po_id
    && self.po_id === self.po_destination.po_id
    && self.sku_id === self.order_line.sku_id
    && self.sku_id === self.sku.sku_id
    && self.sku_id === self.sku_profile.sku_id
    && self.unit === self.order_line.unit
    && self.ordered_qty === self.order_line.ordered_qty
    && self.unit_cost === self.order_line.unit_price
    && self.received_qty <= self.order_line.received_qty
    && self.entity_id === self.envelope.entity_id
    && self.entity_id === self.po_destination.entity_id
    && self.store_id === self.envelope.store_id
    && self.store_id === self.po_destination.store_id
    && self.category === self.envelope.category
    && self.category === self.sku.category
    && self.brand === self.envelope.brand
    && self.brand === self.sku_profile.brand
    && self.plan_month === self.envelope.plan_month
    && self.plan_month === self.po_destination.plan_month
    && self.envelope_cap_snapshot === self.envelope.approved_budget_cost
    && (self.status !== "cancelled" || (self.order.status === "cancelled" && self.received_qty === self.order_line.received_qty))
    && (self.status !== "fulfilled" || self.received_qty === self.order_line.received_qty),
  because: "An OTB PO binding must derive its cost, SKU category/brand, and destination store/entity/month directly from governed supply facts and release remainder strictly when the PO is cancelled.",
})

// ------------------------------------------------ published forecast revisions

const PublishedForecastProps = Schema.Struct({
  forecast_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.String,
  model_version: Schema.String, // pinned ml.ModelVersion version_id
  forecast_revision: Schema.String, // pinned published forecast revision identifier
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_start: Norn.timestamp("utc"),
  published_at: Norn.timestamp("utc"),
  currency: Currency,
  unit: PlanUnit,
  forecast_sales_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  forecast_sales_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  forecast_sales_qty: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit"] }),
  forecast_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" }, additivity: { kind: "additive" } }),
  status: ForecastStatus,
})

/**
 * Governed forecast revision aligned to `PlanLine` grain, pinning both its
 * production `ml.ModelVersion` and explicit `forecast_revision` for
 * `Forecast - Plan` variance questions.
 */
export const PublishedForecast = Norn.type({
  name: "plan.PublishedForecast", title: "Published plan-grain forecast", version: "0.1.0",
  label: { en: "Published forecast", ar: "التوقع المنشور" },
  dependencies: CoreUnitsDep,
  properties: PublishedForecastProps,
  valueLabels: {
    unit: PlanUnitLabels,
    status: {
      draft: { en: "Draft", ar: "مسودة" },
      published: { en: "Published", ar: "منشور" },
      superseded: { en: "Superseded", ar: "مُستبدل" },
    },
  },
  needsReview: ["ar"],
  identity: [
    { name: "primary", properties: ["forecast_id"] },
    { name: "forecast_pin", properties: ["plan_line_id", "model_version", "forecast_revision"] },
  ],
  defaults: { status: "draft" },
  links: [
    { id: "scored_by", target: "ml.ModelVersion", cardinality: { to: "one", from: "many", required: true } },
    { id: "plan_version", target: PlanVersion, cardinality: { to: "one", from: "many", required: true } },
    { id: "plan_line", target: PlanLine, cardinality: { to: "one", from: "many", required: true } },
    { id: "owner_entity", target: LegalEntity, cardinality: { to: "one", from: "many", required: true } },
    { id: "store", target: Store, cardinality: { to: "one", from: "many", required: true } },
  ],
  lifecycles: [{
    property: "status",
    initial: ["draft"],
    terminal: ["superseded"],
    immutable: ["published", "superseded"],
    endings: { published: "resting", superseded: "completed" },
    transitions: [
      { id: "publish", from: ["draft"], to: "published" },
      { id: "supersede", from: ["published"], to: "superseded" },
    ],
  }],
})

export const PublishedForecastValid = Norn.law("forecast_valid", {
  on: PublishedForecast,
  must: (self) =>
    Norn.byteLength(self.plan_month) === 7
    && Norn.parseTimestamp(self.period_start) === Norn.parseTimestamp(self.plan_month + "-01T00:00:00Z")
    && self.forecast_sales_retail >= 0
    && self.forecast_sales_cost >= 0
    && self.forecast_sales_qty >= 0
    && self.forecast_fine_metal_g >= self.forecast_fine_metal_g - self.forecast_fine_metal_g,
  because: "A published plan-grain forecast carries non-negative predicted sales, cost, quantity, and fine-metal grams at a canonical YYYY-MM period_start.",
})
export const PublishedForecastValidHolds = Norn.example("a non-negative published forecast holds", {
  of: PublishedForecastValid,
  given: {
    forecast_id: "PF-1", plan_version_id: "PV-2026-V1", plan_line_id: "PL-1",
    model_version: "supply.demand_forecast@1", forecast_revision: "rev-2026-09-01",
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "gold_jewelry", brand: "nile_gold",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", published_at: "2026-09-01T08:00:00Z",
    currency: "EGP", unit: "g",
    forecast_sales_retail: 54000, forecast_sales_cost: 41000, forecast_sales_qty: 130,
    forecast_fine_metal_g: 132.25, status: "published",
  },
  expect: "holds",
})
export const PublishedForecastNegativeViolated = Norn.example("a negative forecast sales value is refused", {
  of: PublishedForecastValid,
  given: {
    forecast_id: "PF-BAD", plan_version_id: "PV-2026-V1", plan_line_id: "PL-1",
    model_version: "supply.demand_forecast@1", forecast_revision: "rev-2026-09-01",
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "gold_jewelry", brand: "nile_gold",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", published_at: "2026-09-01T08:00:00Z",
    currency: "EGP", unit: "g",
    forecast_sales_retail: -50, forecast_sales_cost: 0, forecast_sales_qty: 0,
    forecast_fine_metal_g: 0, status: "draft",
  },
  expect: "violated",
})

export const PublishedForecastMatchesPlan = Norn.law("matches_plan_line", {
  on: PublishedForecast,
  must: (self) =>
    self.plan_version_id === self.plan_line.plan_version_id
    && self.entity_id === self.plan_line.entity_id
    && self.store_id === self.plan_line.store_id
    && self.category === self.plan_line.category
    && self.brand === self.plan_line.brand
    && self.plan_month === self.plan_line.plan_month
    && self.currency === self.plan_line.currency
    && self.unit === self.plan_line.unit,
  because: "A published plan-grain forecast must align identically with its target PlanLine's version, entity, store, category, brand, month, currency, and unit.",
})
