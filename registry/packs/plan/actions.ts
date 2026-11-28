import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { OrderLine, PurchaseOrder, ShipmentReceipt, Sku } from "../supply-lab/supply.ts"
import { LegalEntity, PosSale, Store, UnitCost } from "../supply-lab/retail.ts"
import {
  ActivePlanLine, ActualPlanVariance, OtbEnvelope, OtbPoBinding, PlanLine, PlanSpreadTarget, PlanVersion,
  PoDestination, PublishedForecast, SkuProfile,
} from "./plan.ts"

// Governed writers and access rules for the S2 Plan & Open-to-Buy pack.
// Every server-derived input field (`fromCaller` / `commitTime` / `inputRequired: false`)
// obeys ORIGIN-DATAFLOW-1: callers omit them and the kernel derives/stamps them.

const Scope = ["orgId", "environmentId"] as const
const Access = { artifactId: "plan.access.policy", version: "0.1.0" } as const
const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const PlanUnit = Schema.Literals(["each", "kg", "l", "g"])
const SupplyUnit = Schema.Literals(["each", "kg", "l"])
const SupplyCategory = Schema.Literals(["dry_goods", "chilled", "pharma", "spares"])
const RoundingRule = Schema.Literals(["largest_remainder", "last_bucket_remainder", "exact_no_remainder"])
const EnvelopeKind = Schema.Literals(["planned", "unplanned"])
const CoreUnitsDep = [{ artifactId: "core.units", version: "1.0.0", kind: "entity" as const }]

// ----------------------------------------- governed SKU & PO grain writers

const SkuProfileInput = Schema.Struct({
  sku_id: Schema.String,
  category: SupplyCategory,
  brand: Schema.String,
  unit_of_measure: SupplyUnit,
})
const SkuProfileRef = Schema.Struct({ sku_id: Schema.String })

/** Register a governed SKU merchandise profile (brand + catalog category/unit). */
export const RegisterSkuProfile = Norn.action({
  id: "plan.sku_profile.register", title: "Register a SKU merchandise profile", version: "0.1.0",
  label: { en: "Register SKU profile", ar: "تسجيل ملف الصنف التجاري" },
  input: SkuProfileInput, output: SkuProfileRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
  ],
  derive: [
    {
      field: "category", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ sku: Schema.Struct({ category: SupplyCategory }) }) }),
        (ctx) => ctx.slice.sku.category,
        { root: "context" },
      ),
    },
    {
      field: "unit_of_measure", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ sku: Schema.Struct({ unit_of_measure: SupplyUnit }) }) }),
        (ctx) => ctx.slice.sku.unit_of_measure,
        { root: "context" },
      ),
    },
  ],
  createLinks: [
    { link: "sku", targetType: Sku, targetIdField: "sku_id" },
  ],
  writes: [{ type: SkuProfile, change: "created" }],
  effect: { kind: "upsert_input_object", type: SkuProfile, objectIdField: "sku_id", writeMode: { kind: "create_only" } },
})

const PoDestinationInput = Schema.Struct({
  po_id: Schema.String,
  store_id: Schema.String,
  entity_id: Schema.String,
  plan_month: Schema.String,
  period_month_date: Norn.date(),
  period_start: Norn.timestamp("utc"),
  currency: Currency,
})
const PoDestinationRef = Schema.Struct({ po_id: Schema.String })

/** Bind a draft `supply.PurchaseOrder` to its governed destination store, entity, and plan month. */
export const AssignPoDestination = Norn.action({
  id: "plan.po_destination.assign", title: "Assign a purchase order destination grain", version: "0.1.0",
  label: { en: "Assign PO destination", ar: "تعيين وجهة أمر الشراء" },
  input: PoDestinationInput, output: PoDestinationRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
  ],
  derive: [
    {
      field: "period_month_date", inputRequired: false,
      expression: Norn.expression(PoDestinationInput, (x) => Norn.parseDate(x.plan_month + "-01")),
    },
    {
      field: "period_start", inputRequired: false,
      expression: Norn.expression(PoDestinationInput, (x) => Norn.parseTimestamp(x.plan_month + "-01T00:00:00Z")),
    },
    {
      field: "currency", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order: Schema.Struct({ currency: Currency }) }) }),
        (ctx) => ctx.slice.order.currency,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PoDestinationInput,
      slice: Schema.Struct({
        order: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
      }),
    }),
    (ctx) =>
      ctx.slice.order.status === "draft"
      && ctx.slice.store.status === "active"
      && ctx.slice.owner_entity.status === "active"
      && ctx.input.entity_id === ctx.slice.store.entity_id
      && ctx.slice.order.currency === ctx.slice.store.currency
      && ctx.slice.order.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "order", targetType: PurchaseOrder, targetIdField: "po_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
  ],
  writes: [{ type: PoDestination, change: "created" }],
  effect: { kind: "upsert_input_object", type: PoDestination, objectIdField: "po_id", writeMode: { kind: "create_only" } },
})

// ------------------------------------------------ PlanVersion lifecycle writers

const CreatePlanVersionInput = Schema.Struct({
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  plan_code: Schema.String,
  revision_number: Schema.Int,
  currency: Currency,
  effective_from: Norn.timestamp("utc"),
  effective_to: Norn.timestamp("utc"),
  recorded_at: Norn.timestamp("utc"),
  created_by: Schema.String,
})
const CreateRevisionInput = Schema.Struct({
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  plan_code: Schema.String,
  revision_number: Schema.Int,
  currency: Currency,
  effective_from: Norn.timestamp("utc"),
  effective_to: Norn.timestamp("utc"),
  recorded_at: Norn.timestamp("utc"),
  supersedes_version_id: Schema.String,
  created_by: Schema.String,
})
const UpdatePlanVersionDraftInput = Schema.Struct({
  plan_version_id: Schema.String,
  effective_from: Norn.timestamp("utc"),
  effective_to: Norn.timestamp("utc"),
  recorded_at: Norn.timestamp("utc"),
})
const PlanVersionRef = Schema.Struct({ plan_version_id: Schema.String })
const GovernPlanVersionInput = Schema.Struct({
  plan_version_id: Schema.String,
  sealed_status: Schema.String,
})
const SupersedePlanVersionInput = Schema.Struct({
  plan_version_id: Schema.String,
  successor_version_id: Schema.String,
})

/** Create an initial draft PlanVersion (revision 1, authenticated commit clock). */
export const CreatePlanVersion = Norn.action({
  id: "plan.version.create", title: "Create a draft plan version", version: "0.1.0",
  label: { en: "Create plan version", ar: "إنشاء إصدار خطة" },
  input: CreatePlanVersionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["created_by"],
  derive: [
    { field: "recorded_at", inputRequired: false, expression: Norn.commitTime() },
  ],
  slice: [
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: CreatePlanVersionInput,
      slice: Schema.Struct({ owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }) }),
    }),
    (ctx) =>
      ctx.slice.owner_entity.status === "active"
      && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
  ],
  writes: [{ type: PlanVersion, change: "created" }],
  effect: { kind: "upsert_input_object", type: PlanVersion, objectIdField: "plan_version_id", writeMode: { kind: "create_only" } },
})

/**
 * Create a new draft PlanVersion that links cleanly to a prior `locked`
 * PlanVersion without mutating locked history, stamping authenticated commit time.
 */
export const CreateSupersedingPlanVersion = Norn.action({
  id: "plan.version.create_revision", title: "Create a superseding draft plan version", version: "0.1.0",
  label: { en: "Create superseding plan version", ar: "إنشاء مراجعة جديدة للخطة" },
  input: CreateRevisionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["created_by"],
  derive: [
    { field: "recorded_at", inputRequired: false, expression: Norn.commitTime() },
  ],
  slice: [
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "prior", type: PlanVersion, key: { property: { from: "input", property: "supersedes_version_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: CreateRevisionInput,
      slice: Schema.Struct({
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
        prior: Schema.Struct({
          status: Schema.String, entity_id: Schema.String, plan_code: Schema.String,
          currency: Schema.String, revision_number: Schema.Int,
        }),
      }),
    }),
    (ctx) =>
      ctx.slice.owner_entity.status === "active"
      && ctx.input.currency === ctx.slice.owner_entity.functional_currency
      && ctx.slice.prior.status === "locked"
      && ctx.input.entity_id === ctx.slice.prior.entity_id
      && ctx.input.plan_code === ctx.slice.prior.plan_code
      && ctx.input.currency === ctx.slice.prior.currency
      && ctx.input.revision_number > ctx.slice.prior.revision_number,
    { root: "context" },
  )],
  createLinks: [
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "supersedes", targetType: PlanVersion, targetIdField: "supersedes_version_id" },
  ],
  writes: [{ type: PlanVersion, change: "created" }],
  effect: { kind: "upsert_input_object", type: PlanVersion, objectIdField: "plan_version_id", writeMode: { kind: "create_only" } },
})

/**
 * Update bitemporal window metadata strictly while `PlanVersion.status === "draft"`
 * (`writeMode: { kind: "draft_only", lifecycle: "status", state: "draft" }`):
 * refuses post-submit, post-approve, post-lock, and post-supersede mutations.
 */
export const UpdatePlanVersionDraft = Norn.action({
  id: "plan.version.update_draft", title: "Update a draft plan version", version: "0.1.0",
  label: { en: "Update draft plan version", ar: "تحديث مسودة الخطة" },
  input: UpdatePlanVersionDraftInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  derive: [
    { field: "recorded_at", inputRequired: false, expression: Norn.commitTime() },
  ],
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: {
    kind: "upsert_input_object", type: PlanVersion, objectIdField: "plan_version_id",
    writeMode: { kind: "draft_only", lifecycle: "status", state: "draft" },
  },
})

export const SubmitPlanVersion = Norn.action({
  id: "plan.version.submit", title: "Submit a plan version for approval", version: "0.1.0",
  label: { en: "Submit plan version", ar: "تقديم الخطة للاعتماد" },
  input: PlanVersionRef, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanVersion, objectIdField: "plan_version_id", lifecycle: "status", transition: "submit" },
})

/**
 * Separation of duties (`independentOf: ["created_by"]`) + fenced spread seal
 * check (`total_spreads === sealed_spreads` + `PlanVersionSpreadsSealed` law).
 */
export const ApprovePlanVersion = Norn.action({
  id: "plan.version.approve", title: "Approve a submitted plan version", version: "0.1.0",
  label: { en: "Approve plan version", ar: "اعتماد إصدار الخطة" },
  input: GovernPlanVersionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  derive: [
    { field: "sealed_status", inputRequired: false, expression: Norn.expression(GovernPlanVersionInput, () => "sealed") },
  ],
  aggregates: [
    { name: "total_spreads", type: PlanSpreadTarget, filter: { plan_version_id: "plan_version_id" }, aggregate: { kind: "count" }, maxItems: 100 },
    { name: "sealed_spreads", type: PlanSpreadTarget, filter: { plan_version_id: "plan_version_id", status: "sealed_status" }, aggregate: { kind: "count" }, maxItems: 100 },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      aggregates: Schema.Struct({ total_spreads: Schema.Int, sealed_spreads: Schema.Int }),
    }),
    (ctx) => ctx.aggregates.total_spreads === ctx.aggregates.sealed_spreads,
    { root: "context" },
  )],
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanVersion, objectIdField: "plan_version_id", lifecycle: "status", transition: "approve" },
  independentOf: ["created_by"],
})

/** Lock an approved PlanVersion once all spreads remain sealed and conserved. */
export const LockPlanVersion = Norn.action({
  id: "plan.version.lock", title: "Lock an approved plan version", version: "0.1.0",
  label: { en: "Lock plan version", ar: "إقفال إصدار الخطة" },
  input: GovernPlanVersionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  derive: [
    { field: "sealed_status", inputRequired: false, expression: Norn.expression(GovernPlanVersionInput, () => "sealed") },
  ],
  aggregates: [
    { name: "total_spreads", type: PlanSpreadTarget, filter: { plan_version_id: "plan_version_id" }, aggregate: { kind: "count" }, maxItems: 100 },
    { name: "sealed_spreads", type: PlanSpreadTarget, filter: { plan_version_id: "plan_version_id", status: "sealed_status" }, aggregate: { kind: "count" }, maxItems: 100 },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      aggregates: Schema.Struct({ total_spreads: Schema.Int, sealed_spreads: Schema.Int }),
    }),
    (ctx) => ctx.aggregates.total_spreads === ctx.aggregates.sealed_spreads,
    { root: "context" },
  )],
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanVersion, objectIdField: "plan_version_id", lifecycle: "status", transition: "lock" },
})

