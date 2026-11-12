import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// What `norn_regression.check_regression` records: one model spec per
// (type, target, features, order field, dataset revision), and one row per
// assumption check, linked to its spec. Written only through these actions.
const Status = Schema.Literals(["pass", "warn", "fail", "not_applicable"])
const Summary = Schema.Literals(["pass", "warn", "fail"])
const Assumption = Schema.Literals(["missingness", "sanity", "linearity", "normality", "homoscedasticity", "independence", "multicollinearity", "influence"])

const SpecFields = Schema.Struct({
  spec: Schema.String,
  object_type: Schema.String,
  target: Schema.String,
  features: Schema.String,
  order_field: Schema.NullOr(Schema.String), // null when no order field was named
  dataset_revision: Schema.String,
  rows: Schema.Int,
  rows_used: Schema.Int,
  method: Schema.String,
  summary: Summary,
  request: Schema.NullOr(Schema.String), // the stats.CheckRequest that asked for it; null for a run from Python
})

export const ModelSpec = Norn.type({
  name: "stats.ModelSpec", title: "Regression model spec", version: "0.1.0",
  properties: SpecFields,
  identity: [{ name: "primary", properties: ["spec"] }],
  // A spec run for a Studio request links to it: request ↔ spec ↔ checks.
  links: [{ id: "request", target: "stats.CheckRequest", cardinality: { to: "one", from: "many" } }],
})

const CheckFields = Schema.Struct({
  check: Schema.String,
  spec: Schema.String,
  assumption: Assumption,
  test: Schema.String,
  subject: Schema.String,
  status: Status,
  statistic: Schema.String,
  p_value: Schema.NullOr(Schema.String), // null for a check without a p-value
  threshold: Schema.String,
  threshold_source: Schema.String,
  remediation: Schema.String,
})

export const AssumptionCheck = Norn.type({
  name: "stats.AssumptionCheck", title: "Regression assumption check", version: "0.1.0",
  properties: CheckFields,
  identity: [{ name: "primary", properties: ["check"] }],
  links: [{ id: "model", target: "stats.ModelSpec", cardinality: { to: "one", from: "many" } }],
})

// A spec the worker records for a request: created with its link to that request.
const RequestedSpecFields = Schema.Struct({
  spec: Schema.String,
  object_type: Schema.String,
  target: Schema.String,
  features: Schema.String,
  order_field: Schema.NullOr(Schema.String),
  dataset_revision: Schema.String,
  rows: Schema.Int,
  rows_used: Schema.Int,
  method: Schema.String,
  summary: Summary,
  request: Schema.String,
})

