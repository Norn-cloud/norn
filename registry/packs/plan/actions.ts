import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { OrderLine, PurchaseOrder, ShipmentReceipt } from "../supply-lab/supply.ts"
import { LegalEntity, Store } from "../supply-lab/retail.ts"
import {
  OtbEnvelope, OtbPoBinding, PlanLine, PlanSpreadTarget, PlanVersion, PublishedForecast,
} from "./plan.ts"

// Governed writers and access rules for the S2 Plan & Open-to-Buy pack.
// Every server-derived input field (`fromCaller` / `inputRequired: false`) obeys
// ORIGIN-DATAFLOW-1: callers omit them and the kernel derives/stamps them.

const Scope = ["orgId", "environmentId"] as const
const Access = { artifactId: "plan.access.policy", version: "0.1.0" } as const
const Currency = Schema.Literals(["EGP", "USD", "EUR"])
const PlanUnit = Schema.Literals(["each", "kg", "l", "g"])
const SupplyUnit = Schema.Literals(["each", "kg", "l"])
const RoundingRule = Schema.Literals(["largest_remainder", "last_bucket_remainder", "exact_no_remainder"])
const EnvelopeKind = Schema.Literals(["planned", "unplanned"])
const CoreUnitsDep = [{ artifactId: "core.units", version: "1.0.0", kind: "entity" as const }]

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
const SupersedePlanVersionInput = Schema.Struct({
  plan_version_id: Schema.String,
  successor_version_id: Schema.String,
})

/** Create an initial draft PlanVersion (revision 1, no predecessor). */
export const CreatePlanVersion = Norn.action({
  id: "plan.version.create", title: "Create a draft plan version", version: "0.1.0",
  label: { en: "Create plan version", ar: "إنشاء إصدار خطة" },
  input: CreatePlanVersionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["created_by"],
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
 * PlanVersion without mutating locked history.
 */
export const CreateSupersedingPlanVersion = Norn.action({
  id: "plan.version.create_revision", title: "Create a superseding draft plan version", version: "0.1.0",
  label: { en: "Create superseding plan version", ar: "إنشاء مراجعة جديدة للخطة" },
  input: CreateRevisionInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["created_by"],
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
 * Update bitemporal window metadata on an editable PlanVersion.
 * Refuses post-lock mutation via the lifecycle `immutable: ["locked", "superseded"]`
 * contract (`lifecycle.immutable_state`).
 */
export const UpdatePlanVersionDraft = Norn.action({
  id: "plan.version.update_draft", title: "Update a draft plan version", version: "0.1.0",
  label: { en: "Update draft plan version", ar: "تحديث مسودة الخطة" },
  input: UpdatePlanVersionDraftInput, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "upsert_input_object", type: PlanVersion, objectIdField: "plan_version_id", writeMode: { kind: "update_only" } },
})

export const SubmitPlanVersion = Norn.action({
  id: "plan.version.submit", title: "Submit a plan version for approval", version: "0.1.0",
  label: { en: "Submit plan version", ar: "تقديم الخطة للاعتماد" },
  input: PlanVersionRef, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanVersion, objectIdField: "plan_version_id", lifecycle: "status", transition: "submit" },
})

/** Separation of duties: the principal who drafted a plan version cannot approve it. */
export const ApprovePlanVersion = Norn.action({
  id: "plan.version.approve", title: "Approve a submitted plan version", version: "0.1.0",
  label: { en: "Approve plan version", ar: "اعتماد إصدار الخطة" },
  input: PlanVersionRef, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PlanVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: PlanVersion, objectIdField: "plan_version_id", lifecycle: "status", transition: "approve" },
  independentOf: ["created_by"],
})

export const LockPlanVersion = Norn.action({
  id: "plan.version.lock", title: "Lock an approved plan version", version: "0.1.0",
  label: { en: "Lock plan version", ar: "إقفال إصدار الخطة" },
  input: PlanVersionRef, output: PlanVersionRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
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
  currency: Currency,
  rounding_rule: RoundingRule,
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
 * Seal a top-down spread target once its child `PlanLine`s sum exactly to every
 * parent target measure and explicit rounding remainder (`TXN-PHANTOM` fenced
 * via `aggregates` + `SpreadTargetConservation` law).
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
      ctx.aggregates.child_count > 0
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
      && ctx.input.spread_remainder_cost === 0
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

/** Record a child leaf PlanLine spread from an open PlanSpreadTarget. */
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
    }),
    (ctx) =>
      ctx.slice.version.status === "draft"
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

export const ActivatePlannedEnvelope = Norn.action({
  id: "plan.otb_envelope.activate_planned", title: "Activate a planned OTB envelope", version: "0.1.0",
  label: { en: "Activate planned OTB envelope", ar: "تفعيل مظروف الميزانية المخطط" },
  input: EnvelopeRef, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({ slice: Schema.Struct({ envelope: Schema.Struct({ envelope_kind: Schema.String }) }) }),
    (ctx) => ctx.slice.envelope.envelope_kind === "planned",
    { root: "context" },
  )],
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: { kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id", lifecycle: "status", transition: "activate_planned" },
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
 * approve their own unplanned budget override.
 */
export const ApproveUnplannedOverride = Norn.action({
  id: "plan.otb_envelope.approve_override", title: "Approve an unplanned OTB override envelope", version: "0.1.0",
  label: { en: "Approve unplanned OTB override", ar: "اعتماد استثناء الميزانية" },
  input: EnvelopeRef, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [{ name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } }],
  preconditions: [Norn.expression(
    Schema.Struct({ slice: Schema.Struct({ envelope: Schema.Struct({ envelope_kind: Schema.String }) }) }),
    (ctx) => ctx.slice.envelope.envelope_kind === "unplanned",
    { root: "context" },
  )],
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: { kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id", lifecycle: "status", transition: "approve_override" },
  independentOf: ["requested_by"],
})

export const CloseEnvelope = Norn.action({
  id: "plan.otb_envelope.close", title: "Close an OTB budget envelope", version: "0.1.0",
  label: { en: "Close OTB envelope", ar: "إغلاق مظروف الميزانية" },
  input: EnvelopeRef, output: EnvelopeRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: OtbEnvelope, change: "modified" }],
  effect: { kind: "fire_transition", type: OtbEnvelope, objectIdField: "envelope_id", lifecycle: "status", transition: "close" },
})