/**
 * Supersede a locked PlanVersion once its successor has been approved or locked,
 * transitioning `locked -> superseded` without mutating any locked field.
 */
export const SupersedePlanVersion = Norn.action({
  id: "plan.version.supersede", title: "Supersede a locked plan version", version: "0.1.0",
  label: { en: "Supersede plan version", ar: "استبدال إصدار الخطة" },
  input: SupersedePlanVersionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "successor", type: PlanVersion, key: { property: { from: "input", property: "successor_version_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: SupersedePlanVersionInput,
      slice: Schema.Struct({
        successor: Schema.Struct({
          status: Schema.String,
          supersedes_version_id: Schema.NullOr(Schema.String),
        }),
      }),
    }),
    (ctx) =>
      (ctx.slice.successor.status === "approved" || ctx.slice.successor.status === "locked")
      && (ctx.slice.successor.supersedes_version_id ?? "") === ctx.input.plan_version_id,
    { root: "context" },
  )],
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanVersion, objectIdField: "plan_version_id", lifecycle: "status", transition: "supersede" },
})

// ------------------------------------ top-down spread targets & leaf PlanLines

const SpreadTargetInput = Schema.Struct({
  spread_target_id: Schema.String,
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_month_date: Norn.date(),
  period_start: Norn.timestamp("utc"),
  currency: Currency,
  rounding_rule: RoundingRule,
  bucket_count: Schema.Int,
  allocation_quantum_cost: Norn.decimal(6),
  target_total_quanta: Schema.Int,
  target_total_weight: Schema.Int,
  target_remainder_quanta: Schema.Int,
  remainder_cutoff_frac: Schema.Int,
  remainder_cutoff_bucket: Schema.Int,
  target_sales_retail: Norn.decimal(6),
  target_sales_cost: Norn.decimal(6),
  target_receipts_cost: Norn.decimal(6),
  target_receipts_retail: Norn.decimal(6),
  target_markdowns_cost: Norn.decimal(6),
  target_markdowns_retail: Norn.decimal(6),
  target_eom_stock_cost: Norn.decimal(6),
  target_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }),
  target_rounding_remainder_cost: Norn.decimal(6),
})
const SpreadTargetRef = Schema.Struct({ spread_target_id: Schema.String })

export const CreateSpreadTarget = Norn.action({
  id: "plan.spread_target.create", title: "Create a top-down plan spread target", version: "0.1.0",
  label: { en: "Create spread target", ar: "إنشاء هدف توزيع الخطة" },
  dependencies: CoreUnitsDep,
  input: SpreadTargetInput, output: SpreadTargetRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
  ],
  derive: [
    {
      field: "period_month_date", inputRequired: false,
      expression: Norn.expression(SpreadTargetInput, (x) => Norn.parseDate(x.plan_month + "-01")),
    },
    {
      field: "period_start", inputRequired: false,
      expression: Norn.expression(SpreadTargetInput, (x) => Norn.parseTimestamp(x.plan_month + "-01T00:00:00Z")),
    },
    {
      field: "target_rounding_remainder_cost", inputRequired: false,
      expression: Norn.expression(SpreadTargetInput, (x) => x.target_remainder_quanta * x.allocation_quantum_cost),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: SpreadTargetInput,
      slice: Schema.Struct({
        version: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
      }),
    }),
    (ctx) =>
      ctx.slice.version.status === "draft"
      && ctx.slice.owner_entity.status === "active"
      && ctx.input.entity_id === ctx.slice.version.entity_id
      && ctx.input.currency === ctx.slice.version.currency
      && ctx.input.currency === ctx.slice.owner_entity.functional_currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "plan_version", targetType: PlanVersion, targetIdField: "plan_version_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
  ],
  writes: [{ type: PlanSpreadTarget, change: "created" }],
  effect: { kind: "upsert_input_object", type: PlanSpreadTarget, objectIdField: "spread_target_id", writeMode: { kind: "create_only" } },
})

/**
 * Seal a top-down spread target once its child `PlanLine`s cover every distinct
 * bucket `1..bucket_count`, conserve total spread weights and remainder quanta,
 * and sum exactly to every parent measure (`TXN-PHANTOM` fenced via `aggregates`
 * + `SpreadTargetConservation`).
 */
export const SealSpreadTarget = Norn.action({
  id: "plan.spread_target.seal", title: "Seal a top-down plan spread target", version: "0.1.0",
  label: { en: "Seal spread target", ar: "ختم هدف توزيع الخطة" },
  dependencies: CoreUnitsDep,
  input: SpreadTargetRef, output: SpreadTargetRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "target", type: PlanSpreadTarget, key: { property: { from: "input", property: "spread_target_id" } } },
  ],
  aggregates: [
    { name: "child_count", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "count" }, maxItems: 50 },
    { name: "sum_base_receipts_cost", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "spread_base_receipts_cost" }, maxItems: 50 },
    { name: "sum_sales_retail", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_sales_retail" }, maxItems: 50 },
    { name: "sum_sales_cost", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_sales_cost" }, maxItems: 50 },
    { name: "sum_receipts_cost", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_receipts_cost" }, maxItems: 50 },
    { name: "sum_receipts_retail", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_receipts_retail" }, maxItems: 50 },
    { name: "sum_markdowns_cost", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_markdowns_cost" }, maxItems: 50 },
    { name: "sum_markdowns_retail", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_markdowns_retail" }, maxItems: 50 },
    { name: "sum_eom_stock_cost", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_eom_stock_cost" }, maxItems: 50 },
    { name: "sum_fine_metal_g", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "planned_fine_metal_g" }, maxItems: 50 },
    { name: "sum_remainder_cost", type: PlanLine, filter: { spread_target_id: "spread_target_id" }, aggregate: { kind: "sum", property: "spread_remainder_cost" }, maxItems: 50 },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      slice: Schema.Struct({
        target: Schema.Struct({
          bucket_count: Schema.Int,
          target_sales_retail: Norn.decimal(6),
          target_sales_cost: Norn.decimal(6),
          target_receipts_cost: Norn.decimal(6),
          target_receipts_retail: Norn.decimal(6),
          target_markdowns_cost: Norn.decimal(6),
          target_markdowns_retail: Norn.decimal(6),
          target_eom_stock_cost: Norn.decimal(6),
          target_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }),
          target_rounding_remainder_cost: Norn.decimal(6),
        }),
      }),
      aggregates: Schema.Struct({
        child_count: Schema.Int,
        sum_base_receipts_cost: Norn.decimal(6),
        sum_sales_retail: Norn.decimal(6),
        sum_sales_cost: Norn.decimal(6),
        sum_receipts_cost: Norn.decimal(6),
        sum_receipts_retail: Norn.decimal(6),
        sum_markdowns_cost: Norn.decimal(6),
        sum_markdowns_retail: Norn.decimal(6),
        sum_eom_stock_cost: Norn.decimal(6),
        sum_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }),
        sum_remainder_cost: Norn.decimal(6),
      }),
    }),
    (ctx) =>
      ctx.aggregates.child_count === ctx.slice.target.bucket_count
      && (ctx.aggregates.sum_base_receipts_cost + ctx.slice.target.target_rounding_remainder_cost) === ctx.slice.target.target_receipts_cost
      && ctx.aggregates.sum_sales_retail === ctx.slice.target.target_sales_retail
      && ctx.aggregates.sum_sales_cost === ctx.slice.target.target_sales_cost
      && ctx.aggregates.sum_receipts_cost === ctx.slice.target.target_receipts_cost
      && ctx.aggregates.sum_receipts_retail === ctx.slice.target.target_receipts_retail
      && ctx.aggregates.sum_markdowns_cost === ctx.slice.target.target_markdowns_cost
      && ctx.aggregates.sum_markdowns_retail === ctx.slice.target.target_markdowns_retail
      && ctx.aggregates.sum_eom_stock_cost === ctx.slice.target.target_eom_stock_cost
      && ctx.aggregates.sum_fine_metal_g === ctx.slice.target.target_fine_metal_g
      && ctx.aggregates.sum_remainder_cost === ctx.slice.target.target_rounding_remainder_cost,
    { root: "context" },
  )],
  writes: [{ type: PlanSpreadTarget, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanSpreadTarget, objectIdField: "spread_target_id", lifecycle: "status", transition: "seal" },
})

const StandalonePlanLineInput = Schema.Struct({
  plan_line_id: Schema.String,
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_month_date: Norn.date(),
  period_start: Norn.timestamp("utc"),
  as_of: Norn.timestamp("utc"),
  currency: Currency,
  unit: PlanUnit,
  planned_sales_retail: Norn.decimal(6),
  planned_sales_cost: Norn.decimal(6),
  planned_sales_qty: Norn.decimal(6),
  planned_receipts_cost: Norn.decimal(6),
  planned_receipts_retail: Norn.decimal(6),
  planned_receipts_qty: Norn.decimal(6),
  planned_markdowns_cost: Norn.decimal(6),
  planned_markdowns_retail: Norn.decimal(6),
  planned_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }),
  spread_bucket_index: Schema.Int,
  spread_weight: Schema.Int,
  spread_base_quanta: Schema.Int,
  spread_frac_remainder: Schema.Int,
  spread_remainder_quanta: Schema.Int,
  spread_base_receipts_cost: Norn.decimal(6),
  spread_remainder_cost: Norn.decimal(6),
  planned_eom_stock_cost: Norn.decimal(6),
  planned_eom_stock_retail: Norn.decimal(6),
  planned_eom_stock_qty: Norn.decimal(6),
})
const SpreadPlanLineInput = Schema.Struct({
  plan_line_id: Schema.String,
  plan_version_id: Schema.String,
  spread_target_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_month_date: Norn.date(),
  period_start: Norn.timestamp("utc"),
  as_of: Norn.timestamp("utc"),
  currency: Currency,
  unit: PlanUnit,
  planned_sales_retail: Norn.decimal(6),
  planned_sales_cost: Norn.decimal(6),
  planned_sales_qty: Norn.decimal(6),
  planned_receipts_cost: Norn.decimal(6),
  planned_receipts_retail: Norn.decimal(6),
  planned_receipts_qty: Norn.decimal(6),
  planned_markdowns_cost: Norn.decimal(6),
  planned_markdowns_retail: Norn.decimal(6),
  planned_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }),
  spread_bucket_index: Schema.Int,
  spread_weight: Schema.Int,
  spread_base_quanta: Schema.Int,
  spread_frac_remainder: Schema.Int,
  spread_remainder_quanta: Schema.Int,
  spread_base_receipts_cost: Norn.decimal(6),
  spread_remainder_cost: Norn.decimal(6),
  planned_eom_stock_cost: Norn.decimal(6),
  planned_eom_stock_retail: Norn.decimal(6),
  planned_eom_stock_qty: Norn.decimal(6),
})
const PlanLineRef = Schema.Struct({ plan_line_id: Schema.String })

/** Record a bottom-up leaf PlanLine into a draft PlanVersion. */
export const RecordPlanLine = Norn.action({
  id: "plan.line.record", title: "Record a merchandise plan line", version: "0.1.0",
  label: { en: "Record plan line", ar: "تسجيل سطر خطة" },
  dependencies: CoreUnitsDep,
  input: StandalonePlanLineInput, output: PlanLineRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  derive: [
    { field: "period_month_date", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, (x) => Norn.parseDate(x.plan_month + "-01")) },
    { field: "spread_bucket_index", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, () => 0) },
    { field: "spread_weight", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, () => 0) },
    { field: "spread_base_quanta", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, () => 0) },
    { field: "spread_frac_remainder", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, () => 0) },
    { field: "spread_remainder_quanta", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, () => 0) },
    { field: "spread_base_receipts_cost", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, (x) => x.planned_receipts_cost) },
    { field: "spread_remainder_cost", inputRequired: false, expression: Norn.expression(StandalonePlanLineInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: StandalonePlanLineInput,
      slice: Schema.Struct({
        version: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String, functional_currency: Schema.String }),
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
      }),
    }),
    (ctx) =>
      ctx.slice.version.status === "draft"
      && ctx.slice.owner_entity.status === "active"
      && ctx.slice.store.status === "active"
      && ctx.input.entity_id === ctx.slice.version.entity_id
      && ctx.input.entity_id === ctx.slice.store.entity_id
      && ctx.input.currency === ctx.slice.version.currency
      && ctx.input.currency === ctx.slice.store.currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "plan_version", targetType: PlanVersion, targetIdField: "plan_version_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: PlanLine, change: "created" }],
  effect: { kind: "upsert_input_object", type: PlanLine, objectIdField: "plan_line_id", writeMode: { kind: "create_only" } },
})

