import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Model versions as typed, governed objects (#1171). A model's definition is a
// `Norn.model` (ModelIR: input, features, target, typed outputs, trust, output
// contracts); its internals stay opaque. A version is registered once its
// training run and its evaluation run (tracking.Run, by run_id) have finished,
// and carries what promotion is decided on as typed values.

const Version = Schema.Struct({
  version_id: Schema.String, // "<model id>@<version>"
  model_id: Schema.String, // the Norn.model definition's id
  version: Schema.String,
  status: Schema.Literals(["candidate", "production", "retired"]),
  training_run: Schema.String, // tracking.Run run_id
  evaluation_run: Schema.String, // tracking.Run run_id
  artifact_uri: Schema.String,
  artifact_sha256: Schema.String,
  artifact_size: Schema.Int,
  input_type: Schema.String,
  input_revision: Schema.String, // the training snapshot: "c<commit>" or "l<load>"
  pinned_dataset_revision: Schema.String, // the evaluation dataset this version must be evaluated on
  evaluation_revision: Schema.String, // the snapshot the evaluation run actually read
  metric_name: Schema.String,
  metric_value: Norn.decimal(9),
  contracts_passed: Schema.Boolean,
})

export const ModelVersionEvidence = Norn.interfaceType({
  name: "ml.ModelVersionEvidence", title: "Evaluated model version", version: "0.1.0",
  properties: Version,
  invariants: [{
    id: "evaluated_on_pinned_dataset",
    message: "A production model version was evaluated on its pinned dataset revision, and every output contract held.",
    expression: Norn.expression(Version, (self) => self.status !== "production" || (self.contracts_passed && self.evaluation_revision === self.pinned_dataset_revision)),
  }],
})

export const ModelVersion = Norn.type({
  name: "ml.ModelVersion", title: "Model version", version: "0.1.0",
  properties: Version, implements: [ModelVersionEvidence],
  identity: [{ name: "primary", properties: ["version_id"] }],
  defaults: { status: "candidate" },
  links: [
    { id: "trained_by", target: "tracking.Run", cardinality: { to: "one", from: "many" } },
    { id: "evaluated_by", target: "tracking.Run", cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [{ property: "status", initial: ["candidate"], terminal: ["retired"], transitions: [
    { id: "promote", from: ["candidate"], to: "production",
      guards: [Norn.expression(Version, (self) => self.contracts_passed && self.evaluation_revision === self.pinned_dataset_revision)] },
    { id: "retire", from: ["candidate", "production"], to: "retired" },
  ] }],
})

const RegisterInput = Schema.Struct({
  version_id: Schema.String, model_id: Schema.String, version: Schema.String,
  training_run: Schema.String, evaluation_run: Schema.String,
  artifact_uri: Schema.String, artifact_sha256: Schema.String, artifact_size: Schema.Int,
  input_type: Schema.String, input_revision: Schema.String,
  pinned_dataset_revision: Schema.String, evaluation_revision: Schema.String,
  metric_name: Schema.String, metric_value: Norn.decimal(9), contracts_passed: Schema.Boolean,
})

export const RegisterModelVersion = Norn.action({
  id: "ml.modelversion.register", title: "Register a model version", version: "0.1.0",
  input: RegisterInput, output: Schema.Struct({ version_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  slice: [
    { name: "training", type: "tracking.Run", key: { property: { from: "input", property: "training_run" } } },
    { name: "evaluation", type: "tracking.Run", key: { property: { from: "input", property: "evaluation_run" } } },
  ],
  // Both runs finished: a version is never registered from a run still writing, or one that failed.
  preconditions: [Norn.unsafeZen<boolean>('slice.training.status == "FINISHED" and slice.evaluation.status == "FINISHED"')],
  createLinks: [
    { link: "trained_by", targetType: "tracking.Run", targetIdField: "training_run" },
    { link: "evaluated_by", targetType: "tracking.Run", targetIdField: "evaluation_run" },
  ],
  writes: [{ type: ModelVersion, change: "created" }],
  effect: { kind: "upsert_input_object", type: ModelVersion, objectIdField: "version_id", writeMode: { kind: "create_only" } },
})

/**
 * Approval-gated (APPROVAL-1): `promote.approval.json` beside this file installs
 * the gate, so an admin or owner decides each call; the promotion law still
 * decides the committed call.
 */
export const PromoteModelVersion = Norn.action({
  id: "ml.modelversion.promote", title: "Promote a model version to production", version: "0.1.0",
  input: Schema.Struct({ version_id: Schema.String }), output: Schema.Struct({ version_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  writes: [{ type: ModelVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: ModelVersion, objectIdField: "version_id", lifecycle: "status", transition: "promote" },
})

export const RetireModelVersion = Norn.action({
  id: "ml.modelversion.retire", title: "Retire a model version", version: "0.1.0",
  input: Schema.Struct({ version_id: Schema.String }), output: Schema.Struct({ version_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  writes: [{ type: ModelVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: ModelVersion, objectIdField: "version_id", lifecycle: "status", transition: "retire" },
})

export const PromotionPolicy = Norn.law({
  id: "ml.promotion_policy.law", title: "No promotion without an evaluation on the pinned dataset", version: "0.1.0",
  law: { lawVersion: "0.1", id: "ml.promotion_policy", owner: "ml",
    severity: "error", rationale: "A model version reaches production only when its evaluation run read the pinned dataset revision and every declared output contract held.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/ml/ml.ts" },
    scope: { root: { kind: "entity", id: "ml.ModelVersion" }, objectTypes: ["ml.ModelVersion"] },
    subject: "ml.ModelVersion", modality: { kind: "always" },
    response: { kind: "invariant", objectType: "ml.ModelVersion", invariantId: "evaluated_on_pinned_dataset" },
  },
  dependencies: [{ artifactId: "ml.ModelVersion.entity", kind: "entity", version: "0.1.0", required: true }],
})

// Who may call the ml actions on a hosted cell: members register, request
// promotion (decided through the approval gate) and retire, in the org and
// environment.
export const MlAccess = Norn.access({
  id: "ml.access", title: "Model versions", version: "0.1.0",
  rules: [
    { ruleId: "register", actions: [RegisterModelVersion], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "promote", actions: [PromoteModelVersion], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "retire", actions: [RetireModelVersion], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["admin", "owner"] } }] },
  ],
})
