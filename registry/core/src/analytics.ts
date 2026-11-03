import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { FORECAST_MANIFEST_DIGEST } from "./shared/metrics-pin.ts"

const Evidence = Schema.Struct({
  id: Schema.String, status: Schema.Literals(["proposed", "published", "refused"]),
  table_version: Schema.String, table_digest: Schema.String, source_digest: Schema.String,
  split_at_ms: Schema.Int, model_digest: Schema.String, parameters_digest: Schema.String,
  assumptions_digest: Schema.String, remedy_digest: Schema.String, mode: Schema.Literals(["inference", "descriptive"]),
  result_assumptions_digest: Schema.String, result_remedy_digest: Schema.String,
  receipt_digest: Schema.String,
  result_table_version: Schema.String, result_table_digest: Schema.String, result_source_digest: Schema.String,
  result_split_at_ms: Schema.Int, result_model_digest: Schema.String, result_parameters_digest: Schema.String,
  complete: Schema.Boolean, fresh: Schema.Boolean, quarantined_count: Schema.Int,
  frontier_age_ms: Schema.Int, max_frontier_age_ms: Schema.Int,
  late_feature_count: Schema.Int, latest_training_label_ms: Schema.Int, temporal_leakage: Schema.Boolean,
  training_count: Schema.Int, holdout_count: Schema.Int, non_forward_outcome_count: Schema.Int,
  holdout_error_nanos: Schema.NullOr(Schema.Int), holdout_error_limit_nanos: Schema.Int,
  error_unit: Schema.String, result_error_unit: Schema.String, diagnostics_passed: Schema.Boolean,
})

export const PublicationEvidence = Norn.interfaceType({
  name: "core.analytics.PublicationEvidence", title: "Qualified publication evidence", version: "0.1.0",
  properties: Evidence,
  invariants: [{ id: "publication_policy", message: "Analytical publication requires matching qualified evidence.",
    expression: Norn.expression(Evidence, self => self.status !== "published" || (
      self.receipt_digest !== "" && self.table_version !== "" && self.table_digest !== "" && self.source_digest !== "" &&
      self.result_table_version === self.table_version && self.result_table_digest === self.table_digest &&
      self.result_source_digest === self.source_digest && self.result_split_at_ms === self.split_at_ms &&
      self.model_digest !== "" && self.parameters_digest !== "" &&
      self.result_model_digest === self.model_digest && self.result_parameters_digest === self.parameters_digest &&
      self.complete && self.quarantined_count === 0 && self.fresh && self.frontier_age_ms >= 0 &&
      self.max_frontier_age_ms >= 0 && self.frontier_age_ms <= self.max_frontier_age_ms &&
      !self.temporal_leakage && self.late_feature_count === 0 && self.latest_training_label_ms < self.split_at_ms &&
      self.training_count > 0 && self.holdout_count > 0 && self.non_forward_outcome_count === 0 &&
      self.holdout_error_nanos !== null && (self.holdout_error_nanos ?? -1) >= 0 && self.holdout_error_limit_nanos >= 0 && (self.holdout_error_nanos ?? -1) <= self.holdout_error_limit_nanos &&
      self.error_unit !== "" && self.error_unit === self.result_error_unit &&
      self.assumptions_digest !== "" && self.assumptions_digest === self.result_assumptions_digest &&
      self.remedy_digest !== "" && self.remedy_digest === self.result_remedy_digest &&
      (self.diagnostics_passed || self.mode === "descriptive")
    )),
  }],
})

/** ANALYSIS-PUBLISH-1: only the configured, recorded Function receipt may
 * populate evidence before the declared publication policy is evaluated. */
export const Publication = Norn.type({
  name: "core.analytics.Publication", title: "Analytical publication", version: "0.1.0",
  properties: Evidence, implements: [PublicationEvidence], identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "proposed", receipt_digest: "" },
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
        Norn.expression(Evidence, self => self.fresh && self.frontier_age_ms >= 0 && self.max_frontier_age_ms >= 0 &&
          self.frontier_age_ms <= self.max_frontier_age_ms),
        Norn.expression(Evidence, self => !self.temporal_leakage),
        Norn.expression(Evidence, self => self.late_feature_count === 0),
        Norn.expression(Evidence, self => self.latest_training_label_ms < self.split_at_ms),
        Norn.expression(Evidence, self => self.training_count > 0 && self.holdout_count > 0 && self.non_forward_outcome_count === 0),
        Norn.expression(Evidence, self => self.holdout_error_nanos !== null && (self.holdout_error_nanos ?? -1) >= 0 && self.holdout_error_limit_nanos >= 0 &&
          (self.holdout_error_nanos ?? -1) <= self.holdout_error_limit_nanos && self.error_unit !== "" && self.error_unit === self.result_error_unit),
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
  description: "Accept configured Function evidence, then apply the declared publication policy under reviewer authority.",
  effect: { kind: "fire_transition", type: Publication, objectIdField: "id", lifecycle: "status", transition: "publish",
    acceptReceipt: { referenceField: "receipt", manifestDigest: FORECAST_MANIFEST_DIGEST, digestProperty: "receipt_digest",
      resultBindings: {
        result_table_version: "table_version", result_table_digest: "table_digest", result_source_digest: "source_digest",
        result_split_at_ms: "split_at_ms", result_model_digest: "implementation_digest", result_parameters_digest: "parameters_digest",
        result_assumptions_digest: "assumptions_digest", result_remedy_digest: "remedy_digest",
        complete: "complete", fresh: "fresh", quarantined_count: "quarantine_count", frontier_age_ms: "frontier_age_ms",
        late_feature_count: "late_features", latest_training_label_ms: "latest_training_label_ms", temporal_leakage: "temporal_leakage",
        training_count: "training_rows", holdout_count: "holdout_rows", non_forward_outcome_count: "non_forward_outcome_count",
        holdout_error_nanos: "rmse_nanos", result_error_unit: "error_unit", diagnostics_passed: "diagnostics_passed",
      },
    },
  },
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