/** Record a child leaf PlanLine spread from an open PlanSpreadTarget, enforcing unique bucket_index per target. */
export const RecordSpreadPlanLine = Norn.action({
  id: "plan.line.record_spread", title: "Record a top-down spread plan line", version: "0.1.0",
  label: { en: "Record spread plan line", ar: "تسجيل سطر خطة موزع" },
  dependencies: CoreUnitsDep,
  input: SpreadPlanLineInput, output: PlanLineRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "spread_target", type: PlanSpreadTarget, key: { property: { from: "input", property: "spread_target_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  aggregates: [
    {
      name: "existing_bucket_count",
      type: PlanLine,
      filter: { spread_target_id: "spread_target_id", spread_bucket_index: "spread_bucket_index" },
      aggregate: { kind: "count" },
      maxItems: 10,
    },
  ],
  derive: [
    { field: "period_month_date", inputRequired: false, expression: Norn.expression(SpreadPlanLineInput, (x) => Norn.parseDate(x.plan_month + "-01")) },
    {
      field: "spread_frac_remainder", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ spread_weight: Schema.Int, spread_base_quanta: Schema.Int }),
          slice: Schema.Struct({ spread_target: Schema.Struct({ target_total_quanta: Schema.Int, target_total_weight: Schema.Int }) }),
        }),
        (ctx) => (ctx.slice.spread_target.target_total_quanta * ctx.input.spread_weight) - (ctx.input.spread_base_quanta * ctx.slice.spread_target.target_total_weight),
        { root: "context" },
      ),
    },
    {
      field: "spread_base_receipts_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ spread_base_quanta: Schema.Int }),
          slice: Schema.Struct({ spread_target: Schema.Struct({ allocation_quantum_cost: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.spread_base_quanta * ctx.slice.spread_target.allocation_quantum_cost,
        { root: "context" },
      ),
    },
    {
      field: "spread_remainder_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ spread_remainder_quanta: Schema.Int }),
          slice: Schema.Struct({ spread_target: Schema.Struct({ allocation_quantum_cost: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.spread_remainder_quanta * ctx.slice.spread_target.allocation_quantum_cost,
        { root: "context" },
      ),
    },
    {
      field: "planned_receipts_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ spread_base_quanta: Schema.Int, spread_remainder_quanta: Schema.Int }),
          slice: Schema.Struct({ spread_target: Schema.Struct({ allocation_quantum_cost: Norn.decimal(6) }) }),
        }),
        (ctx) => (ctx.input.spread_base_quanta + ctx.input.spread_remainder_quanta) * ctx.slice.spread_target.allocation_quantum_cost,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: SpreadPlanLineInput,
      slice: Schema.Struct({
        version: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        spread_target: Schema.Struct({
          status: Schema.String, plan_version_id: Schema.String, entity_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String, currency: Schema.String,
        }),
        owner_entity: Schema.Struct({ status: Schema.String }),
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
      }),
      aggregates: Schema.Struct({ existing_bucket_count: Schema.Int }),
    }),
    (ctx) =>
      ctx.aggregates.existing_bucket_count === 0
      && ctx.slice.version.status === "draft"
      && ctx.slice.spread_target.status === "open"
      && ctx.slice.owner_entity.status === "active"
      && ctx.slice.store.status === "active"
      && ctx.input.plan_version_id === ctx.slice.spread_target.plan_version_id
      && ctx.input.entity_id === ctx.slice.spread_target.entity_id
      && ctx.input.category === ctx.slice.spread_target.category
      && ctx.input.brand === ctx.slice.spread_target.brand
      && ctx.input.plan_month === ctx.slice.spread_target.plan_month
      && ctx.input.currency === ctx.slice.spread_target.currency
      && ctx.input.entity_id === ctx.slice.store.entity_id
      && ctx.input.currency === ctx.slice.store.currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "plan_version", targetType: PlanVersion, targetIdField: "plan_version_id" },
    { link: "spread_target", targetType: PlanSpreadTarget, targetIdField: "spread_target_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: PlanLine, change: "created" }],
  effect: { kind: "upsert_input_object", type: PlanLine, objectIdField: "plan_line_id", writeMode: { kind: "create_only" } },
})

// ------------------------------------ ActivePlanLine baseline + PosSale restate

const ActiveBaselineInput = Schema.Struct({
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
const BaselineRef = Schema.Struct({ slot_id: Schema.String })

/** Activate a locked PlanLine as the active baseline for `Actual - Plan` variance. */
export const ActivatePlanBaseline = Norn.action({
  id: "plan.baseline.activate", title: "Activate a locked plan line baseline", version: "0.1.0",
  label: { en: "Activate plan baseline", ar: "تفعيل سطر الخطة المعتمد" },
  input: ActiveBaselineInput, output: BaselineRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "plan_line", type: PlanLine, key: { property: { from: "input", property: "plan_line_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  derive: [
    { field: "brand", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ brand: Schema.String }) }) }), (ctx) => ctx.slice.plan_line.brand, { root: "context" }) },
    { field: "plan_month", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ plan_month: Schema.String }) }) }), (ctx) => ctx.slice.plan_line.plan_month, { root: "context" }) },
    { field: "period_month_date", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ period_month_date: Norn.date() }) }) }), (ctx) => ctx.slice.plan_line.period_month_date, { root: "context" }) },
    { field: "currency", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ currency: Currency }) }) }), (ctx) => ctx.slice.plan_line.currency, { root: "context" }) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: ActiveBaselineInput,
      slice: Schema.Struct({
        version: Schema.Struct({ status: Schema.String }),
        plan_line: Schema.Struct({
          plan_version_id: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, unit: Schema.String,
        }),
      }),
    }),
    (ctx) =>
      ctx.slice.version.status === "locked"
      && ctx.input.plan_version_id === ctx.slice.plan_line.plan_version_id
      && ctx.input.entity_id === ctx.slice.plan_line.entity_id
      && ctx.input.store_id === ctx.slice.plan_line.store_id
      && ctx.input.category === ctx.slice.plan_line.category
      && ctx.input.unit === ctx.slice.plan_line.unit,
    { root: "context" },
  )],
  createLinks: [
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: ActivePlanLine, change: "created" }],
  effect: { kind: "upsert_input_object", type: ActivePlanLine, objectIdField: "slot_id", writeMode: { kind: "create_only" } },
})

const SupersedeBaselineInput = Schema.Struct({
  slot_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.String,
})

/**
 * Re-point an existing `plan.ActivePlanLine` slot to a newly `locked` successor
 * `PlanVersion` & `PlanLine` at the exact same merchandise grain, allowing
 * clean supersession without blocking `plan.version.supersede` or duplicating
 * actuals across revisions.
 */
export const SupersedePlanBaseline = Norn.action({
  id: "plan.baseline.supersede", title: "Supersede an active plan baseline slot", version: "0.1.0",
  label: { en: "Supersede plan baseline", ar: "استبدال سطر الخطة النشط" },
  input: SupersedeBaselineInput, output: BaselineRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "existing", type: ActivePlanLine, key: { property: { from: "input", property: "slot_id" } } },
    { name: "new_version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "new_plan_line", type: PlanLine, key: { property: { from: "input", property: "plan_line_id" } } },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: SupersedeBaselineInput,
      slice: Schema.Struct({
        existing: Schema.Struct({
          plan_version_id: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String,
          period_month_date: Norn.date(), currency: Schema.String, unit: Schema.String,
        }),
        new_version: Schema.Struct({
          status: Schema.String,
          supersedes_version_id: Schema.NullOr(Schema.String),
        }),
        new_plan_line: Schema.Struct({
          plan_version_id: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String,
          period_month_date: Norn.date(), currency: Schema.String, unit: Schema.String,
        }),
      }),
    }),
    (ctx) =>
      ctx.slice.new_version.status === "locked"
      && (ctx.slice.new_version.supersedes_version_id ?? "") === ctx.slice.existing.plan_version_id
      && ctx.input.plan_version_id === ctx.slice.new_plan_line.plan_version_id
      && ctx.slice.existing.entity_id === ctx.slice.new_plan_line.entity_id
      && ctx.slice.existing.store_id === ctx.slice.new_plan_line.store_id
      && ctx.slice.existing.category === ctx.slice.new_plan_line.category
      && ctx.slice.existing.brand === ctx.slice.new_plan_line.brand
      && ctx.slice.existing.plan_month === ctx.slice.new_plan_line.plan_month
      && Norn.parseDate(ctx.slice.existing.period_month_date) === Norn.parseDate(ctx.slice.new_plan_line.period_month_date)
      && ctx.slice.existing.currency === ctx.slice.new_plan_line.currency
      && ctx.slice.existing.unit === ctx.slice.new_plan_line.unit,
    { root: "context" },
  )],
  writes: [{ type: ActivePlanLine, change: "modified" }],
  effect: { kind: "upsert_input_object", type: ActivePlanLine, objectIdField: "slot_id", writeMode: { kind: "update_only" } },
})

const RestatePosSaleInput = Schema.Struct({
  sale_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  sold_qty: Norn.decimal(6),
  unit: SupplyUnit,
  net_revenue: Norn.decimal(6),
  applied_unit_cost: Norn.decimal(4),
  cogs_amount: Norn.decimal(6),
  gross_margin: Norn.decimal(6),
  gross_margin_rate: Norn.decimal(6),
  cost_status: Schema.Literals(["costed", "missing_cost"]),
  costed_ref: Schema.String,
  currency: Currency,
  sold_at: Norn.timestamp("utc"),
})
const PosSaleRef = Schema.Struct({ sale_id: Schema.String })

/**
 * Governed late correction / restatement of an existing `supply.PosSale`
 * contribution (`update_only` on the same `sale_id`), preserving prior
 * knowledge-time incarnations for `AsOfReader` (`#1888`).
 */
export const RestatePosSale = Norn.action({
  id: "plan.pos_sale.restate", title: "Restate an existing POS sale contribution", version: "0.1.0",
  label: { en: "Restate POS sale", ar: "إعادة بيان سطر المبيعات" },
  input: RestatePosSaleInput, output: PosSaleRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "sale", type: PosSale, key: { property: { from: "input", property: "sale_id" } } },
    { name: "cost", type: UnitCost, key: { property: { from: "sale", property: "sku_id" } } },
  ],
  derive: [
    { field: "store_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ sale: Schema.Struct({ store_id: Schema.String }) }) }), (ctx) => ctx.slice.sale.store_id, { root: "context" }) },
    { field: "sku_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ sale: Schema.Struct({ sku_id: Schema.String }) }) }), (ctx) => ctx.slice.sale.sku_id, { root: "context" }) },
    { field: "unit", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ sale: Schema.Struct({ unit: SupplyUnit }) }) }), (ctx) => ctx.slice.sale.unit, { root: "context" }) },
    { field: "currency", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ sale: Schema.Struct({ currency: Currency }) }) }), (ctx) => ctx.slice.sale.currency, { root: "context" }) },
    { field: "cost_status", inputRequired: false, expression: Norn.expression(RestatePosSaleInput, () => "costed") },
    { field: "costed_ref", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ cost: Schema.Struct({ cost_ref: Schema.String }) }) }), (ctx) => ctx.slice.cost.cost_ref, { root: "context" }) },
    { field: "applied_unit_cost", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ cost: Schema.Struct({ unit_cost: Norn.decimal(4) }) }) }), (ctx) => ctx.slice.cost.unit_cost, { root: "context" }) },
    {
      field: "cogs_amount", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ sold_qty: Norn.decimal(6) }),
          slice: Schema.Struct({ cost: Schema.Struct({ unit_cost: Norn.decimal(4) }) }),
        }),
        (ctx) => ctx.input.sold_qty * ctx.slice.cost.unit_cost,
        { root: "context" },
      ),
    },
    {
      field: "gross_margin", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ sold_qty: Norn.decimal(6), net_revenue: Norn.decimal(6) }),
          slice: Schema.Struct({ cost: Schema.Struct({ unit_cost: Norn.decimal(4) }) }),
        }),
        (ctx) => ctx.input.net_revenue - (ctx.input.sold_qty * ctx.slice.cost.unit_cost),
        { root: "context" },
      ),
    },
    { field: "gross_margin_rate", inputRequired: false, expression: Norn.expression(RestatePosSaleInput, () => 0) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: RestatePosSaleInput,
      slice: Schema.Struct({
        sale: Schema.Struct({ cost_status: Schema.String, currency: Schema.String }),
        cost: Schema.Struct({ currency: Schema.String }),
      }),
    }),
    (ctx) =>
      ctx.slice.sale.cost_status === "costed"
      && ctx.slice.sale.currency === ctx.slice.cost.currency
      && ctx.input.sold_qty > 0
      && ctx.input.net_revenue >= 0,
    { root: "context" },
  )],
  writes: [{ type: PosSale, change: "modified" }],
  effect: { kind: "upsert_input_object", type: PosSale, objectIdField: "sale_id", writeMode: { kind: "update_only" } },
})