// ----------------------------------------- fail-closed OTB PO authorization

const AuthorizePoLineInput = Schema.Struct({
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
  expected_payment_date: Norn.timestamp("utc"),
  payment_terms_days: Schema.Int,
  as_of: Norn.timestamp("utc"),
  authorized_by: Schema.String,
})
const BindingRef = Schema.Struct({ binding_id: Schema.String })

/**
 * Fail-closed OTB guard at PO line authorization:
 * - Refuses when no `OtbEnvelope` exists for the target category/period (`slice`).
 * - Refuses when the envelope is not `active` (blocks unapproved `unplanned` envelopes).
 * - Fences `plan.OtbPoBinding` by `envelope_id` via `aggregates` (`TXN-PHANTOM`)
 *   and refuses any PO authorization where `consumed_otb_cost + authorized_cost > approved_budget_cost`.
 * - Derives all OTB cost buckets and cap snapshots in the kernel (`ORIGIN-DATAFLOW-1`).
 */
export const AuthorizePoLineAgainstOtb = Norn.action({
  id: "plan.otb.authorize_po_line", title: "Authorize a PO line against an OTB envelope", version: "0.1.0",
  label: { en: "Authorize PO line (OTB)", ar: "اعتماد بند أمر الشراء ضمن الميزانية" },
  input: AuthorizePoLineInput, output: BindingRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  fromCaller: ["authorized_by"],
  slice: [
    { name: "envelope", type: OtbEnvelope, key: { property: { from: "input", property: "envelope_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
    { name: "order_line", type: OrderLine, key: { property: { from: "input", property: "line_id" } } },
    { name: "owner_entity", type: LegalEntity, key: { property: { from: "input", property: "entity_id" } } },
    { name: "store", type: Store, key: { property: { from: "input", property: "store_id" } } },
  ],
  aggregates: [{
    name: "consumed_otb_cost",
    type: OtbPoBinding,
    filter: { envelope_id: "envelope_id" },
    aggregate: { kind: "sum", property: "otb_consumed_cost" },
    maxItems: 200,
  }],
  derive: [
    { field: "received_qty", inputRequired: false, expression: Norn.expression(AuthorizePoLineInput, () => 0) },
    { field: "recognized_receipt_cost", inputRequired: false, expression: Norn.expression(AuthorizePoLineInput, () => 0) },
    { field: "cancelled_released_cost", inputRequired: false, expression: Norn.expression(AuthorizePoLineInput, () => 0) },
    {
      field: "ordered_qty", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ order_line: Schema.Struct({ ordered_qty: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.order_line.ordered_qty,
        { root: "context" },
      ),
    },
    {
      field: "authorized_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ unit_cost: Norn.decimal(4) }),
          slice: Schema.Struct({ order_line: Schema.Struct({ ordered_qty: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.slice.order_line.ordered_qty * ctx.input.unit_cost,
        { root: "context" },
      ),
    },
    {
      field: "unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ unit_cost: Norn.decimal(4) }),
          slice: Schema.Struct({ order_line: Schema.Struct({ ordered_qty: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.slice.order_line.ordered_qty * ctx.input.unit_cost,
        { root: "context" },
      ),
    },
    {
      field: "otb_consumed_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          input: Schema.Struct({ unit_cost: Norn.decimal(4) }),
          slice: Schema.Struct({ order_line: Schema.Struct({ ordered_qty: Norn.decimal(6) }) }),
        }),
        (ctx) => ctx.slice.order_line.ordered_qty * ctx.input.unit_cost,
        { root: "context" },
      ),
    },
    {
      field: "envelope_cap_snapshot", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ envelope: Schema.Struct({ approved_budget_cost: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.envelope.approved_budget_cost,
        { root: "context" },
      ),
    },
    {
      field: "prior_consumed_snapshot", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ aggregates: Schema.Struct({ consumed_otb_cost: Norn.decimal(6) }) }),
        (ctx) => ctx.aggregates.consumed_otb_cost,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: AuthorizePoLineInput,
      slice: Schema.Struct({
        envelope: Schema.Struct({
          status: Schema.String, plan_version_id: Schema.String, entity_id: Schema.String,
          store_id: Schema.String, category: Schema.String, brand: Schema.String,
          plan_month: Schema.String, currency: Schema.String, approved_budget_cost: Norn.decimal(6),
        }),
        order: Schema.Struct({ status: Schema.String }),
        order_line: Schema.Struct({ po_id: Schema.String, unit: Schema.String, currency: Schema.String, received_qty: Norn.decimal(6) }),
      }),
      aggregates: Schema.Struct({ consumed_otb_cost: Norn.decimal(6) }),
    }),
    (ctx) =>
      ctx.slice.envelope.status === "active"
      && (ctx.slice.order.status === "draft" || ctx.slice.order.status === "submitted")
      && ctx.slice.order_line.po_id === ctx.input.po_id
      && ctx.slice.order_line.received_qty === 0
      && ctx.input.unit === ctx.slice.order_line.unit
      && ctx.input.currency === ctx.slice.order_line.currency
      && ctx.input.currency === ctx.slice.envelope.currency
      && ctx.input.plan_version_id === ctx.slice.envelope.plan_version_id
      && ctx.input.entity_id === ctx.slice.envelope.entity_id
      && ctx.input.store_id === ctx.slice.envelope.store_id
      && ctx.input.category === ctx.slice.envelope.category
      && ctx.input.brand === ctx.slice.envelope.brand
      && ctx.input.plan_month === ctx.slice.envelope.plan_month
      && (ctx.aggregates.consumed_otb_cost + ctx.input.authorized_cost) <= ctx.slice.envelope.approved_budget_cost,
    { root: "context" },
  )],
  createLinks: [
    { link: "envelope", targetType: OtbEnvelope, targetIdField: "envelope_id" },
    { link: "order", targetType: PurchaseOrder, targetIdField: "po_id" },
    { link: "order_line", targetType: OrderLine, targetIdField: "line_id" },
    { link: "owner_entity", targetType: LegalEntity, targetIdField: "entity_id" },
    { link: "store", targetType: Store, targetIdField: "store_id" },
  ],
  writes: [{ type: OtbPoBinding, change: "created" }],
  effect: { kind: "upsert_input_object", type: OtbPoBinding, objectIdField: "binding_id", writeMode: { kind: "create_only" } },
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
 * `recognized_receipt_cost` while conserving net `otb_consumed_cost`.
 */
export const RecordOtbPartialReceipt = Norn.action({
  id: "plan.otb.record_partial_receipt", title: "Recognize partial receipt against an OTB PO binding", version: "0.1.0",
  label: { en: "Record OTB partial receipt", ar: "تسجيل استلام جزئي على الميزانية" },
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
        Schema.Struct({
          slice: Schema.Struct({
            binding: Schema.Struct({ unit_cost: Norn.decimal(4) }),
            order_line: Schema.Struct({ received_qty: Norn.decimal(6) }),
          }),
        }),
        (ctx) => ctx.slice.order_line.received_qty * ctx.slice.binding.unit_cost,
        { root: "context" },
      ),
    },
    {
      field: "next_unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({
          slice: Schema.Struct({
            binding: Schema.Struct({ authorized_cost: Norn.decimal(6), unit_cost: Norn.decimal(4) }),
            order_line: Schema.Struct({ received_qty: Norn.decimal(6) }),
          }),
        }),
        (ctx) => ctx.slice.binding.authorized_cost - (ctx.slice.order_line.received_qty * ctx.slice.binding.unit_cost),
        { root: "context" },
      ),
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
      && ctx.slice.order_line.received_qty > 0
      && ctx.slice.order_line.received_qty < ctx.slice.binding.ordered_qty,
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
  as_of: Norn.timestamp("utc"),
  next_unreceived_authorized_cost: Norn.decimal(6),
  next_cancelled_released_cost: Norn.decimal(6),
  next_otb_consumed_cost: Norn.decimal(6),
})

/**
 * Release the unreceived OTB balance when the bound `supply.PurchaseOrder` is
 * cancelled (or partially received and remaining unreceived quantity closed):
 * already-recognized receipts remain consumed, while the unreceived balance
 * returns to remaining OTB.
 */
export const ReleaseCancelledPoOtb = Norn.action({
  id: "plan.otb.release_cancelled_po", title: "Release unreceived OTB on PO cancellation", version: "0.1.0",
  label: { en: "Release cancelled PO OTB", ar: "تحرير ميزانية أمر الشراء الملغى" },
  input: CancelPoOtbInput, output: BindingRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  slice: [
    { name: "binding", type: OtbPoBinding, key: { property: { from: "input", property: "binding_id" } } },
    { name: "order", type: PurchaseOrder, key: { property: { from: "input", property: "po_id" } } },
  ],
  derive: [
    {
      field: "next_unreceived_authorized_cost", inputRequired: false,
      expression: Norn.expression(CancelPoOtbInput, () => 0),
    },
    {
      field: "next_cancelled_released_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ binding: Schema.Struct({ unreceived_authorized_cost: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.binding.unreceived_authorized_cost,
        { root: "context" },
      ),
    },
    {
      field: "next_otb_consumed_cost", inputRequired: false,
      expression: Norn.expression(
        Schema.Struct({ slice: Schema.Struct({ binding: Schema.Struct({ recognized_receipt_cost: Norn.decimal(6) }) }) }),
        (ctx) => ctx.slice.binding.recognized_receipt_cost,
        { root: "context" },
      ),
    },
  ],
  preconditions: [Norn.expression(
    Schema.Struct({
      input: CancelPoOtbInput,
      slice: Schema.Struct({
        binding: Schema.Struct({ po_id: Schema.String }),
        order: Schema.Struct({ status: Schema.String }),
      }),
    }),
    (ctx) =>
      ctx.slice.binding.po_id === ctx.input.po_id
      && (ctx.slice.order.status === "cancelled" || ctx.slice.order.status === "partially_received"),
    { root: "context" },
  )],
  writes: [{ type: OtbPoBinding, change: "modified" }],
  effect: {
    kind: "fire_transition", type: OtbPoBinding, objectIdField: "binding_id",
    lifecycle: "status", transition: "cancel",
    setFields: {
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
})
const ForecastRef = Schema.Struct({ forecast_id: Schema.String })

/** Record a plan-grain forecast from a production `ml.ModelVersion`. */
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

export const PublishForecast = Norn.action({
  id: "plan.forecast.publish", title: "Publish a plan-grain forecast revision", version: "0.1.0",
  label: { en: "Publish forecast", ar: "نشر التوقع" },
  input: ForecastRef, output: ForecastRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PublishedForecast, change: "modified" }],
  effect: { kind: "fire_transition", type: PublishedForecast, objectIdField: "forecast_id", lifecycle: "status", transition: "publish" },
})

export const SupersedeForecast = Norn.action({
  id: "plan.forecast.supersede", title: "Supersede a published forecast revision", version: "0.1.0",
  label: { en: "Supersede forecast", ar: "استبدال التوقع" },
  input: ForecastRef, output: ForecastRef,
  scopeRequired: Scope, idempotent: true, ledgered: true, access: Access,
  writes: [{ type: PublishedForecast, change: "modified" }],
  effect: { kind: "fire_transition", type: PublishedForecast, objectIdField: "forecast_id", lifecycle: "status", transition: "supersede" },
})

// ------------------------------------------------------------------- access

const org = { dimensions: ["org", "environment"] } as const
export const PlanAccess = Norn.access({
  id: "plan.access", title: "Merchandise planning and OTB governance", version: "0.1.0",
  rules: [
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
      ruleId: "otb_authorize_and_cancel",
      actions: [AuthorizePoLineAgainstOtb, ReleaseCancelledPoOtb],
      resourceType: OtbPoBinding, scope: org,
      allOf: [{ role: { anyOf: ["ops_manager", "merch_director"] } }],
    },
    {
      ruleId: "otb_receive",
      actions: [RecordOtbPartialReceipt, FulfilOtbPoBinding],
      resourceType: OtbPoBinding, scope: org,
      allOf: [{ role: { anyOf: ["warehouse_clerk", "ops_manager"] } }],
    },
    {
      ruleId: "published_forecast",
      actions: [RecordPublishedForecast, PublishForecast, SupersedeForecast],
      resourceType: PublishedForecast, scope: org,
      allOf: [{ role: { anyOf: ["data_engineer", "planner"] } }],
    },
  ],
})
