import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Evidence = Schema.Struct({
  id: Schema.String, status: Schema.Literals(["proposed", "published", "refused"]),
  table_version: Schema.String, table_digest: Schema.String, source_digest: Schema.String,
  split_at_ms: Schema.Int, model_digest: Schema.String, parameters_digest: Schema.String,
  assumptions_digest: Schema.String, remedy_digest: Schema.String, mode: Schema.Literals(["inference", "descriptive"]),
  result_assumptions_digest: Schema.String, result_remedy_digest: Schema.String,
  receipt_digest: Schema.String,
  result_table_version: Schema.String, result_table_digest: Schema.String, result_source_digest: Schema.String,
  result_split_at_ms: Schema.Int, result_model_digest: Schema.String, result_parameters_digest: Schema.String,
  complete: Schema.Boolean, quarantined_count: Schema.Int,
  frontier_age_ms: Schema.Int, max_frontier_age_ms: Schema.Int,
  late_feature_count: Schema.Int, latest_training_label_ms: Schema.Int,
  training_count: Schema.Int, holdout_count: Schema.Int, non_forward_outcome_count: Schema.Int,
  holdout_error: Norn.decimal(9), holdout_error_limit: Norn.decimal(9),
  error_unit: Schema.String, result_error_unit: Schema.String, diagnostics_passed: Schema.Boolean,
})

/** ANALYSIS-PUBLISH-1: recorded Function evidence precedes projection authority.
 * No ordinary create/update action authors these evidence fields. Until the
 * acceptReceipt effect binds authenticated results, the empty receipt pin keeps
 * publication fail-closed. PUBLISH accepts only a receipt reference and an ID.
 */
export const Publication = Norn.type({
  name: "core.analytics.Publication", title: "Analytical publication", version: "0.1.0",
  properties: Evidence, identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "proposed", receipt_digest: "" },
  invariants: [{ id: "publication_policy", message: "Analytical publication requires matching qualified evidence.",
    expression: Norn.expression(Evidence, self => self.status !== "published" || (
      self.receipt_digest !== "" && self.table_version !== "" && self.table_digest !== "" && self.source_digest !== "" &&
      self.result_table_version === self.table_version && self.result_table_digest === self.table_digest &&
      self.result_source_digest === self.source_digest && self.result_split_at_ms === self.split_at_ms &&
      self.model_digest !== "" && self.parameters_digest !== "" &&
      self.result_model_digest === self.model_digest && self.result_parameters_digest === self.parameters_digest &&
      self.complete && self.quarantined_count === 0 && self.frontier_age_ms >= 0 &&
      self.max_frontier_age_ms >= 0 && self.frontier_age_ms <= self.max_frontier_age_ms &&
      self.late_feature_count === 0 && self.latest_training_label_ms < self.split_at_ms &&
      self.training_count > 0 && self.holdout_count > 0 && self.non_forward_outcome_count === 0 &&
      self.holdout_error >= 0 && self.holdout_error_limit >= 0 && self.holdout_error <= self.holdout_error_limit &&
      self.error_unit !== "" && self.error_unit === self.result_error_unit &&
      self.assumptions_digest !== "" && self.assumptions_digest === self.result_assumptions_digest &&
      self.remedy_digest !== "" && self.remedy_digest === self.result_remedy_digest &&
      (self.diagnostics_passed || self.mode === "descriptive")
    )),
  }],
  lifecycles: [{ property: "status", initial: ["proposed"], terminal: ["published", "refused"], transitions: [
    { id: "publish", from: ["proposed"], to: "published", guardRefusalCode: "core.projection.publication_guard",
      guards: [
        Norn.expression(Evidence, self => self.receipt_digest !== ""),
        Norn.expression(Evidence, self => self.table_version !== "" && self.table_digest !== "" && self.source_digest !== "" &&
          self.result_table_version === self.table_version && self.result_table_digest === self.table_digest &&
          self.result_source_digest === self.source_digest && self.result_split_at_ms === self.split_at_ms),
        Norn.expression(Evidence, self => self.model_digest !== "" && self.parameters_digest !== "" &&
          self.result_model_digest === self.model_digest && self.result_parameters_digest === self.parameters_digest),
        Norn.expression(Evidence, self => self.complete && self.quarantined_count === 0),
        Norn.expression(Evidence, self => self.frontier_age_ms >= 0 && self.max_frontier_age_ms >= 0 &&
          self.frontier_age_ms <= self.max_frontier_age_ms),
        Norn.expression(Evidence, self => self.late_feature_count === 0),
        Norn.expression(Evidence, self => self.latest_training_label_ms < self.split_at_ms),
        Norn.expression(Evidence, self => self.training_count > 0 && self.holdout_count > 0 && self.non_forward_outcome_count === 0),
        Norn.expression(Evidence, self => self.holdout_error >= 0 && self.holdout_error_limit >= 0 &&
          self.holdout_error <= self.holdout_error_limit && self.error_unit !== "" && self.error_unit === self.result_error_unit),
        Norn.expression(Evidence, self => self.assumptions_digest !== "" && self.assumptions_digest === self.result_assumptions_digest &&
          self.remedy_digest !== "" && self.remedy_digest === self.result_remedy_digest &&
          (self.diagnostics_passed || self.mode === "descriptive")),
      ],
    },
    { id: "refuse", from: ["proposed"], to: "refused" },
  ] }],
})

const Reference = Schema.Struct({ id: Schema.String, receipt: Schema.String })

export const Publish = Norn.action({
  id: "core.analytics.publish", title: "Publish qualified analysis", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Reference, output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Publication, change: "modified" }],
  description: "Fail-closed declaration: acceptReceipt must bind the referenced Function result before this transition can succeed.",
  effect: { kind: "fire_transition", type: Publication, objectIdField: "id", lifecycle: "status", transition: "publish" },
})

export const Refuse = Norn.action({
  id: "core.analytics.refuse", title: "Refuse analytical publication", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Publication, change: "modified" }],
  effect: { kind: "fire_transition", type: Publication, objectIdField: "id", lifecycle: "status", transition: "refuse" },
})

export const PublicationPolicy = Norn.law({
  id: "core.analytics.publication_policy.law", title: "Publish only qualified recorded analysis", version: "0.1.0",
  law: { lawVersion: "0.1", id: "core.analytics.publication_policy", owner: "core.analytics",
    severity: "error", rationale: "ANALYSIS-PUBLISH-1: publication requires complete fresh inputs, chronological knowledge, qualified holdout evidence and matching Function pins; reviewer authority is enforced by the action's AccessIR.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/core/src/analytics.ts" },
    scope: { root: { kind: "entity", id: "core.analytics.Publication" }, objectTypes: ["core.analytics.Publication"] },
    subject: "core.analytics.Publication", modality: { kind: "always" },
    response: { kind: "invariant", objectType: "core.analytics.Publication", invariantId: "publication_policy" },
  },
  dependencies: [{ artifactId: "core.analytics.Publication.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const PublicationAccess = Norn.access({
  id: "core.analytics.publication.access", title: "Analytical publication reviewer", version: "0.1.0",
  rules: [{ ruleId: "reviewer", actions: [Publish, Refuse], resourceType: Publication,
    scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["AnalyticsReviewer"] } }],
  }],
})