// ------------------------------------------------ OTB envelope lifecycle

const PlannedEnvelopeInput = Schema.Struct({
  envelope_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_start: Norn.timestamp("utc"),
  currency: Currency,
  envelope_kind: EnvelopeKind,
  approved_budget_cost: Norn.decimal(6),
  requested_by: Schema.String,
  active_grain_slot: Schema.String,
})
const UnplannedEnvelopeInput = Schema.Struct({
  envelope_id: Schema.String,
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_start: Norn.timestamp("utc"),
  currency: Currency,
  envelope_kind: EnvelopeKind,
  approved_budget_cost: Norn.decimal(6),
  override_reason: Schema.String,
  requested_by: Schema.String,
  active_grain_slot: Schema.String,
})
const ActivateEnvelopeInput = Schema.Struct({
  envelope_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  currency: Schema.String,
  active_slot_marker: Schema.String,
})
const EnvelopeRef = Schema.Struct({ envelope_id: Schema.String })

/**
 * Open a planned OTB envelope from an approved or locked PlanVersion's PlanLine,
 * deriving `approved_budget_cost` strictly from `slice.plan_line.planned_receipts_cost`.
 */
export const CreatePlannedEnvelope = Norn.action({
  id: "plan.otb_envelope.create_planned", title: "Create a planned OTB budget envelope", version: "0.1.0",
  label: { en: "Create planned OTB envelope", ar: "إنشاء مظروف ميزانية مخطط" },
  input: PlannedEnvelopeInput, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["requested_by"],
  slice: [
    { name: "version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "plan_line", type: PlanLine, key: { property: { from: "input", property: "plan_line_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  derive: [
    {
      field: "envelope_kind", inputRequired: false,
      expression: Norn.expression(PlannedEnvelopeInput, () => "planned"),
    },
    {
      field: "active_grain_slot", inputRequired: false,
      expression: Norn.expression(PlannedEnvelopeInput, (x) => x.envelope_id),
    },
    {
      field: "approved_budget_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_receipts_cost: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.plan_line.planned_receipts_cost,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PlannedEnvelopeInput,
      slice: Schema.Struct({
        version: Schema.Struct({ status: Schema.String }),
        plan_line: Schema.Struct({
          plan_version_id: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String, currency: Schema.String,
        }),
        owner_entity: Schema.Struct({ status: Schema.String }),
        store: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) =>
      (ctx.slice.version.status === "approved" || ctx.slice.version.status === "locked")
      && ctx.slice.owner_entity.status === "active"
      && ctx.slice.store.status === "active"
      && ctx.input.plan_version_id === ctx.slice.plan_line.plan_version_id
      && ctx.input.entity_id === ctx.slice.plan_line.entity_id
      && ctx.input.store_id === ctx.slice.plan_line.store_id
      && ctx.input.category === ctx.slice.plan_line.category
      && ctx.input.brand === ctx.slice.plan_line.brand
      && ctx.input.plan_month === ctx.slice.plan_line.plan_month
      && ctx.input.currency === ctx.slice.plan_line.currency,
    { root: "context" },
  )],
  createLinks: [
    { link: "plan_version", targetType: PlanVersion, targetIdField: "plan_version_id" },
    { link: "plan_line", targetType: PlanLine, targetIdField: "plan_line_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: OtbEnvelope, change: "created" }],
  effect: { kind: "upsert_input_object", type: OtbEnvelope, objectIdField: "envelope_id", writeMode: { kind: "create_only" } },
})

/**
 * Activate a planned OTB envelope as the sole active governing envelope for
 * its `(entity, store, category, brand, plan_month, currency)` grain,
 * refusing if a predecessor envelope is still active or its PlanVersion is
 * already superseded.
 */
export const ActivatePlannedEnvelope = Norn.action({
  id: "plan.otb_envelope.activate_planned", title: "Activate a planned OTB envelope", version: "0.1.0",
  label: { en: "Activate planned OTB envelope", ar: "تفعيل مظروف الميزانية المخطط" },
  input: ActivateEnvelopeInput, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } },
    { name: "plan_version", type: PlanVersion, key: { property: { from: "envelope", property: "plan_version_id" } } },
  ],
  aggregates: [
    {
      name: "active_grain_envelopes",
      type: OtbEnvelope,
      filter: {
        entity_id: "entity_id", store_id: "store_id", category: "category",
        brand: "brand", plan_month: "plan_month", currency: "currency",
        active_grain_slot: "active_slot_marker",
      },
      aggregate: { kind: "count" },
      maxItems: 10,
    },
  ],
  derive: [
    {
      field: "active_slot_marker", inputRequired: false,
      expression: Norn.expression(ActivateEnvelopeInput, () => "ACTIVE"),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: ActivateEnvelopeInput,
      slice: Schema.Struct({
        envelope: Schema.Struct({
          envelope_kind: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String, currency: Schema.String,
        }),
        plan_version: Schema.Struct({ status: Schema.String }),
      }),
      aggregates: Schema.Struct({ active_grain_envelopes: Schema.Int }),
    }),
    (ctx) =>
      ctx.slice.envelope.envelope_kind === "planned"
      && (ctx.slice.plan_version.status === "approved" || ctx.slice.plan_version.status === "locked")
      && ctx.aggregates.active_grain_envelopes === 0
      && ctx.input.entity_id === ctx.slice.envelope.entity_id
      && ctx.input.store_id === ctx.slice.envelope.store_id
      && ctx.input.category === ctx.slice.envelope.category
      && ctx.input.brand === ctx.slice.envelope.brand
      && ctx.input.plan_month === ctx.slice.envelope.plan_month
      && ctx.input.currency === ctx.slice.envelope.currency,
    { root: "context" },
  )],
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id",
    lifecycle: "status", transition: "activate_planned",
    setFields: { active_grain_slot: "active_slot_marker" },
  },
})

/** Request an explicit `unplanned` OTB envelope for an unbudgeted category/period. */
export const RequestUnplannedEnvelope = Norn.action({
  id: "plan.otb_envelope.request_unplanned", title: "Request an unplanned OTB override envelope", version: "0.1.0",
  label: { en: "Request unplanned OTB envelope", ar: "طلب استثناء ميزانية غير مخطط" },
  input: UnplannedEnvelopeInput, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["requested_by"],
  slice: [
    { name: "version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  derive: [
    { field: "envelope_kind", inputRequired: false, expression: Norn.expression(UnplannedEnvelopeInput, () => "unplanned") },
    { field: "active_grain_slot", inputRequired: false, expression: Norn.expression(UnplannedEnvelopeInput, (x) => x.envelope_id) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: UnplannedEnvelopeInput,
      slice: Schema.Struct({
        version: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String }),
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
      }),
    }),
    (ctx) =>
      (ctx.slice.version.status === "approved" || ctx.slice.version.status === "locked")
      && ctx.slice.owner_entity.status === "active"
      && ctx.slice.store.status === "active"
      && ctx.input.entity_id === ctx.slice.version.entity_id
      && ctx.input.entity_id === ctx.slice.store.entity_id
      && ctx.input.currency === ctx.slice.version.currency
      && ctx.input.currency === ctx.slice.store.currency
      && ctx.input.approved_budget_cost > 0,
    { root: "context" },
  )],
  createLinks: [
    { link: "plan_version", targetType: PlanVersion, targetIdField: "plan_version_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: OtbEnvelope, change: "created" }],
  effect: { kind: "upsert_input_object", type: OtbEnvelope, objectIdField: "envelope_id", writeMode: { kind: "create_only" } },
})

/** Submit an explicit `unplanned` OTB envelope for independent override approval. */
export const SubmitUnplannedOverride = Norn.action({
  id: "plan.otb_envelope.submit_override", title: "Submit an unplanned OTB override for approval", version: "0.1.0",
  label: { en: "Submit unplanned OTB override", ar: "تقديم استثناء الميزانية للاعتماد" },
  input: EnvelopeRef, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({ slice: Schema.Struct({ envelope: Schema.Struct({ envelope_kind: Schema.String }) }) }),
    (ctx) => ctx.slice.envelope.envelope_kind === "unplanned",
    { root: "context" },
  )],
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: { kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id", lifecycle: "status", transition: "submit_override" },
})

/**
 * Governed override approval action for an explicit `unplanned` OTB envelope.
 * Separation of duties (`independentOf: ["requested_by"]`): the requester cannot
 * approve their own unplanned budget override, and at most one envelope may be
 * active for the grain at a time.
 */
export const ApproveUnplannedOverride = Norn.action({
  id: "plan.otb_envelope.approve_override", title: "Approve an unplanned OTB override envelope", version: "0.1.0",
  label: { en: "Approve unplanned OTB override", ar: "اعتماد استثناء الميزانية" },
  input: ActivateEnvelopeInput, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } },
    { name: "plan_version", type: PlanVersion, key: { property: { from: "envelope", property: "plan_version_id" } } },
  ],
  aggregates: [
    {
      name: "active_grain_envelopes",
      type: OtbEnvelope,
      filter: {
        entity_id: "entity_id", store_id: "store_id", category: "category",
        brand: "brand", plan_month: "plan_month", currency: "currency",
        active_grain_slot: "active_slot_marker",
      },
      aggregate: { kind: "count" },
      maxItems: 10,
    },
  ],
  derive: [
    {
      field: "active_slot_marker", inputRequired: false,
      expression: Norn.expression(ActivateEnvelopeInput, () => "ACTIVE"),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: ActivateEnvelopeInput,
      slice: Schema.Struct({
        envelope: Schema.Struct({
          envelope_kind: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String, currency: Schema.String,
        }),
        plan_version: Schema.Struct({ status: Schema.String }),
      }),
      aggregates: Schema.Struct({ active_grain_envelopes: Schema.Int }),
    }),
    (ctx) =>
      ctx.slice.envelope.envelope_kind === "unplanned"
      && (ctx.slice.plan_version.status === "approved" || ctx.slice.plan_version.status === "locked")
      && ctx.aggregates.active_grain_envelopes === 0
      && ctx.input.entity_id === ctx.slice.envelope.entity_id
      && ctx.input.store_id === ctx.slice.envelope.store_id
      && ctx.input.category === ctx.slice.envelope.category
      && ctx.input.brand === ctx.slice.envelope.brand
      && ctx.input.plan_month === ctx.slice.envelope.plan_month
      && ctx.input.currency === ctx.slice.envelope.currency,
    { root: "context" },
  )],
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id",
    lifecycle: "status", transition: "approve_override",
    setFields: { active_grain_slot: "active_slot_marker" },
  },
  independentOf: ["requested_by"],
})

