import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// The demo for applying a model (#1171 M4), served with the tracking and ml
// packs by ../run-demo.sh: customers, a churn model that names where its
// predictions are written, and the typed prediction those writes create.

const CustomerProperties = Schema.Struct({
  customer_id: Schema.String,
  tenure_months: Schema.Int,
  monthly_spend: Norn.decimal(2),
  plan: Schema.Literals(["basic", "pro"]),
  churned: Schema.Boolean,
})

export const Customer = Norn.type({
  name: "sales.Customer", title: "Customer", version: "0.1.0",
  properties: CustomerProperties,
  identity: [{ name: "primary", properties: ["customer_id"] }],
})

export const RecordCustomer = Norn.action({
  id: "sales.customer.record", title: "Record a customer", version: "0.1.0",
  input: CustomerProperties, output: Schema.Struct({ customer_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Customer, change: "created" }],
  effect: { kind: "upsert_input_object", type: Customer, objectIdField: "customer_id", writeMode: { kind: "create_only" } },
})

const Outputs = Schema.Struct({
  churn_probability: Norn.decimal(4),
  risk_band: Schema.Literals(["low", "medium", "high"]),
})

const PredictionProperties = Schema.Struct({
  prediction_id: Schema.String, // "<subject>@<batch_id>"
  subject: Schema.String, // the scored sales.Customer's id
  model_version: Schema.String, // the ml.ModelVersion that produced it
  churn_probability: Norn.decimal(4),
  risk_band: Schema.Literals(["low", "medium", "high"]),
})

/** One customer's churn prediction from one applied model version. */
export const ChurnPrediction = Norn.type({
  name: "sales.ChurnPrediction", title: "Churn prediction", version: "0.1.0",
  properties: PredictionProperties,
  identity: [{ name: "primary", properties: ["prediction_id"] }],
  links: [
    { id: "scored", target: "sales.Customer", cardinality: { to: "one", from: "many" } },
    { id: "predicted_by", target: "ml.ModelVersion", cardinality: { to: "one", from: "many" } },
  ],
})

/**
 * Create-only, by the convention `norn_models.apply_model` follows
 * (`<namespace>.<lowercased type>.record`). The scored customer and a
 * production model version must exist; both are linked.
 */
export const RecordChurnPrediction = Norn.action({
  id: "sales.churnprediction.record", title: "Record a churn prediction", version: "0.1.0",
  input: PredictionProperties, output: Schema.Struct({ prediction_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  slice: [
    { name: "customer", type: "sales.Customer", key: { property: { from: "input", property: "subject" } } },
    { name: "version", type: "ml.ModelVersion", key: { property: { from: "input", property: "model_version" } } },
  ],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ version: Schema.Struct({ status: Schema.Literals(["candidate", "production", "retired"]) }) }) }),
    (input) => input.slice.version.status === "production")],
  createLinks: [
    { link: "scored", targetType: "sales.Customer", targetIdField: "subject" },
    { link: "predicted_by", targetType: "ml.ModelVersion", targetIdField: "model_version" },
  ],
  writes: [{ type: ChurnPrediction, change: "created" }],
  effect: { kind: "upsert_input_object", type: ChurnPrediction, objectIdField: "prediction_id", writeMode: { kind: "create_only" } },
})

/** Churn risk from tenure, spend and plan. Its internals stay opaque: the caller's `predict` computes it. */
export const ChurnModel = Norn.model({
  id: "sales.churn_model", title: "Churn risk", version: "0.1.0",
  input: Customer,
  features: ["tenure_months", "monthly_spend", "plan"],
  target: "churned",
  outputs: Outputs,
  trust: "contract_checked",
  contracts: [
    { id: "probability", output: "churn_probability", rule: { kind: "range", min: "0", max: "1" } },
    { id: "finite", output: "churn_probability", rule: { kind: "finite" } },
    { id: "band", output: "risk_band", rule: { kind: "oneOf", values: ["low", "medium", "high"] } },
  ],
  predictions: ChurnPrediction,
  method: { label: "logistic regression on tenure" },
})
