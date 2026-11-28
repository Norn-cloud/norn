import { Schema } from "effect"
import { Norn } from "@norn/authoring"

/**
 * Synthetic compartmented need-to-know disclosure benchmark (#1874).
 *
 * Exercises incomparable compartments (`compartment.alpha`, `compartment.beta`)
 * and their conjunction (`compartment.alpha` + `compartment.beta`) across point
 * reads, reveals, action references (`ACTION-REF-IFC-1`), `ORIGIN-DATAFLOW-1`,
 * `LIFE-SOUND`, `RED-TEAM-1`, Cedar policy enforcement, and `SqliteCell` commits.
 * Makes no claim of ordered Bell-LaPadula level dominance.
 */

const Scope = ["orgId", "environmentId"] as const
const Access = { artifactId: "intel.compartment.access.policy", version: "0.1.0" } as const

export const CompartmentMarkings = Norn.markings({
  id: "intel.compartment.markings.entity",
  title: "Compartmented need-to-know markings",
  version: "0.1.0",
  markings: [
    {
      id: "compartment.alpha",
      label: "Compartment Alpha",
      severity: "high",
      category: "confidential.alpha",
      mask: { kind: "redacted" },
    },
    {
      id: "compartment.beta",
      label: "Compartment Beta",
      severity: "high",
      category: "confidential.beta",
      mask: { kind: "redacted" },
    },
  ],
})

const DossierStatus = Schema.Literals(["draft", "active", "archived"])

export const Dossier = Norn.type({
  name: "intel.Dossier",
  title: "Compartmented Dossier",
  version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String,
    code: Schema.String,
    status: DossierStatus,
    created_by: Schema.String,
    alpha_brief: Schema.String,
    beta_brief: Schema.String,
    joint_brief: Schema.String,
  }),
  propertyMarkings: {
    alpha_brief: ["compartment.alpha"],
    beta_brief: ["compartment.beta"],
    joint_brief: ["compartment.alpha", "compartment.beta"],
  },
  protected: ["created_by"],
  origins: {
    created_by: "actor",
    alpha_brief: "caller",
    beta_brief: "caller",
    joint_brief: "caller",
  },
  immutableAfterCreate: ["code", "alpha_brief", "beta_brief", "joint_brief"],
  defaults: { status: "draft" },
  identity: [{ name: "primary", properties: ["id"] }],
  lifecycles: [
    {
      property: "status",
      initial: "draft",
      states: {
        draft: { on: { activate: "active", archive: "archived" } },
        active: { resting: true, on: { archive: "archived" } },
        archived: { final: "completed" },
      },
    },
  ],
})

const DispatchStatus = Schema.Literals(["open", "closed"])

export const Dispatch = Norn.type({
  name: "intel.Dispatch",
  title: "Operational Dispatch",
  version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String,
    dossier_id: Schema.String,
    summary: Schema.String,
    status: DispatchStatus,
    alpha_note: Schema.NullOr(Schema.String),
    beta_note: Schema.NullOr(Schema.String),
    joint_note: Schema.NullOr(Schema.String),
  }),
  propertyMarkings: {
    alpha_note: ["compartment.alpha"],
    beta_note: ["compartment.beta"],
    joint_note: ["compartment.alpha", "compartment.beta"],
  },
  defaults: { status: "open" },
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    {
      id: "dossier",
      target: Dossier,
      cardinality: { to: "one", from: "many" },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: "open",
      states: {
        open: {
          on: {
            close: {
              to: "closed",
              guards: [
                Norn.expression(
                  Schema.Struct({ summary: Schema.String }),
                  (self) => self.summary !== "",
                ),
              ],
            },
          },
        },
        closed: { final: "completed" },
      },
    },
  ],
})

