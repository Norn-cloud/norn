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
  objectType: Schema.String,
  target: Schema.String,
  features: Schema.String,
  orderField: Schema.String, // "" when no order field was named
  datasetRevision: Schema.String,
  rows: Schema.Int,
  rowsUsed: Schema.Int,
  method: Schema.String,
  summary: Summary,
})

export const ModelSpec = Norn.type({
  name: "stats.ModelSpec", title: "Regression model spec", version: "0.1.0",
  properties: SpecFields,
  identity: [{ name: "primary", properties: ["spec"] }],
})

const CheckFields = Schema.Struct({
  check: Schema.String,
  spec: Schema.String,
  assumption: Assumption,
  test: Schema.String,
  subject: Schema.String,
  status: Status,
  statistic: Schema.String,
  pValue: Schema.String, // "" for a check without a p-value
  threshold: Schema.String,
  thresholdSource: Schema.String,
  remediation: Schema.String,
})

export const AssumptionCheck = Norn.type({
  name: "stats.AssumptionCheck", title: "Regression assumption check", version: "0.1.0",
  properties: CheckFields,
  identity: [{ name: "primary", properties: ["check"] }],
  links: [{ id: "spec", target: "stats.ModelSpec", cardinality: { to: "one", from: "many" } }],
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
  // The spec must exist: it is read by its key in the same commit, and linked.
  slice: [{ name: "spec", type: ModelSpec, key: { property: { from: "input", property: "spec" } } }],
  createLinks: [{ link: "spec", targetType: ModelSpec, targetIdField: "spec" }],
  effect: { kind: "upsert_input_object", type: AssumptionCheck, objectIdField: "check", writeMode: { kind: "create_only" } },
})

// One click in Studio: a queued request a server-side worker runs
// (norn_regression.worker). The worker reports progress and the result
// through stats.checkrequest.finish; the request keeps its history in receipts.
const RequestFields = Schema.Struct({
  request: Schema.String,
  objectType: Schema.String,
  target: Schema.String,
  features: Schema.String, // comma-joined
  orderField: Schema.String, // "" when none
  status: Schema.Literals(["queued", "running", "done", "failed"]),
  spec: Schema.String, // the stats.ModelSpec id once done, else ""
  error: Schema.String, // why it failed, else ""
})

export const CheckRequest = Norn.type({
  name: "stats.CheckRequest", title: "Regression check request", version: "0.1.0",
  properties: RequestFields,
  identity: [{ name: "primary", properties: ["request"] }],
})

export const SubmitCheckRequest = Norn.action({
  id: "stats.checkrequest.submit", title: "Request regression assumption checks", version: "0.1.0",
  input: RequestFields,
  output: Schema.Struct({ request: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  writes: [{ type: CheckRequest, change: "created" }],
  effect: { kind: "upsert_input_object", type: CheckRequest, objectIdField: "request", writeMode: { kind: "create_only" } },
})

export const FinishCheckRequest = Norn.action({
  id: "stats.checkrequest.finish", title: "Report a regression check request's progress", version: "0.1.0",
  input: RequestFields,
  output: Schema.Struct({ request: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  access: { artifactId: "stats.regression.access.policy", version: "0.1.0" },
  writes: [{ type: CheckRequest, change: "modified" }],
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
      actions: [RecordModelSpec],
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
