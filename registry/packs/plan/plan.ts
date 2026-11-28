import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Commitment } from "../../core/src/rea.ts"
import {
  CountEvidence, OrderLine, PurchaseOrder, ReceiptEvidence,
} from "../supply-lab/supply.ts"
import { LegalEntity, SaleEvidence, Store } from "../supply-lab/retail.ts"

// Resource Planning Lab (S2 — Versioned Plan Pack & Open-to-Buy):
// Bitemporal, versioned merchandise planning (`PlanVersion`, `PlanSpreadTarget`,
// `PlanLine`), internal purchase-at-cost budget envelopes (`OtbEnvelope`),
// PO commitment bindings (`OtbPoBinding`), and published demand/sales forecasts
// (`PublishedForecast`) composed additively with `supply-lab` and `ml` — zero
// domain opcodes.
//
// Structural guarantees:
// - `PlanTarget` is `disjointWith` `core.Commitment` (budgets/envelopes are
//   internal targets; open `supply.OrderLine`s remain the commitments consuming
//   them) and `disjointWith` actual evidence interfaces (`CountEvidence`,
//   `ReceiptEvidence`, `SaleEvidence`), so plans can never share a type or flag
//   with actuals.
// - `PlanVersion` is bitemporal (D07) with lifecycle
//   `draft -> submitted -> approved -> locked -> superseded`, immutable once
//   `locked`; supersession links successor -> predecessor without mutating
//   locked history.
// - `PlanLine` lives strictly at `(entity x store x category x brand x ISO month)`
//   grain with additive flow measures (sales, receipts, markdowns, UCUM fine-metal
//   grams `#1794`) and semi-additive EOM stock (`METRIC-ADDITIVITY-1`,
//   `METRIC-UNIT-PARTITION-1`).
// - `PlanSpreadTarget` enforces exact top-down spread conservation (`LAW-LINK-MANY-1`)
//   with explicit decimal rounding remainder tracking (no silent drift).

const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const PlanUnit = Schema.Literals(["each", "kg", "l", "g"])
const SupplyUnit = Schema.Literals(["each", "kg", "l"])
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

// ------------------------------------------------ bitemporal PlanVersion (D07)

const PlanVersionProps = Schema.Struct({
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  plan_code: Schema.String,
  revision_number: Schema.Int,
  currency: Currency,
  // Bitemporal window (D07): effective horizon vs knowledge recording time.
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
 * Immutable once `locked`; superseding creates a new successor linking back
 * to the locked predecessor and transitions `locked -> superseded` without
 * modifying any locked property.
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
  currency: Currency,
  rounding_rule: RoundingRule,
  target_sales_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_sales_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_receipts_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_receipts_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_markdowns_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_markdowns_retail: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  target_eom_stock_cost: Norn.decimal(6, { additivity: { kind: "semi_additive" }, unitSource: ["currency"] }),
  // Fine-metal grams target via UCUM (#1794) for jewelry categories.
  target_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" }, additivity: { kind: "additive" } }),
  // Explicit decimal rounding remainder distributed to child lines (no silent rounding drift).
  target_rounding_remainder_cost: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["currency"] }),
  status: SpreadStatus,
})

