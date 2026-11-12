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
  writes: [{ type: ModelSpec, change: "created" }],
  effect: { kind: "upsert_input_object", type: ModelSpec, objectIdField: "spec", writeMode: { kind: "create_only" } },
})

export const RecordAssumptionCheck = Norn.action({
  id: "stats.assumptioncheck.record", title: "Record a regression assumption check", version: "0.1.0",
  input: CheckFields,
  output: Schema.Struct({ check: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: AssumptionCheck, change: "created" }],
  // The spec must exist: it is read by its key in the same commit, and linked.
  slice: [{ name: "spec", type: ModelSpec, key: { property: { from: "input", property: "spec" } } }],
  createLinks: [{ link: "spec", targetType: ModelSpec, targetIdField: "spec" }],
  effect: { kind: "upsert_input_object", type: AssumptionCheck, objectIdField: "check", writeMode: { kind: "create_only" } },
})