export const CloseEnvelope = Norn.action({
  id: "plan.otb_envelope.close", title: "Close an OTB budget envelope", version: "0.1.0",
  label: { en: "Close OTB envelope", ar: "إغلاق مظروف الميزانية" },
  input: EnvelopeRef, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id",
    lifecycle: "status", transition: "close",
    setFields: { active_grain_slot: "envelope_id" },
  },
})

// ----------------------------- atomic fail-closed OTB PO authorization + submit

const AuthorizePoLineInput = Schema.Struct({
  binding_id: Schema.String,
  envelope_id: Schema.String,
  po_id: Schema.String,
  line_id: Schema.String,
  // Merchandise grain & SKU keys verified strictly against governed slice records
  // so fenced OTB consumption aggregates across all revisions of the grain:
  sku_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  currency: Schema.String,
  expected_payment_date: Norn.timestamp("utc"),
  payment_terms_days: Schema.Int,
  as_of: Norn.timestamp("utc"),
  // Kernel-derived strictly from governed slice + fenced aggregates (caller never supplies):
  plan_version_id: Schema.String,
  unit: Schema.String,
  ordered_qty: Norn.decimal(6),
  unit_cost: Norn.decimal(4),
  authorized_cost: Norn.decimal(6),
  envelope_cap_snapshot: Norn.decimal(6),
  prior_consumed_snapshot: Norn.decimal(6),
  authorized_binding_seq: Norn.decimal(0),
})
const AuthorizePoWriterInput = Schema.Struct({
  binding_id: Schema.String,
  envelope_id: Schema.String,
  po_id: Schema.String,
  line_id: Schema.String,
  expected_payment_date: Schema.String,
  payment_terms_days: Schema.Int,
  as_of: Schema.String,
  plan_version_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  sku_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  currency: Schema.String,
  unit: Schema.String,
  ordered_qty: Norn.decimal(6),
  unit_cost: Norn.decimal(4),
  authorized_cost: Norn.decimal(6),
  envelope_cap_snapshot: Norn.decimal(6),
  prior_consumed_snapshot: Norn.decimal(6),
  authorized_binding_seq: Norn.decimal(0),
})
const PoRef = Schema.Struct({ po_id: Schema.String })
const BindingRef = Schema.Struct({ binding_id: Schema.String })

const OtbBindingCreateValues = Schema.Struct({
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
  currency: Schema.String,
  unit: Schema.String,
  ordered_qty: Norn.decimal(6),
  received_qty: Norn.decimal(6),
  unit_cost: Norn.decimal(4),
  authorized_cost: Norn.decimal(6),
  recognized_receipt_cost: Norn.decimal(6),
  unreceived_authorized_cost: Norn.decimal(6),
  cancelled_released_cost: Norn.decimal(6),
  otb_consumed_cost: Norn.decimal(6),
  envelope_cap_snapshot: Norn.decimal(6),
  prior_consumed_snapshot: Norn.decimal(6),
  authorized_binding_seq: Norn.decimal(0),
  expected_payment_date: Schema.String,
  payment_terms_days: Schema.Int,
  as_of: Schema.String,
  authorized_by: Schema.String,
  status: Schema.Literal("authorized"),
})

/**
 * Pure synchronous writer creating `plan.OtbPoBinding` atomically in the exact
 * same commit that transitions `supply.PurchaseOrder` `draft -> submitted`
 * (links are attached declaratively via `createLinks` on
 * `AuthorizePoLineAgainstOtb`).
 */
export const AuthorizePoOtbWriter = Norn.decision({
  id: "plan.otb.authorize_po_writer",
  title: "Create OTB PO binding atomically with PO submission",
  version: "0.1.0",
  contract: "decide",
  scopeRequired: Scope,
  input: Schema.Struct({
    slice: Schema.Struct({}),
    event: Schema.Struct({
      input: AuthorizePoWriterInput,
      actor: Schema.String,
      operationId: Schema.String,
    }),
  }),
  output: Schema.Struct({
    facts: Schema.Array(Schema.Struct({
      op: Schema.Literals(["create"]),
      objectType: Schema.Literals(["plan.OtbPoBinding"]),
      key: Schema.String,
      values: OtbBindingCreateValues,
    })),
    commands: Schema.Array(Schema.Struct({})),
    timers: Schema.Array(Schema.Struct({})),
  }),
}, (input) => ({
  facts: [{
    op: "create",
    objectType: "plan.OtbPoBinding",
    key: input.event.input.binding_id,
    values: {
      binding_id: input.event.input.binding_id,
      envelope_id: input.event.input.envelope_id,
      plan_version_id: input.event.input.plan_version_id,
      entity_id: input.event.input.entity_id,
      store_id: input.event.input.store_id,
      sku_id: input.event.input.sku_id,
      category: input.event.input.category,
      brand: input.event.input.brand,
      plan_month: input.event.input.plan_month,
      po_id: input.event.input.po_id,
      line_id: input.event.input.line_id,
      currency: input.event.input.currency,
      unit: input.event.input.unit,
      ordered_qty: input.event.input.ordered_qty,
      received_qty: 0,
      unit_cost: input.event.input.unit_cost,
      authorized_cost: input.event.input.authorized_cost,
      recognized_receipt_cost: 0,
      unreceived_authorized_cost: input.event.input.authorized_cost,
      cancelled_released_cost: 0,
      otb_consumed_cost: input.event.input.authorized_cost,
      envelope_cap_snapshot: input.event.input.envelope_cap_snapshot,
      prior_consumed_snapshot: input.event.input.prior_consumed_snapshot,
      authorized_binding_seq: input.event.input.authorized_binding_seq,
      expected_payment_date: input.event.input.expected_payment_date,
      payment_terms_days: input.event.input.payment_terms_days,
      as_of: input.event.input.as_of,
      authorized_by: input.event.actor,
      status: "authorized",
    },
  }],
  commands: [],
  timers: [],
}))

/**
 * Atomic, fail-closed OTB authorization & PO submission:
 * - Transitions `supply.PurchaseOrder` `draft -> submitted` (with SoD
 *   `independentOf: ["created_by"]`), creates `plan.OtbPoBinding` + governed
 *   links (`createLinks`), and updates `OtbEnvelope.binding_count` in ONE
 *   atomic checked transaction (`synchronousRule`).
 * - Requires the target `OtbEnvelope` to be the active governing envelope for
 *   the grain under a non-superseded (`approved` or `locked`) `PlanVersion`.
 * - Fences `plan.OtbPoBinding` consumption across the ENTIRE merchandise grain
 *   `(entity_id, store_id, category, brand, plan_month, currency)` (`TXN-PHANTOM`)
 *   so superseding a plan version/envelope NEVER resets spend while predecessor
 *   commitments remain open.
 */
export const AuthorizePoLineAgainstOtb = Norn.action({
  id: "plan.otb.authorize_po_line", title: "Authorize and submit a draft PO against an OTB envelope", version: "0.1.0",
  label: { en: "Authorize & submit PO (OTB)", ar: "اعتماد وإرسال أمر الشراء ضمن الميزانية" },
  input: AuthorizePoLineInput, output: PoRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
    { name: "order_line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "sku", type: Sku, key: { property: { from: "input", property: "sku_id" } } },
    { name: "sku_profile", type: SkuProfile, key: { property: { from: "input", property: "sku_id" } } },
    { name: "po_destination", type: PoDestination, key: { property: { from: "input", property: "po_id" } } },
    { name: "store", type: Store, key: { property: { from: "po_destination", property: "store_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "po_destination", property: "entity_id" } } },
    { name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } },
    { name: "plan_version", type: PlanVersion, key: { property: { from: "envelope", property: "plan_version_id" } } },
  ],
  aggregates: [
    {
      name: "consumed_otb_cost",
      type: OtbPoBinding,
      filter: {
        entity_id: "entity_id", store_id: "store_id", category: "category",
        brand: "brand", plan_month: "plan_month", currency: "currency",
      },
      aggregate: { kind: "sum", property: "otb_consumed_cost" },
      maxItems: 200,
    },
    {
      name: "grain_binding_count",
      type: OtbPoBinding,
      filter: {
        entity_id: "entity_id", store_id: "store_id", category: "category",
        brand: "brand", plan_month: "plan_month", currency: "currency",
      },
      aggregate: { kind: "count" },
      maxItems: 200,
    },
    {
      name: "po_line_count",
      type: OrderLine,
      filter: { po_id: "po_id" },
      aggregate: { kind: "count" },
      maxItems: 50,
    },
  ],
  derive: [
    { field: "plan_version_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ envelope: Schema.Struct({ plan_version_id: Schema.String }) }) }), (ctx) => ctx.slice.envelope.plan_version_id, { root: "context" }) },
    { field: "unit", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ unit: Schema.String }) }) }), (ctx) => ctx.slice.order_line.unit, { root: "context" }) },
    { field: "ordered_qty", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ ordered_qty: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.order_line.ordered_qty, { root: "context" }) },
    { field: "unit_cost", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ unit_price: Norn.decimal(4) }) }) }), (ctx) => ctx.slice.order_line.unit_price, { root: "context" }) },
    {
      field: "authorized_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ ordered_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }) }) }),
        (ctx) => ctx.slice.order_line.ordered_qty * ctx.slice.order_line.unit_price,
        { root: "context" },
      ),
    },
    { field: "envelope_cap_snapshot", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ envelope: Schema.Struct({ approved_budget_cost: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.envelope.approved_budget_cost, { root: "context" }) },
    { field: "prior_consumed_snapshot", inputRequired: false, expression: Norn.expression(Schema.Struct({ aggregates: Schema.Struct({ consumed_otb_cost: Norn.decimal(6) }) }), (ctx) => ctx.aggregates.consumed_otb_cost, { root: "context" }) },
    { field: "authorized_binding_seq", inputRequired: false, expression: Norn.expression(Schema.Struct({ aggregates: Schema.Struct({ grain_binding_count: Schema.Int }) }), (ctx) => ctx.aggregates.grain_binding_count + 1, { root: "context" }) },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: AuthorizePoLineInput,
      slice: Schema.Struct({
        order: Schema.Struct({ status: Schema.String, currency: Schema.String }),
        order_line: Schema.Struct({ po_id: Schema.String, sku_id: Schema.String, unit: Schema.String, currency: Schema.String, received_qty: Norn.decimal(6) }),
        sku: Schema.Struct({ category: Schema.String, unit_of_measure: Schema.String }),
        sku_profile: Schema.Struct({ category: Schema.String, brand: Schema.String }),
        po_destination: Schema.Struct({ entity_id: Schema.String, store_id: Schema.String, plan_month: Schema.String, currency: Schema.String }),
        store: Schema.Struct({ status: Schema.String, entity_id: Schema.String, currency: Schema.String }),
        owner_entity: Schema.Struct({ status: Schema.String }),
        envelope: Schema.Struct({
          status: Schema.String, active_grain_slot: Schema.String,
          entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String,
          currency: Schema.String, approved_budget_cost: Norn.decimal(6),
        }),
        plan_version: Schema.Struct({ status: Schema.String }),
      }),
      aggregates: Schema.Struct({ consumed_otb_cost: Norn.decimal(6), po_line_count: Schema.Int }),
    }),
    (ctx) =>
      ctx.slice.order.status === "draft"
      && ctx.aggregates.po_line_count === 1
      && ctx.slice.order_line.po_id === ctx.input.po_id
      && ctx.slice.order_line.sku_id === ctx.input.sku_id
      && ctx.slice.order_line.received_qty === 0
      && ctx.slice.envelope.status === "active"
      && ctx.slice.envelope.active_grain_slot === "ACTIVE"
      && (ctx.slice.plan_version.status === "approved" || ctx.slice.plan_version.status === "locked")
      && ctx.slice.store.status === "active"
      && ctx.slice.owner_entity.status === "active"
      && ctx.input.entity_id === ctx.slice.store.entity_id
      && ctx.input.entity_id === ctx.slice.po_destination.entity_id
      && ctx.input.store_id === ctx.slice.po_destination.store_id
      && ctx.input.category === ctx.slice.sku.category
      && ctx.input.category === ctx.slice.sku_profile.category
      && ctx.input.brand === ctx.slice.sku_profile.brand
      && ctx.input.plan_month === ctx.slice.po_destination.plan_month
      && ctx.input.currency === ctx.slice.order.currency
      && ctx.slice.order.currency === ctx.slice.order_line.currency
      && ctx.slice.order_line.currency === ctx.slice.po_destination.currency
      && ctx.slice.order_line.currency === ctx.slice.store.currency
      && ctx.slice.order_line.currency === ctx.slice.envelope.currency
      && ctx.slice.order_line.unit === ctx.slice.sku.unit_of_measure
      && ctx.slice.envelope.entity_id === ctx.input.entity_id
      && ctx.slice.envelope.store_id === ctx.input.store_id
      && ctx.slice.envelope.category === ctx.input.category
      && ctx.slice.envelope.brand === ctx.input.brand
      && ctx.slice.envelope.plan_month === ctx.input.plan_month
      && (ctx.aggregates.consumed_otb_cost + ctx.input.authorized_cost) <= ctx.slice.envelope.approved_budget_cost,
    { root: "context" },
  )],
  createLinks: [
    { source: { objectType: OtbPoBinding, idField: "binding_id" }, link: "envelope", targetType: OtbEnvelope, targetIdField: "envelope_id" },
    { source: { objectType: OtbPoBinding, idField: "binding_id" }, link: "order", targetType: PurchaseOrder, targetIdField: "po_id" },
    { source: { objectType: OtbPoBinding, idField: "binding_id" }, link: "order_line", targetType: OrderLine, targetIdField: "line_id" },
    { source: { objectType: OtbPoBinding, idField: "binding_id" }, link: "sku", targetType: Sku, targetIdField: "sku_id" },
    { source: { objectType: OtbPoBinding, idField: "binding_id" }, link: "sku_profile", targetType: SkuProfile, targetIdField: "sku_id" },
    { source: { objectType: OtbPoBinding, idField: "binding_id" }, link: "po_destination", targetType: PoDestination, targetIdField: "po_id" },
  ],
  synchronousRule: AuthorizePoOtbWriter,
  writes: [
    { type: PurchaseOrder, change: "modified" },
    { type: OtbPoBinding, change: "created" },
  ],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "po_id", lifecycle: "status", transition: "submit" },
  independentOf: ["created_by"],
})