export const DossierCodeNonEmpty = Norn.law({
  id: "intel.dossier.code_non_empty.law",
  title: "Every dossier carries a non-empty code",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "intel.dossier.code_non_empty",
    owner: "intel",
    severity: "error",
    rationale: "Every compartmented dossier requires a non-empty reference code.",
    provenance: {
      source: "authoring_sdk",
      sourcePath: "fixtures/packs/compartmented-ifc/compartmented.ts",
    },
    scope: { root: { kind: "entity", id: "intel.Dossier" }, objectTypes: ["intel.Dossier"] },
    subject: "intel.Dossier",
    condition: "true",
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.code != ''" },
  },
  dependencies: [{ artifactId: "intel.Dossier.entity", kind: "entity", version: "0.1.0", required: true }],
})

const DossierInput = Schema.Struct({
  id: Schema.String,
  code: Schema.String,
  created_by: Schema.String,
  alpha_brief: Schema.String,
  beta_brief: Schema.String,
  joint_brief: Schema.String,
})

const DossierRef = Schema.Struct({ id: Schema.String })

export const RegisterDossier = Norn.action({
  id: "intel.dossier.register",
  title: "Register compartmented dossier",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  fromCaller: ["created_by"],
  input: DossierInput,
  output: DossierRef,
  writes: [{ type: Dossier, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Dossier,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

export const ActivateDossier = Norn.action({
  id: "intel.dossier.activate",
  title: "Activate dossier",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  independentOf: ["created_by"],
  input: DossierRef,
  output: DossierRef,
  writes: [{ type: Dossier, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Dossier,
    objectIdField: "id",
    lifecycle: "status",
    transition: "activate",
  },
})

export const ArchiveDossier = Norn.action({
  id: "intel.dossier.archive",
  title: "Archive dossier",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  independentOf: ["created_by"],
  input: DossierRef,
  output: DossierRef,
  writes: [{ type: Dossier, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Dossier,
    objectIdField: "id",
    lifecycle: "status",
    transition: "archive",
  },
})

const DispatchInput = Schema.Struct({
  id: Schema.String,
  dossier_id: Schema.String,
  summary: Schema.String,
  alpha_note: Schema.NullOr(Schema.String),
  beta_note: Schema.NullOr(Schema.String),
  joint_note: Schema.NullOr(Schema.String),
})

const DispatchRef = Schema.Struct({ id: Schema.String })

/** Positive witness: reads only unmarked `dossier.code`, so `createLinks` commits with 0 compartment taint. */
export const CreateUnmarkedDispatch = Norn.action({
  id: "intel.dispatch.create_unmarked",
  title: "Create unmarked dispatch linked to registered dossier",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: DispatchInput,
  output: DispatchRef,
  slice: [
    {
      name: "dossier",
      type: Dossier,
      key: { property: { from: "input", property: "dossier_id" } },
    },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({ slice: Schema.Struct({ dossier: Schema.Struct({ code: Schema.String }) }) }),
      (ctx) => ctx.slice.dossier.code !== "",
      { root: "context" },
    ),
  ],
  createLinks: [{ link: "dossier", targetType: Dossier, targetIdField: "dossier_id" }],
  writes: [{ type: Dispatch, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Dispatch,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

/** Adversarial predicate-influence witness: reads `compartment.alpha` before `createLinks`, forcing refusal. */
export const CreateAlphaInfluencedDispatch = Norn.action({
  id: "intel.dispatch.create_alpha_influenced",
  title: "Create dispatch conditioned on Compartment Alpha brief",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: DispatchInput,
  output: DispatchRef,
  slice: [
    {
      name: "dossier",
      type: Dossier,
      key: { property: { from: "input", property: "dossier_id" } },
    },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        slice: Schema.Struct({
          dossier: Schema.Struct({ code: Schema.String, alpha_brief: Schema.String }),
        }),
      }),
      (ctx) => ctx.slice.dossier.code !== "" && ctx.slice.dossier.alpha_brief !== "",
      { root: "context" },
    ),
  ],
  createLinks: [{ link: "dossier", targetType: Dossier, targetIdField: "dossier_id" }],
  writes: [{ type: Dispatch, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Dispatch,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

/** Conjunction witness: reads BOTH `alpha_brief` and `beta_brief` without `createLinks`; inherits `alpha ∪ beta`. */
export const CreateJointInfluencedDispatch = Norn.action({
  id: "intel.dispatch.create_joint_influenced",
  title: "Create dispatch conditioned on both Compartment Alpha and Beta briefs",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: DispatchInput,
  output: DispatchRef,
  slice: [
    {
      name: "dossier",
      type: Dossier,
      key: { property: { from: "input", property: "dossier_id" } },
    },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        slice: Schema.Struct({
          dossier: Schema.Struct({
            code: Schema.String,
            alpha_brief: Schema.String,
            beta_brief: Schema.String,
          }),
        }),
      }),
      (ctx) =>
        ctx.slice.dossier.code !== "" &&
        ctx.slice.dossier.alpha_brief !== "" &&
        ctx.slice.dossier.beta_brief !== "",
      { root: "context" },
    ),
  ],
  writes: [{ type: Dispatch, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Dispatch,
    objectIdField: "id",
    writeMode: { kind: "create_only" },
  },
})

export const CloseDispatch = Norn.action({
  id: "intel.dispatch.close",
  title: "Close dispatch",
  version: "0.1.0",
  scopeRequired: Scope,
  idempotent: true,
  ledgered: true,
  access: Access,
  input: DispatchRef,
  output: DispatchRef,
  writes: [{ type: Dispatch, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Dispatch,
    objectIdField: "id",
    lifecycle: "status",
    transition: "close",
  },
})

export const CompartmentAccess = Norn.access({
  id: "intel.compartment.access",
  title: "Compartmented disclosure access policy",
  version: "0.1.0",
  rules: [
    {
      ruleId: "dossier_readers_public",
      actions: ["norn.read"],
      resourceType: Dossier,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["viewer", "clear_alpha", "clear_beta", "clear_ab"] } },
        { resource: { name: "code", op: "ne", value: "RESTRICTED" } },
      ],
    },
    {
      ruleId: "dossier_readers_cleared",
      actions: ["norn.read"],
      resourceType: Dossier,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["clear_alpha", "clear_beta", "clear_ab"] } }],
    },
    {
      ruleId: "dispatch_readers",
      actions: ["norn.read"],
      resourceType: Dispatch,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["viewer", "clear_alpha", "clear_beta", "clear_ab"] } }],
    },
    {
      ruleId: "reveal_alpha",
      actions: ["norn.reveal"],
      resourceType: "norn.Marking",
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["clear_alpha", "clear_ab"] } },
        { marking: { anyOf: ["compartment.alpha"] } },
      ],
    },
    {
      ruleId: "reveal_beta",
      actions: ["norn.reveal"],
      resourceType: "norn.Marking",
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["clear_beta", "clear_ab"] } },
        { marking: { anyOf: ["compartment.beta"] } },
      ],
    },
    {
      ruleId: "dossier_operators",
      actions: [RegisterDossier, ActivateDossier, ArchiveDossier],
      resourceType: Dossier,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["operator", "clear_ab"] } }],
    },
    {
      ruleId: "dispatch_unmarked_writers",
      actions: [CreateUnmarkedDispatch, CloseDispatch],
      resourceType: Dispatch,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["viewer", "operator", "clear_alpha", "clear_beta", "clear_ab"] } }],
    },
    {
      ruleId: "dispatch_alpha_writers",
      actions: [CreateAlphaInfluencedDispatch],
      resourceType: Dispatch,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["clear_alpha", "clear_ab"] } }],
    },
    {
      ruleId: "dispatch_joint_writers",
      actions: [CreateJointInfluencedDispatch],
      resourceType: Dispatch,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["clear_ab"] } }],
    },
  ],
})