export const RecordRequestedModelSpec = Norn.action({
  id: "stats.modelspec.record_for_request", title: "Record a requested regression model spec", version: "0.1.0",
  input: RequestedSpecFields,
  output: Schema.Struct({ spec: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  slice: [{ name: "request", type: "stats.CheckRequest", key: { property: { from: "input", property: "request" } } }],
  createLinks: [{ link: "request", targetType: "stats.CheckRequest", targetIdField: "request" }],
  writes: [{ type: ModelSpec, change: "created" }],
  effect: { kind: "upsert_input_object", type: ModelSpec, objectIdField: "spec", writeMode: { kind: "create_only" } },
})

export const RecordModelSpec = Norn.action({
  id: "stats.modelspec.record", title: "Record a regression model spec", version: "0.1.0",
  input: SpecFields,
  output: Schema.Struct({ spec: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  writes: [{ type: ModelSpec, change: "created" }],
  effect: { kind: "upsert_input_object", type: ModelSpec, objectIdField: "spec", writeMode: { kind: "create_only" } },
})

export const RecordAssumptionCheck = Norn.action({
  id: "stats.assumptioncheck.record", title: "Record a regression assumption check", version: "0.1.0",
  input: CheckFields,
  output: Schema.Struct({ check: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  writes: [{ type: AssumptionCheck, change: "created" }],
  // The spec must exist: it is read by its key in the same commit, and linked (link "model").
  slice: [{ name: "spec", type: ModelSpec, key: { property: { from: "input", property: "spec" } } }],
  createLinks: [{ link: "model", targetType: ModelSpec, targetIdField: "spec" }],
  effect: { kind: "upsert_input_object", type: AssumptionCheck, objectIdField: "check", writeMode: { kind: "create_only" } },
})

// One click in Studio: a queued request a server-side worker runs
// (norn_regression.worker). The worker reports progress and the result
// through stats.checkrequest.finish; the request keeps its history in receipts.
const RequestFields = Schema.Struct({
  request: Schema.String,
  object_type: Schema.String,
  target: Schema.String,
  features: Schema.String, // comma-joined
  order_field: Schema.NullOr(Schema.String), // null when none
  status: Schema.Literals(["queued", "running", "done", "failed"]),
  spec: Schema.NullOr(Schema.String), // the stats.ModelSpec id once done, else null
  error: Schema.NullOr(Schema.String), // why it failed, else null
})

export const CheckRequest = Norn.type({
  name: "stats.CheckRequest", title: "Regression check request", version: "0.1.0",
  properties: RequestFields,
  identity: [{ name: "primary", properties: ["request"] }],
  // A new request is queued; spec and error stay null (absent) until the worker sets them.
  defaults: { status: "queued" },
})

// The click: the person names the dataset and the model; Norn sets the rest.
const SubmitFields = Schema.Struct({
  request: Schema.String,
  object_type: Schema.String,
  target: Schema.String,
  features: Schema.String,
  order_field: Schema.NullOr(Schema.String),
})

export const SubmitCheckRequest = Norn.action({
  id: "stats.checkrequest.submit", title: "Check regression assumptions", version: "0.1.0",
  input: SubmitFields,
  output: Schema.Struct({ request: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  inputLabels: {
    request: { title: "Request id", description: "A name for this run, unique in the cell, e.g. rq-sales-2026-11." },
    object_type: { title: "Dataset", description: "The object type to check, e.g. lab.Sample or an uploaded CSV's type." },
    target: { title: "Target", description: "The numeric field the model predicts." },
    features: { title: "Features", description: "Comma-separated numeric fields, e.g. x1,x2,x3." },
  },
  inputGroups: [{ id: "what", title: "What to check", fields: ["/request", "/object_type", "/target", "/features", "/order_field"] }],
  writes: [{ type: CheckRequest, change: "created" }],
  effect: { kind: "upsert_input_object", type: CheckRequest, objectIdField: "request", writeMode: { kind: "create_only" } },
})

// The worker's progress: running, done with the spec it recorded, or failed with the reason.
export const FinishCheckRequest = Norn.action({
  id: "stats.checkrequest.finish", title: "Report a regression check request's progress", version: "0.1.0",
  input: RequestFields,
  output: Schema.Struct({ request: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  writes: [{ type: CheckRequest, change: "created" }, { type: CheckRequest, change: "modified" }],
  effect: { kind: "upsert_input_object", type: CheckRequest, objectIdField: "request", writeMode: { kind: "upsert" } },
})

// Who may call the stats actions on a hosted cell: a member (the worker's
// Studio API key holds role member) or above, in the request's org and
// environment. On a local dev host the session token is the owner.
export const StatsAccess = Norn.access({
  id: "stats.regression.access",
  title: "Regression assumption checks",
  version: "0.1.0",
  rules: [
    {
      ruleId: "request",
      actions: [SubmitCheckRequest],
      resourceType: CheckRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }],
    },
    {
      ruleId: "report",
      actions: [FinishCheckRequest],
      resourceType: CheckRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }],
    },
    {
      ruleId: "record-spec",
      actions: [RecordModelSpec, RecordRequestedModelSpec],
      resourceType: ModelSpec,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }],
    },
    {
      ruleId: "record-check",
      actions: [RecordAssumptionCheck],
      resourceType: AssumptionCheck,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }],
    },
  ],
})