const PartialReceiptOtbInput = Schema.Struct({
  binding_id: Schema.String,
  line_id: Schema.String,
  receipt_id: Schema.String,
  as_of: Norn.timestamp("utc"),
  next_received_qty: Norn.decimal(6),
  next_recognized_receipt_cost: Norn.decimal(6),
  next_unreceived_authorized_cost: Norn.decimal(6),
})

/**
 * Shift accepted partial receipt cost from `unreceived_authorized_cost` into
 * `recognized_receipt_cost` while conserving net `otb_consumed_cost`. Supports
 * repeated consecutive partial receipts (`authorized -> partially_received` and
 * `partially_received -> partially_received`), coupled directly to checked
 * `supply.OrderLine.received_qty` and `supply.OrderLine.unit_price`.
 */
export const RecordOtbPartialReceipt = Norn.action({
  id: "plan.otb.record_partial_receipt", title: "Recognize partial receipt against an OTB PO binding", version: "0.1.0",
  label: { en: "Record OTB partial receipt", ar: "تسجيل استلام جزئي على الميزانية" },
  input: PartialReceiptOtbInput, output: BindingRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "binding", type: OtbPoBinding, key: { property: { from: "input", property: "binding_id" } } },
    { name: "order_line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "order_line", property: "po_id" } } },
    { name: "receipt", type: ShipmentReceipt, key: { property: { from: "input", property: "receipt_id" } } },
  ],
  aggregates: [{
    name: "shipped_receipts_qty",
    type: ShipmentReceipt,
    filter: { line_id: "line_id" },
    aggregate: { kind: "sum", property: "received_qty" },
    maxItems: 100,
  }],
  derive: [
    {
      field: "next_received_qty", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ received_qty: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.order_line.received_qty,
        { root: "context" },
      ),
    },
    {
      field: "next_recognized_receipt_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          slice: Schema.Struct({
            order_line: Schema.Struct({ received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }),
          }),
        }),
        (ctx) => ctx.slice.order_line.received_qty * ctx.slice.order_line.unit_price,
        { root: "context" },
      ),
    },
    {
      field: "next_unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          slice: Schema.Struct({
            binding: Schema.Struct({ authorized_cost: Norn.decimal(6) }),
            order_line: Schema.Struct({ received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }),
          }),
        }),
        (ctx) => ctx.slice.binding.authorized_cost - (ctx.slice.order_line.received_qty * ctx.slice.order_line.unit_price),
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PartialReceiptOtbInput,
      slice: Schema.Struct({
        binding: Schema.Struct({ line_id: Schema.String, ordered_qty: Norn.decimal(6), received_qty: Norn.decimal(6) }),
        order: Schema.Struct({ status: Schema.String }),
        order_line: Schema.Struct({ received_qty: Norn.decimal(6) }),
        receipt: Schema.Struct({ line_id: Schema.NullOr(Schema.String) }),
      }),
      aggregates: Schema.Struct({ shipped_receipts_qty: Norn.decimal(6) }),
    }),
    (ctx) =>
      ctx.slice.binding.line_id === ctx.input.line_id
      && (ctx.slice.order.status === "submitted" || ctx.slice.order.status === "partially_received")
      && (ctx.slice.receipt.line_id ?? "") === ctx.input.line_id
      && ctx.slice.order_line.received_qty > ctx.slice.binding.received_qty
      && ctx.slice.order_line.received_qty < ctx.slice.binding.ordered_qty
      && ctx.slice.order_line.received_qty === ctx.aggregates.shipped_receipts_qty,
    { root: "context" },
  )],
  writes: [{ type: OtbPoBinding, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbPoBinding, objectIdField: "binding_id",
    lifecycle: "status", transition: "receive_partial",
    setFields: {
      received_qty: "next_received_qty",
      recognized_receipt_cost: "next_recognized_receipt_cost",
      unreceived_authorized_cost: "next_unreceived_authorized_cost",
      as_of: "as_of",
    },
  },
})

const UpdatePartialReceiptOtbInput = Schema.Struct({
  binding_id: Schema.String,
  line_id: Schema.String,
  as_of: Norn.timestamp("utc"),
  received_qty: Norn.decimal(6),
  recognized_receipt_cost: Norn.decimal(6),
  unreceived_authorized_cost: Norn.decimal(6),
})

/**
 * Recognize a subsequent partial receipt on an already `partially_received`
 * `plan.OtbPoBinding` (`writeMode: update_only`, no lifecycle self-transition),
 * shifting exact receipt cost from `unreceived_authorized_cost` to
 * `recognized_receipt_cost` while conserving net `otb_consumed_cost`.
 */
export const UpdateOtbPartialReceipt = Norn.action({
  id: "plan.otb.update_partial_receipt", title: "Recognize subsequent partial receipt on an OTB PO binding", version: "0.1.0",
  label: { en: "Update OTB partial receipt", ar: "تحديث الاستلام الجزئي على الميزانية" },
  input: UpdatePartialReceiptOtbInput, output: BindingRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "binding", type: OtbPoBinding, key: { property: { from: "input", property: "binding_id" } } },
    { name: "order_line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "order_line", property: "po_id" } } },
  ],
  aggregates: [{
    name: "shipped_receipts_qty",
    type: ShipmentReceipt,
    filter: { line_id: "line_id" },
    aggregate: { kind: "sum", property: "received_qty" },
    maxItems: 100,
  }],
  derive: [
    {
      field: "received_qty", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ received_qty: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.order_line.received_qty,
        { root: "context" },
      ),
    },
    {
      field: "recognized_receipt_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          slice: Schema.Struct({
            order_line: Schema.Struct({ received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }),
          }),
        }),
        (ctx) => ctx.slice.order_line.received_qty * ctx.slice.order_line.unit_price,
        { root: "context" },
      ),
    },
    {
      field: "unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          slice: Schema.Struct({
            binding: Schema.Struct({ authorized_cost: Norn.decimal(6) }),
            order_line: Schema.Struct({ received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }),
          }),
        }),
        (ctx) => ctx.slice.binding.authorized_cost - (ctx.slice.order_line.received_qty * ctx.slice.order_line.unit_price),
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: UpdatePartialReceiptOtbInput,
      slice: Schema.Struct({
        binding: Schema.Struct({ status: Schema.String, line_id: Schema.String, ordered_qty: Norn.decimal(6), received_qty: Norn.decimal(6) }),
        order: Schema.Struct({ status: Schema.String }),
        order_line: Schema.Struct({ received_qty: Norn.decimal(6) }),
      }),
      aggregates: Schema.Struct({ shipped_receipts_qty: Norn.decimal(6) }),
    }),
    (ctx) =>
      ctx.slice.binding.status === "partially_received"
      && ctx.slice.binding.line_id === ctx.input.line_id
      && ctx.slice.order.status === "partially_received"
      && ctx.slice.order_line.received_qty > ctx.slice.binding.received_qty
      && ctx.slice.order_line.received_qty < ctx.slice.binding.ordered_qty
      && ctx.slice.order_line.received_qty === ctx.aggregates.shipped_receipts_qty,
    { root: "context" },
  )],
  writes: [{ type: OtbPoBinding, change: "modified" }],
  effect: { kind: "upsert_input_object", type: OtbPoBinding, objectIdField: "binding_id", writeMode: { kind: "update_only" } },
})

/** Fulfil an OTB PO binding once the underlying `supply.OrderLine` is fully received. */
export const FulfilOtbPoBinding = Norn.action({
  id: "plan.otb.fulfil_po_line", title: "Fulfil an OTB PO binding on full receipt", version: "0.1.0",
  label: { en: "Fulfil OTB PO binding", ar: "إكمال ربط أمر الشراء بالميزانية" },
  input: PartialReceiptOtbInput, output: BindingRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "binding", type: OtbPoBinding, key: { property: { from: "input", property: "binding_id" } } },
    { name: "order_line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "receipt", type: ShipmentReceipt, key: { property: { from: "input", property: "receipt_id" } } },
  ],
  derive: [
    {
      field: "next_received_qty", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ received_qty: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.order_line.received_qty,
        { root: "context" },
      ),
    },
    {
      field: "next_recognized_receipt_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ binding: Schema.Struct({ authorized_cost: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.binding.authorized_cost,
        { root: "context" },
      ),
    },
    {
      field: "next_unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(PartialReceiptOtbInput, () => 0),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PartialReceiptOtbInput,
      slice: Schema.Struct({
        binding: Schema.Struct({ line_id: Schema.String, ordered_qty: Norn.decimal(6) }),
        order_line: Schema.Struct({ received_qty: Norn.decimal(6) }),
        receipt: Schema.Struct({ line_id: Schema.NullOr(Schema.String) }),
      }),
    }),
    (ctx) =>
      ctx.slice.binding.line_id === ctx.input.line_id
      && (ctx.slice.receipt.line_id ?? "") === ctx.input.line_id
      && ctx.slice.order_line.received_qty === ctx.slice.binding.ordered_qty,
    { root: "context" },
  )],
  writes: [{ type: OtbPoBinding, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbPoBinding, objectIdField: "binding_id",
    lifecycle: "status", transition: "fulfil",
    setFields: {
      received_qty: "next_received_qty",
      recognized_receipt_cost: "next_recognized_receipt_cost",
      unreceived_authorized_cost: "next_unreceived_authorized_cost",
      as_of: "as_of",
    },
  },
})

const CancelPoOtbInput = Schema.Struct({
  binding_id: Schema.String,
  po_id: Schema.String,
  line_id: Schema.String,
  as_of: Norn.timestamp("utc"),
  next_received_qty: Norn.decimal(6),
  next_recognized_receipt_cost: Norn.decimal(6),
  next_unreceived_authorized_cost: Norn.decimal(6),
  next_cancelled_released_cost: Norn.decimal(6),
  next_otb_consumed_cost: Norn.decimal(6),
})

/**
 * Release the exact unreceived OTB balance strictly AFTER the bound
 * `supply.PurchaseOrder` has reached terminal `cancelled` state (where no
 * further receipts can post). Computes both recognized receipts and the
 * released remainder directly from checked `supply.OrderLine` and fenced
 * `supply.ShipmentReceipt` sums, never trusting stale binding balances.
 */