/**
 * Top-down merchandise plan target at `(plan_version, entity, category, brand,
 * ISO month)` grain. Sealing requires exact conservation across all child
 * `PlanLine`s (including explicit decimal rounding remainder), preventing any
 * silent rounding drift.
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
    { id: "plan_version", target: PlanVersion, cardinality: { to: "one", from: "many", required: true } },
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

export const SpreadTargetValid = Norn.law("spread_target_valid", {
  on: PlanSpreadTarget,
  must: (self) =>
    Norn.byteLength(self.plan_month) === 7
    && self.target_sales_retail >= 0
    && self.target_sales_cost >= 0
    && self.target_receipts_cost >= 0
    && self.target_receipts_retail >= 0
    && self.target_markdowns_cost >= 0
    && self.target_markdowns_retail >= 0
    && self.target_eom_stock_cost >= 0
    && self.target_fine_metal_g >= self.target_fine_metal_g - self.target_fine_metal_g
    && (self.rounding_rule !== "exact_no_remainder" || self.target_rounding_remainder_cost === 0),
  because: "A top-down spread target carries non-negative planned measures, a canonical YYYY-MM month, and zero remainder when exact_no_remainder is declared.",
})
export const SpreadTargetValidHolds = Norn.example("a valid top-down spread target holds", {
  of: SpreadTargetValid,
  given: {
    spread_target_id: "SP-1", plan_version_id: "PV-2026-V1", entity_id: "LE-SOLE",
    category: "dry_goods", brand: "nile_foods", plan_month: "2026-09", currency: "EGP",
    rounding_rule: "largest_remainder", target_sales_retail: 10000, target_sales_cost: 6000,
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
    category: "dry_goods", brand: "nile_foods", plan_month: "2026-09", currency: "EGP",
    rounding_rule: "exact_no_remainder", target_sales_retail: 10000, target_sales_cost: 6000,
    target_receipts_cost: 10000.01, target_receipts_retail: 16000, target_markdowns_cost: 200,
    target_markdowns_retail: 350, target_eom_stock_cost: 5000, target_fine_metal_g: 0,
    target_rounding_remainder_cost: 0.01, status: "open",
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
  period_start: Norn.timestamp("utc"), // first UTC instant of the ISO month
  as_of: Norn.timestamp("utc"), // valid-time anchor for semi-additive EOM stock
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
  // Explicit exact decimal remainder attributed to this child line under top-down spread.
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
    && self.planned_sales_retail >= 0
    && self.planned_sales_cost >= 0
    && self.planned_sales_qty >= 0
    && self.planned_receipts_cost >= 0
    && self.planned_receipts_retail >= 0
    && self.planned_receipts_qty >= 0
    && self.planned_markdowns_cost >= 0
    && self.planned_markdowns_retail >= 0
    && self.planned_fine_metal_g >= self.planned_fine_metal_g - self.planned_fine_metal_g
    && self.planned_eom_stock_cost >= 0
    && self.planned_eom_stock_retail >= 0
    && self.planned_eom_stock_qty >= 0,
  because: "A plan line carries non-negative planned sales, receipts, markdowns, fine-metal mass, and EOM stock at a canonical YYYY-MM month.",
})
export const PlanLineValidHolds = Norn.example("a valid jewelry/retail plan line holds", {
  of: PlanLineMeasuresNonNegative,
  given: {
    plan_line_id: "PL-1", plan_version_id: "PV-2026-V1", spread_target_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "gold_jewelry", brand: "nile_gold",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", as_of: "2026-09-30T23:59:59Z",
    currency: "EGP", unit: "g",
    planned_sales_retail: 50000, planned_sales_cost: 38000, planned_sales_qty: 120,
    planned_receipts_cost: 40000, planned_receipts_retail: 52000, planned_receipts_qty: 125,
    planned_markdowns_cost: 500, planned_markdowns_retail: 800, planned_fine_metal_g: 125.5,
    spread_remainder_cost: 0, planned_eom_stock_cost: 15000, planned_eom_stock_retail: 20000,
    planned_eom_stock_qty: 40,
  },
  expect: "holds",
})
export const PlanLineNegativeReceiptsViolated = Norn.example("a negative planned receipts budget is refused", {
  of: PlanLineMeasuresNonNegative,
  given: {
    plan_line_id: "PL-BAD", plan_version_id: "PV-2026-V1", spread_target_id: null,
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
    plan_month: "2026-09", period_start: "2026-09-01T00:00:00Z", as_of: "2026-09-30T23:59:59Z",
    currency: "EGP", unit: "each",
    planned_sales_retail: 10000, planned_sales_cost: 6000, planned_sales_qty: 100,
    planned_receipts_cost: -100, planned_receipts_retail: 12000, planned_receipts_qty: 100,
    planned_markdowns_cost: 0, planned_markdowns_retail: 0, planned_fine_metal_g: 0,
    spread_remainder_cost: 0, planned_eom_stock_cost: 3000, planned_eom_stock_retail: 5000,
    planned_eom_stock_qty: 30,
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
    && self.currency === self.spread_target.currency,
  because: "A child plan line linked to a top-down spread target must match its version, entity, category, brand, month, and currency.",
})

// LAW-LINK-MANY-1: exact top-down spread conservation across child PlanLines.
export const SpreadTargetConservation = Norn.law("spread_conservation", {
  on: PlanSpreadTarget,
  inverses: { plan_lines: PlanLine },
  when: (self) => self.status === "sealed",
  must: (self) =>
    self.plan_lines.length > 0
    && Norn.sum(self.plan_lines.map((l) => l.planned_sales_retail)) === self.target_sales_retail
    && Norn.sum(self.plan_lines.map((l) => l.planned_sales_cost)) === self.target_sales_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_receipts_cost)) === self.target_receipts_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_receipts_retail)) === self.target_receipts_retail
    && Norn.sum(self.plan_lines.map((l) => l.planned_markdowns_cost)) === self.target_markdowns_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_markdowns_retail)) === self.target_markdowns_retail
    && Norn.sum(self.plan_lines.map((l) => l.planned_eom_stock_cost)) === self.target_eom_stock_cost
    && Norn.sum(self.plan_lines.map((l) => l.planned_fine_metal_g)) === self.target_fine_metal_g
    && Norn.sum(self.plan_lines.map((l) => l.spread_remainder_cost)) === self.target_rounding_remainder_cost,
  because: "Child plan lines under a sealed top-down spread target must sum exactly to every parent measure and explicit rounding remainder with zero silent drift.",
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
    && self.approved_budget_cost >= 0
    && (
      (self.envelope_kind === "planned" && self.plan_line_id !== null)
      || (self.envelope_kind === "unplanned" && self.plan_line_id === null && self.override_reason !== null && self.approved_budget_cost > 0)
    ),
  because: "A planned OTB envelope binds a plan line; an unplanned envelope carries null plan_line_id, an explicit override reason, and a positive override budget.",
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
  // cancellations release unreceived -> cancelled_released_cost.
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
 * at purchase-at-cost, conserving OTB across partial receipts and releasing
 * unreceived value on PO cancellation while tracking expected cash payment
 * timing separately.
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
  protected: ["authorized_by"],
  defaults: { status: "authorized" },
  links: [
    { id: "envelope", target: OtbEnvelope, cardinality: { to: "one", from: "many", required: true }, inverse: "bindings" },
    { id: "order", target: PurchaseOrder, cardinality: { to: "one", from: "many", required: true } },
    { id: "order_line", target: OrderLine, cardinality: { to: "one", from: "one", required: true } },
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
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
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
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
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
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
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
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
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
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "dry_goods", brand: "nile_foods",
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
    entity_id: "LE-SOLE", store_id: "ST-CFC", category: "spares", brand: "none",
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
    && self.po_id === self.order_line.po_id
    && self.unit === self.order_line.unit
    && self.ordered_qty === self.order_line.ordered_qty
    && self.received_qty <= self.order_line.received_qty
    && self.entity_id === self.envelope.entity_id
    && self.store_id === self.envelope.store_id
    && self.category === self.envelope.category
    && self.brand === self.envelope.brand
    && self.plan_month === self.envelope.plan_month
    && self.envelope_cap_snapshot === self.envelope.approved_budget_cost,
  because: "An OTB PO binding must match its governing envelope's grain, currency, and approved budget cap, and its underlying supply.OrderLine's ordered quantity, unit, and currency.",
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
    && self.forecast_sales_retail >= 0
    && self.forecast_sales_cost >= 0
    && self.forecast_sales_qty >= 0
    && self.forecast_fine_metal_g >= self.forecast_fine_metal_g - self.forecast_fine_metal_g,
  because: "A published plan-grain forecast carries non-negative predicted sales, cost, quantity, and fine-metal grams.",
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
