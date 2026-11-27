import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Model versions as typed, governed objects (#1171). A model's definition is a
// `Norn.model` (ModelIR: input, features, target, typed outputs, trust, output
// contracts); its internals stay opaque. A version is registered once its
// training run and its evaluation run (tracking.Run, by run_id) have finished,
// and receives its evaluation evidence exclusively from an authenticated
// `ml.forecast.evaluate` FunctionReceipt (`RECEIPT-CONSUME-1`), never from
// caller-supplied strings or booleans.

// Content digest of `FORECAST_EVALUATOR_MANIFEST` in
// `sdks/python/regression/norn_regression/receipt.py` (`ml.forecast.evaluate` v1).
const FORECAST_EVALUATOR_MANIFEST_DIGEST =
  "sha256:727d22e10828fff4346a4c231533eda37216d4b74ae5d1264fb4d9b6fcf3fef0"

const Version = Schema.Struct({
  version_id: Schema.String, // "<model id>@<version>"
  model_id: Schema.String, // the Norn.model definition's id
  version: Schema.String,
  status: Schema.Literals(["candidate", "evaluated", "production", "retired"]),
  training_run: Schema.String, // tracking.Run run_id
  evaluation_run: Schema.String, // tracking.Run run_id
  artifact_uri: Schema.String,
  artifact_sha256: Schema.String,
  artifact_size: Schema.Int,
  input_type: Schema.String,
  input_revision: Schema.String, // the training snapshot: "c<commit>" or "l<load>"
  pinned_dataset_revision: Schema.String, // the evaluation dataset this version must be evaluated on
  evaluation_receipt: Schema.NullOr(Schema.String), // authenticated FunctionReceipt content digest
  evaluation_revision: Schema.NullOr(Schema.String), // snapshot read by the authenticated evaluator
  evaluated_artifact_sha256: Schema.NullOr(Schema.String), // SHA-256 of the model bytes scored by the evaluator
  evaluated_artifact_size: Schema.NullOr(Schema.Int), // byte size of the model bytes scored by the evaluator
  metric_name: Schema.NullOr(Schema.String),
  metric_value: Schema.NullOr(Norn.decimal(9)),
  contracts_passed: Schema.NullOr(Schema.Boolean),
})

export const ModelVersionEvidence = Norn.interfaceType({
  name: "ml.ModelVersionEvidence", title: "Evaluated model version", version: "0.1.0",
  properties: Version,
  invariants: [{
    id: "evaluated_on_pinned_dataset",
    message: "A production model version has an authenticated evaluation receipt on its registered artifact and pinned dataset revision, and every output contract held.",
    expression: Norn.expression(
      Version,
      (self) =>
        self.status !== "production" ||
        ((self.contracts_passed ?? false) &&
          self.evaluation_receipt !== null &&
          self.pinned_dataset_revision === self.evaluation_revision &&
          self.artifact_sha256 === self.evaluated_artifact_sha256 &&
          self.artifact_size === self.evaluated_artifact_size),
    ),
  }],
})

// A version as stored: its evidence, and the verified principal that registered
// it (ACTOR-STAMP-1), so promotion and retirement are separated from it.
const StoredVersion = Schema.Struct({
  version_id: Schema.String,
  model_id: Schema.String,
  version: Schema.String,
  status: Schema.Literals(["candidate", "evaluated", "production", "retired"]),
  training_run: Schema.String,
  evaluation_run: Schema.String,
  artifact_uri: Schema.String,
  artifact_sha256: Schema.String,
  artifact_size: Schema.Int,
  input_type: Schema.String,
  input_revision: Schema.String,
  pinned_dataset_revision: Schema.String,
  evaluation_receipt: Schema.NullOr(Schema.String),
  evaluation_revision: Schema.NullOr(Schema.String),
  evaluated_artifact_sha256: Schema.NullOr(Schema.String),
  evaluated_artifact_size: Schema.NullOr(Schema.Int),
  metric_name: Schema.NullOr(Schema.String),
  metric_value: Schema.NullOr(Norn.decimal(9)),
  contracts_passed: Schema.NullOr(Schema.Boolean),
  registered_by: Schema.String,
})

export const ModelVersion = Norn.type({
  name: "ml.ModelVersion", title: "Model version", version: "0.1.0",
  properties: StoredVersion, implements: [ModelVersionEvidence],
  identity: [{ name: "primary", properties: ["version_id"] }],
  // Stamped from the registering principal only; nobody rewrites it.
  protected: ["registered_by"],
  defaults: { status: "candidate" },
  links: [
    { id: "trained_by", target: "tracking.Run", cardinality: { to: "one", from: "many" } },
    { id: "evaluated_by", target: "tracking.Run", cardinality: { to: "one", from: "many" } },
  ],
  lifecycles: [{ property: "status", initial: ["candidate"], terminal: ["retired"], transitions: [
    { id: "accept_evaluation", from: ["candidate"], to: "evaluated" },
    // ANALYSIS-PUBLISH-1: a version without an authenticated evaluation receipt,
    // evaluated on another revision than its pinned one (temporal leakage),
    // scored on different artifact bytes than registered, or failing a contract
    // is refused as a publication guard.
    { id: "promote", from: ["evaluated"], to: "production",
      guards: [Norn.expression(
        Version,
        (self) =>
          (self.contracts_passed ?? false) &&
          self.evaluation_receipt !== null &&
          self.pinned_dataset_revision === self.evaluation_revision &&
          self.artifact_sha256 === self.evaluated_artifact_sha256 &&
          self.artifact_size === self.evaluated_artifact_size,
      )],
      guardRefusalCode: "core.projection.publication_guard" },
    { id: "retire", from: ["candidate", "evaluated", "production"], to: "retired" },
  ] }],
})

const RegisterInput = Schema.Struct({
  version_id: Schema.String, model_id: Schema.String, version: Schema.String,
  training_run: Schema.String, evaluation_run: Schema.String,
  artifact_uri: Schema.String, artifact_sha256: Schema.String, artifact_size: Schema.Int,
  input_type: Schema.String, input_revision: Schema.String,
  pinned_dataset_revision: Schema.String,
  registered_by: Schema.String,
})

export const RegisterModelVersion = Norn.action({
  id: "ml.modelversion.register", title: "Register a model version", version: "0.1.0",
  input: RegisterInput, output: Schema.Struct({ version_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  // The server stamps the registering principal; a caller never supplies it.
  fromCaller: ["registered_by"],
  slice: [
    { name: "training", type: "tracking.Run", key: { property: { from: "input", property: "training_run" } } },
    { name: "evaluation", type: "tracking.Run", key: { property: { from: "input", property: "evaluation_run" } } },
  ],
  // Both runs finished: a version is never registered from a run still writing, or one that failed.
  preconditions: [
    Norn.expression(
      Schema.Struct({
        slice: Schema.Struct({
          training: Schema.Struct({ status: Schema.String }),
          evaluation: Schema.Struct({ status: Schema.String }),
        }),
      }),
      (ctx) => ctx.slice.training.status === "FINISHED" && ctx.slice.evaluation.status === "FINISHED",
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "trained_by", targetType: "tracking.Run", targetIdField: "training_run" },
    { link: "evaluated_by", targetType: "tracking.Run", targetIdField: "evaluation_run" },
  ],
  writes: [{ type: ModelVersion, change: "created" }],
  effect: { kind: "upsert_input_object", type: ModelVersion, objectIdField: "version_id", writeMode: { kind: "create_only" } },
})

/**
 * RECEIPT-CONSUME-1: bind evaluation evidence onto a candidate model version
 * strictly from a host-authenticated `ml.forecast.evaluate` FunctionReceipt.
 */
export const AcceptModelEvaluation = Norn.action({
  id: "ml.modelversion.accept_evaluation", title: "Accept an authenticated model evaluation receipt", version: "0.1.0",
  input: Schema.Struct({ version_id: Schema.String, evaluation_receipt: Schema.String }),
  output: Schema.Struct({ version_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: false, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  writes: [{ type: ModelVersion, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ModelVersion,
    objectIdField: "version_id",
    lifecycle: "status",
    transition: "accept_evaluation",
    acceptReceipt: {
      referenceField: "evaluation_receipt",
      manifestDigest: FORECAST_EVALUATOR_MANIFEST_DIGEST,
      digestProperty: "evaluation_receipt",
      resultBindings: {
        evaluation_revision: "evaluation_revision",
        evaluated_artifact_sha256: "evaluated_artifact_sha256",
        evaluated_artifact_size: "evaluated_artifact_size",
        metric_name: "metric_name",
        metric_value: "metric_value",
        contracts_passed: "contracts_passed",
      },
    },
  },
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
  // Separation of duties (ACTOR-INDEPENDENT-1): whoever registered a version never promotes it.
  independentOf: ["registered_by"],
})

export const RetireModelVersion = Norn.action({
  id: "ml.modelversion.retire", title: "Retire a model version", version: "0.1.0",
  input: Schema.Struct({ version_id: Schema.String }), output: Schema.Struct({ version_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  writes: [{ type: ModelVersion, change: "modified" }],
  effect: { kind: "fire_transition", type: ModelVersion, objectIdField: "version_id", lifecycle: "status", transition: "retire" },
  // Separation of duties: whoever registered a version never retires it either.
  independentOf: ["registered_by"],
})

const Batch = Schema.Struct({
  batch_id: Schema.String,
  model_version: Schema.String, // the ml.ModelVersion version_id applied
  input_type: Schema.String, // the scored object type
  input_revision: Schema.String, // the snapshot of the input rows the batch scored
  rows: Schema.Int, // predictions written, one per input row
  status: Schema.Literals(["recorded"]),
})

/**
 * One application of a promoted model version (#1171 M4): which version
 * scored which input snapshot, and how many predictions it wrote. Each
 * prediction is an object of the model's `predictions` type carrying `subject`
 * and `model_version`; the batch is recorded only after every declared output
 * contract held on every row (`sdks/python/models/norn_models`).
 */
export const PredictionBatch = Norn.type({
  name: "ml.PredictionBatch", title: "Prediction batch", version: "0.1.0",
  properties: Batch,
  identity: [{ name: "primary", properties: ["batch_id"] }],
  defaults: { status: "recorded" },
  links: [
    { id: "applied", target: "ml.ModelVersion", cardinality: { to: "one", from: "many" } },
  ],
})

/** Create-only, and only for a version in production: a candidate or retired version is never applied. */
export const RecordPredictionBatch = Norn.action({
  id: "ml.predictionbatch.record", title: "Record a prediction batch", version: "0.1.0",
  input: Schema.Struct({
    batch_id: Schema.String, model_version: Schema.String, input_type: Schema.String,
    input_revision: Schema.String, rows: Schema.Int,
  }),
  output: Schema.Struct({ batch_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "ml.access.policy", version: "0.1.0" },
  slice: [
    { name: "version", type: "ml.ModelVersion", key: { property: { from: "input", property: "model_version" } } },
  ],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ version: Version }) }), (input) => input.slice.version.status === "production")],
  createLinks: [
    { link: "applied", targetType: "ml.ModelVersion", targetIdField: "model_version" },
  ],
  writes: [{ type: PredictionBatch, change: "created" }],
  effect: { kind: "upsert_input_object", type: PredictionBatch, objectIdField: "batch_id", writeMode: { kind: "create_only" } },
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
// promotion (decided through the approval gate) and record prediction
// batches; admins and owners retire, in the org and environment.
export const MlAccess = Norn.access({
  id: "ml.access", title: "Model versions", version: "0.1.0",
  rules: [
    { ruleId: "register", actions: [RegisterModelVersion], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "accept_evaluation", actions: [AcceptModelEvaluation], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "promote", actions: [PromoteModelVersion], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "apply", actions: [RecordPredictionBatch], resourceType: PredictionBatch,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "retire", actions: [RetireModelVersion], resourceType: ModelVersion,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["admin", "owner"] } }] },
  ],
})