export const ReleaseCancelledPoOtb = Norn.action({
  id: "plan.otb.release_cancelled_po", title: "Release unreceived OTB on PO cancellation", version: "0.1.0",
  label: { en: "Release cancelled PO OTB", ar: "تحرير ميزانية أمر الشراء الملغى" },
  input: CancelPoOtbInput, output: BindingRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "binding", type: OtbPoBinding, key: { property: { from: "input", property: "binding_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
    { name: "order_line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
  ],
  aggregates: [{
    name: "shipped_receipts_qty",
    type: ShipmentReceipt,
    filter: { line_id: "line_id" },
    aggregate: { kind: "sum", property: "received_qty" },
    maxItems: 100,
  }],
  derive: [
    {
      field: "next_received_qty", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ received_qty: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.order_line.received_qty,
        { root: "context" },
      ),
    },
    {
      field: "next_recognized_receipt_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }) }) }),
        (ctx) => ctx.slice.order_line.received_qty * ctx.slice.order_line.unit_price,
        { root: "context" },
      ),
    },
    {
      field: "next_unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(CancelPoOtbInput, () => 0),
    },
    {
      field: "next_cancelled_released_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          slice: Schema.Struct({
            order_line: Schema.Struct({ ordered_qty: Norn.decimal(6), received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }),
          }),
        }),
        (ctx) => (ctx.slice.order_line.ordered_qty - ctx.slice.order_line.received_qty) * ctx.slice.order_line.unit_price,
        { root: "context" },
      ),
    },
    {
      field: "next_otb_consumed_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ received_qty: Norn.decimal(6), unit_price: Norn.decimal(4) }) }) }),
        (ctx) => ctx.slice.order_line.received_qty * ctx.slice.order_line.unit_price,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: CancelPoOtbInput,
      slice: Schema.Struct({
        binding: Schema.Struct({ po_id: Schema.String, line_id: Schema.String }),
        order: Schema.Struct({ status: Schema.String }),
        order_line: Schema.Struct({ po_id: Schema.String, received_qty: Norn.decimal(6) }),
      }),
      aggregates: Schema.Struct({ shipped_receipts_qty: Norn.decimal(6) }),
    }),
    (ctx) =>
      ctx.slice.binding.po_id === ctx.input.po_id
      && ctx.slice.binding.line_id === ctx.input.line_id
      && ctx.slice.order_line.po_id === ctx.input.po_id
      && ctx.slice.order.status === "cancelled"
      && ctx.slice.order_line.received_qty === ctx.aggregates.shipped_receipts_qty,
    { root: "context" },
  )],
  writes: [{ type: OtbPoBinding, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbPoBinding, objectIdField: "binding_id",
    lifecycle: "status", transition: "cancel",
    setFields: {
      received_qty: "next_received_qty",
      recognized_receipt_cost: "next_recognized_receipt_cost",
      unreceived_authorized_cost: "next_unreceived_authorized_cost",
      cancelled_released_cost: "next_cancelled_released_cost",
      otb_consumed_cost: "next_otb_consumed_cost",
      as_of: "as_of",
    },
  },
})

// ------------------------------------------------ PublishedForecast writers

const PublishedForecastInput = Schema.Struct({
  forecast_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.String,
  model_version: Schema.String,
  forecast_revision: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: Schema.String,
  brand: Schema.String,
  plan_month: Schema.String,
  period_start: Norn.timestamp("utc"),
  published_at: Norn.timestamp("utc"),
  currency: Currency,
  unit: PlanUnit,
  forecast_sales_retail: Norn.decimal(6),
  forecast_sales_cost: Norn.decimal(6),
  forecast_sales_qty: Norn.decimal(6),
  forecast_fine_metal_g: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }),
  forecast_sales_retail_variance: Norn.decimal(6),
  forecast_sales_cost_variance: Norn.decimal(6),
  forecast_sales_qty_variance: Norn.decimal(6),
})
const PublishForecastInput = Schema.Struct({
  forecast_id: Schema.String,
  published_at: Norn.timestamp("utc"),
})
const ForecastRef = Schema.Struct({ forecast_id: Schema.String })

/** Record a draft plan-grain forecast from a production `ml.ModelVersion`, deriving exact `Forecast - Plan` variances. */
export const RecordPublishedForecast = Norn.action({
  id: "plan.forecast.record", title: "Record a plan-grain forecast revision", version: "0.1.0",
  label: { en: "Record plan forecast", ar: "تسجيل توقع الخطة" },
  dependencies: CoreUnitsDep,
  input: PublishedForecastInput, output: ForecastRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "model", type: "ml.ModelVersion", key: { property: { from: "input", property: "model_version" } } },
    { name: "plan_version", type: PlanVersion, key: { property: { from: "input", property: "plan_version_id" } } },
    { name: "plan_line", type: PlanLine, key: { property: { from: "input", property: "plan_line_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  derive: [
    { field: "published_at", inputRequired: false, expression: Norn.commitTime() },
    {
      field: "forecast_sales_retail_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ forecast_sales_retail: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_retail: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.forecast_sales_retail - ctx.slice.plan_line.planned_sales_retail,
        { root: "context" },
      ),
    },
    {
      field: "forecast_sales_cost_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ forecast_sales_cost: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_cost: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.forecast_sales_cost - ctx.slice.plan_line.planned_sales_cost,
        { root: "context" },
      ),
    },
    {
      field: "forecast_sales_qty_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ forecast_sales_qty: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_qty: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.forecast_sales_qty - ctx.slice.plan_line.planned_sales_qty,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: PublishedForecastInput,
      slice: Schema.Struct({
        model: Schema.Struct({ status: Schema.String }),
        plan_line: Schema.Struct({
          plan_version_id: Schema.String, entity_id: Schema.String, store_id: Schema.String,
          category: Schema.String, brand: Schema.String, plan_month: Schema.String,
          currency: Schema.String, unit: Schema.String,
        }),
      }),
    }),
    (ctx) =>
      ctx.slice.model.status === "production"
      && ctx.input.plan_version_id === ctx.slice.plan_line.plan_version_id
      && ctx.input.entity_id === ctx.slice.plan_line.entity_id
      && ctx.input.store_id === ctx.slice.plan_line.store_id
      && ctx.input.category === ctx.slice.plan_line.category
      && ctx.input.brand === ctx.slice.plan_line.brand
      && ctx.input.plan_month === ctx.slice.plan_line.plan_month
      && ctx.input.currency === ctx.slice.plan_line.currency
      && ctx.input.unit === ctx.slice.plan_line.unit,
    { root: "context" },
  )],
  createLinks: [
    { link: "scored_by", targetType: "ml.ModelVersion", targetIdField: "model_version" },
    { link: "plan_version", targetType: PlanVersion, targetIdField: "plan_version_id" },
    { link: "plan_line", targetType: PlanLine, targetIdField: "plan_line_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: PublishedForecast, change: "created" }],
  effect: { kind: "upsert_input_object", type: PublishedForecast, objectIdField: "forecast_id", writeMode: { kind: "create_only" } },
})

/**
 * Publish a draft plan-grain forecast revision (`draft -> published`),
 * re-checking that its linked `ml.ModelVersion` is STILL in `production` at
 * publish time and stamping `published_at` from `Norn.commitTime()`.
 */
export const PublishForecast = Norn.action({
  id: "plan.forecast.publish", title: "Publish a plan-grain forecast revision", version: "0.1.0",
  label: { en: "Publish forecast", ar: "نشر التوقع" },
  input: PublishForecastInput, output: ForecastRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "forecast", type: PublishedForecast, key: { property: { from: "input", property: "forecast_id" } } },
    { name: "model", type: "ml.ModelVersion", key: { property: { from: "forecast", property: "model_version" } } },
  ],
  derive: [
    { field: "published_at", inputRequired: false, expression: Norn.commitTime() },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      slice: Schema.Struct({ model: Schema.Struct({ status: Schema.String }) }),
    }),
    (ctx) => ctx.slice.model.status === "production",
    { root: "context" },
  )],
  writes: [{ type: PublishedForecast, change: "modified" }],
  effect: {
    kind: "fire_transition", type: PublishedForecast, objectIdField: "forecast_id",
    lifecycle: "status", transition: "publish",
    setFields: { published_at: "published_at" },
  },
})

export const SupersedeForecast = Norn.action({
  id: "plan.forecast.supersede", title: "Supersede a published forecast revision", version: "0.1.0",
  label: { en: "Supersede forecast", ar: "استبدال التوقع" },
  input: ForecastRef, output: ForecastRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PublishedForecast, change: "modified" }],
  effect: { kind: "fire_transition", type: PublishedForecast, objectIdField: "forecast_id", lifecycle: "status", transition: "supersede" },
})

// ----------------------------------------- governed Actual - Plan variance writers

const CostStatus = Schema.Literals(["costed", "missing_cost"])

const ActualPlanVarianceInput = Schema.Struct({
  variance_id: Schema.String,
  slot_id: Schema.String,
  plan_version_id: Schema.String,
  plan_line_id: Schema.String,
  entity_id: Schema.String,
  store_id: Schema.String,
  category: SupplyCategory,
  brand: Schema.String,
  plan_month: Schema.String,
  period_start: Norn.timestamp("utc"),
  period_end: Norn.timestamp("utc"),
  currency: Currency,
  unit: SupplyUnit,
  cost_status: CostStatus,
  actual_sales_retail: Norn.decimal(6),
  planned_sales_retail: Norn.decimal(6),
  sales_retail_variance: Norn.decimal(6),
  actual_sales_cost: Norn.decimal(6),
  planned_sales_cost: Norn.decimal(6),
  sales_cost_variance: Norn.decimal(6),
  actual_sales_qty: Norn.decimal(6),
  planned_sales_qty: Norn.decimal(6),
  sales_qty_variance: Norn.decimal(6),
  as_of: Norn.timestamp("utc"),
})
const VarianceRef = Schema.Struct({ variance_id: Schema.String })

/**
 * Record an initial governed `Actual - Plan` variance snapshot pinned to an
 * active baseline slot, deriving planned targets and exact variances from
 * `slice.plan_line` and bounding actuals against fenced `PosSale` sums.
 */
export const RecordActualPlanVariance = Norn.action({
  id: "plan.variance.record_actual_vs_plan", title: "Record an Actual minus Plan variance snapshot", version: "0.1.0",
  label: { en: "Record Actual vs Plan variance", ar: "تسجيل انحراف الفعلي مقابل الخطة" },
  input: ActualPlanVarianceInput, output: VarianceRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "slot", type: ActivePlanLine, key: { property: { from: "input", property: "slot_id" } } },
    { name: "plan_line", type: PlanLine, key: { property: { from: "slot", property: "plan_line_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  aggregates: [
    {
      name: "store_sales_retail",
      type: PosSale,
      filter: { store_id: "store_id", currency: "currency" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: false },
      aggregate: { kind: "sum", property: "net_revenue" },
      maxItems: 200,
    },
    {
      name: "store_sales_qty",
      type: PosSale,
      filter: { store_id: "store_id", unit: "unit" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: false },
      aggregate: { kind: "sum", property: "sold_qty" },
      maxItems: 200,
    },
  ],
  derive: [
    { field: "as_of", inputRequired: false, expression: Norn.commitTime() },
    { field: "plan_version_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ plan_version_id: Schema.String }) }) }), (ctx) => ctx.slice.slot.plan_version_id, { root: "context" }) },
    { field: "plan_line_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ plan_line_id: Schema.String }) }) }), (ctx) => ctx.slice.slot.plan_line_id, { root: "context" }) },
    { field: "brand", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ brand: Schema.String }) }) }), (ctx) => ctx.slice.slot.brand, { root: "context" }) },
    { field: "plan_month", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ plan_month: Schema.String }) }) }), (ctx) => ctx.slice.slot.plan_month, { root: "context" }) },
    { field: "planned_sales_retail", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_retail: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.plan_line.planned_sales_retail, { root: "context" }) },
    {
      field: "sales_retail_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ actual_sales_retail: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_retail: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.actual_sales_retail - ctx.slice.plan_line.planned_sales_retail,
        { root: "context" },
      ),
    },
    { field: "planned_sales_cost", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_cost: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.plan_line.planned_sales_cost, { root: "context" }) },
    {
      field: "sales_cost_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ cost_status: CostStatus, actual_sales_cost: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_cost: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.cost_status === "costed" ? (ctx.input.actual_sales_cost - ctx.slice.plan_line.planned_sales_cost) : 0,
        { root: "context" },
      ),
    },
    { field: "planned_sales_qty", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_qty: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.plan_line.planned_sales_qty, { root: "context" }) },
    {
      field: "sales_qty_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ actual_sales_qty: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_qty: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.actual_sales_qty - ctx.slice.plan_line.planned_sales_qty,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: ActualPlanVarianceInput,
      slice: Schema.Struct({
        slot: Schema.Struct({
          entity_id: Schema.String, store_id: Schema.String, category: Schema.String,
          currency: Schema.String, unit: Schema.String,
        }),
        plan_line: Schema.Struct({ period_start: Norn.timestamp("utc") }),
      }),
      aggregates: Schema.Struct({ store_sales_retail: Norn.decimal(6), store_sales_qty: Norn.decimal(6) }),
    }),
    (ctx) =>
      ctx.input.entity_id === ctx.slice.slot.entity_id
      && ctx.input.store_id === ctx.slice.slot.store_id
      && ctx.input.category === ctx.slice.slot.category
      && ctx.input.currency === ctx.slice.slot.currency
      && ctx.input.unit === ctx.slice.slot.unit
      && Norn.parseTimestamp(ctx.input.period_start) === Norn.parseTimestamp(ctx.slice.plan_line.period_start)
      && ctx.input.actual_sales_retail <= ctx.aggregates.store_sales_retail
      && ctx.input.actual_sales_qty <= ctx.aggregates.store_sales_qty,
    { root: "context" },
  )],
  createLinks: [
    { link: "baseline", targetType: ActivePlanLine, targetIdField: "slot_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: ActualPlanVariance, change: "created" }],
  effect: { kind: "upsert_input_object", type: ActualPlanVariance, objectIdField: "variance_id", writeMode: { kind: "create_only" } },
})

/**
 * Refresh an existing `Actual - Plan` variance snapshot in-place (`update_only`)
 * after a late `PosSale` restatement or baseline supersession, preserving
 * prior knowledge-time incarnations for `evaluate_question_known_at`.
 */
export const RefreshActualPlanVariance = Norn.action({
  id: "plan.variance.refresh_actual_vs_plan", title: "Refresh an Actual minus Plan variance snapshot", version: "0.1.0",
  label: { en: "Refresh Actual vs Plan variance", ar: "تحديث انحراف الفعلي مقابل الخطة" },
  input: ActualPlanVarianceInput, output: VarianceRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "existing", type: ActualPlanVariance, key: { property: { from: "input", property: "variance_id" } } },
    { name: "slot", type: ActivePlanLine, key: { property: { from: "input", property: "slot_id" } } },
    { name: "plan_line", type: PlanLine, key: { property: { from: "slot", property: "plan_line_id" } } },
  ],
  aggregates: [
    {
      name: "store_sales_retail",
      type: PosSale,
      filter: { store_id: "store_id", currency: "currency" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: false },
      aggregate: { kind: "sum", property: "net_revenue" },
      maxItems: 200,
    },
    {
      name: "store_sales_qty",
      type: PosSale,
      filter: { store_id: "store_id", unit: "unit" },
      window: { property: "sold_at", start: "period_start", end: "period_end", startInclusive: true, endInclusive: false },
      aggregate: { kind: "sum", property: "sold_qty" },
      maxItems: 200,
    },
  ],
  derive: [
    { field: "as_of", inputRequired: false, expression: Norn.commitTime() },
    { field: "plan_version_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ plan_version_id: Schema.String }) }) }), (ctx) => ctx.slice.slot.plan_version_id, { root: "context" }) },
    { field: "plan_line_id", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ plan_line_id: Schema.String }) }) }), (ctx) => ctx.slice.slot.plan_line_id, { root: "context" }) },
    { field: "brand", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ brand: Schema.String }) }) }), (ctx) => ctx.slice.slot.brand, { root: "context" }) },
    { field: "plan_month", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ slot: Schema.Struct({ plan_month: Schema.String }) }) }), (ctx) => ctx.slice.slot.plan_month, { root: "context" }) },
    { field: "planned_sales_retail", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_retail: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.plan_line.planned_sales_retail, { root: "context" }) },
    {
      field: "sales_retail_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ actual_sales_retail: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_retail: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.actual_sales_retail - ctx.slice.plan_line.planned_sales_retail,
        { root: "context" },
      ),
    },
    { field: "planned_sales_cost", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_cost: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.plan_line.planned_sales_cost, { root: "context" }) },
    {
      field: "sales_cost_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ cost_status: CostStatus, actual_sales_cost: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_cost: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.cost_status === "costed" ? (ctx.input.actual_sales_cost - ctx.slice.plan_line.planned_sales_cost) : 0,
        { root: "context" },
      ),
    },
    { field: "planned_sales_qty", inputRequired: false, expression: Norn.expression(Schema.Struct({ slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_qty: Norn.decimal(6) }) }) }), (ctx) => ctx.slice.plan_line.planned_sales_qty, { root: "context" }) },
    {
      field: "sales_qty_variance", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ actual_sales_qty: Norn.decimal(6) }),
          slice: Schema.Struct({ plan_line: Schema.Struct({ planned_sales_qty: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.input.actual_sales_qty - ctx.slice.plan_line.planned_sales_qty,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: ActualPlanVarianceInput,
      slice: Schema.Struct({
        existing: Schema.Struct({ slot_id: Schema.String }),
        slot: Schema.Struct({
          entity_id: Schema.String, store_id: Schema.String, category: Schema.String,
          currency: Schema.String, unit: Schema.String,
        }),
        plan_line: Schema.Struct({ period_start: Norn.timestamp("utc") }),
      }),
      aggregates: Schema.Struct({ store_sales_retail: Norn.decimal(6), store_sales_qty: Norn.decimal(6) }),
    }),
    (ctx) =>
      ctx.slice.existing.slot_id === ctx.input.slot_id
      && ctx.input.entity_id === ctx.slice.slot.entity_id
      && ctx.input.store_id === ctx.slice.slot.store_id
      && ctx.input.category === ctx.slice.slot.category
      && ctx.input.currency === ctx.slice.slot.currency
      && ctx.input.unit === ctx.slice.slot.unit
      && Norn.parseTimestamp(ctx.input.period_start) === Norn.parseTimestamp(ctx.slice.plan_line.period_start)
      && ctx.input.actual_sales_retail <= ctx.aggregates.store_sales_retail
      && ctx.input.actual_sales_qty <= ctx.aggregates.store_sales_qty,
    { root: "context" },
  )],
  writes: [{ type: ActualPlanVariance, change: "modified" }],
  effect: { kind: "upsert_input_object", type: ActualPlanVariance, objectIdField: "variance_id", writeMode: { kind: "update_only" } },
})

// ------------------------------------------------------------------- access

const org = { dimensions: ["org", "environment"] } as const
const PlanReaders = ["planner", "merch_director", "ops_manager", "warehouse_clerk", "data_engineer"] as const
export const PlanAccess = Norn.access({
  id: "plan.access", title: "Merchandise planning and OTB governance", version: "0.1.0",
  rules: [
    {
      ruleId: "sku_profile",
      actions: [RegisterSkuProfile],
      resourceType: SkuProfile, scope: org,
      allOf: [{ role: { anyOf: ["data_engineer", "planner"] } }],
    },
    {
      ruleId: "po_destination",
      actions: [AssignPoDestination],
      resourceType: PoDestination, scope: org,
      allOf: [{ role: { anyOf: ["planner", "ops_manager"] } }],
    },
    {
      ruleId: "author_plan_version",
      actions: [CreatePlanVersion, CreateSupersedingPlanVersion, UpdatePlanVersionDraft, SubmitPlanVersion],
      resourceType: PlanVersion, scope: org,
      allOf: [{ role: { anyOf: ["planner"] } }],
    },
    {
      ruleId: "govern_plan_version",
      actions: [ApprovePlanVersion, LockPlanVersion, SupersedePlanVersion],
      resourceType: PlanVersion, scope: org,
      allOf: [{ role: { anyOf: ["merch_director", "ops_manager"] } }],
    },
    {
      ruleId: "spread_target",
      actions: [CreateSpreadTarget, SealSpreadTarget],
      resourceType: PlanSpreadTarget, scope: org,
      allOf: [{ role: { anyOf: ["planner", "merch_director"] } }],
    },
    {
      ruleId: "plan_lines",
      actions: [RecordPlanLine, RecordSpreadPlanLine],
      resourceType: PlanLine, scope: org,
      allOf: [{ role: { anyOf: ["planner"] } }],
    },
    {
      ruleId: "plan_baseline",
      actions: [ActivatePlanBaseline, SupersedePlanBaseline],
      resourceType: ActivePlanLine, scope: org,
      allOf: [{ role: { anyOf: ["merch_director", "ops_manager", "planner"] } }],
    },
    {
      ruleId: "restate_actual_sale",
      actions: [RestatePosSale],
      resourceType: PosSale, scope: org,
      allOf: [{ role: { anyOf: ["ops_manager", "merch_director"] } }],
    },
    {
      ruleId: "actual_plan_variance",
      actions: [RecordActualPlanVariance, RefreshActualPlanVariance],
      resourceType: ActualPlanVariance, scope: org,
      allOf: [{ role: { anyOf: ["planner", "merch_director", "ops_manager"] } }],
    },
    {
      ruleId: "request_envelope",
      actions: [CreatePlannedEnvelope, ActivatePlannedEnvelope, RequestUnplannedEnvelope, SubmitUnplannedOverride],
      resourceType: OtbEnvelope, scope: org,
      allOf: [{ role: { anyOf: ["planner"] } }],
    },
    {
      ruleId: "approve_envelope_override",
      actions: [ApproveUnplannedOverride, CloseEnvelope],
      resourceType: OtbEnvelope, scope: org,
      allOf: [{ role: { anyOf: ["merch_director", "ops_manager"] } }],
    },
    {
      ruleId: "otb_authorize_po",
      actions: [AuthorizePoLineAgainstOtb],
      resourceType: PurchaseOrder, scope: org,
      allOf: [{ role: { anyOf: ["ops_manager", "merch_director"] } }],
    },
    {
      ruleId: "otb_cancel_release",
      actions: [ReleaseCancelledPoOtb],
      resourceType: OtbPoBinding, scope: org,
      allOf: [{ role: { anyOf: ["ops_manager", "merch_director"] } }],
    },
    {
      ruleId: "otb_receive",
      actions: [RecordOtbPartialReceipt, UpdateOtbPartialReceipt, FulfilOtbPoBinding],
      resourceType: OtbPoBinding, scope: org,
      allOf: [{ role: { anyOf: ["warehouse_clerk", "ops_manager"] } }],
    },
    {
      ruleId: "published_forecast",
      actions: [RecordPublishedForecast, PublishForecast, SupersedeForecast],
      resourceType: PublishedForecast, scope: org,
      allOf: [{ role: { anyOf: ["data_engineer", "planner"] } }],
    },
    // Aligned read rules (`norn.read`) ensuring `plan.OtbEnvelope` and
    // `plan.OtbPoBinding` share identical read authority so `plan.otb_position`
    // can never be read with envelopes visible and bindings hidden.
    { ruleId: "read_otb_envelope", actions: ["norn.read"], resourceType: OtbEnvelope, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_otb_po_binding", actions: ["norn.read"], resourceType: OtbPoBinding, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_plan_version", actions: ["norn.read"], resourceType: PlanVersion, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_plan_spread_target", actions: ["norn.read"], resourceType: PlanSpreadTarget, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_plan_line", actions: ["norn.read"], resourceType: PlanLine, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_active_plan_line", actions: ["norn.read"], resourceType: ActivePlanLine, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_sku_profile", actions: ["norn.read"], resourceType: SkuProfile, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_po_destination", actions: ["norn.read"], resourceType: PoDestination, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_published_forecast", actions: ["norn.read"], resourceType: PublishedForecast, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
    { ruleId: "read_actual_plan_variance", actions: ["norn.read"], resourceType: ActualPlanVariance, scope: org, allOf: [{ role: { anyOf: PlanReaders } }] },
  ],
})
