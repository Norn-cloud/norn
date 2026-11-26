/**
 * `analytics` BI additivity (`BI-ADD-1`) & preregistration ontology pack.
 *
 * Grounded in W3C RDF Data Cube (`qb:DataStructureDefinition`, `qb:Slice`),
 * Open Semantic Interchange (OSI 0.1.1), and Norn's `DATA-FORECAST` /
 * `DATA-PUBLISH-GUARD` acceptance contracts (`docs/spec/acceptance/data-engineering.md`).
 *
 * All lifecycles carry explicit `LIFE-SOUND-1..8` endings and immutability
 * declarations so every reachable run terminates cleanly or rests validly, and
 * `analytics.Study` stays inside the `PROOF-LIFECYCLE-2/3` bounded graph profile
 * (non-nullable scalar context, entity-rooted predicate laws).
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent } from "../../core/src/rea.ts"

// ------------------------------------------------------------------ BI metrics

export const Analyst = Norn.type({
  name: "analytics.Analyst",
  role: "thing",
  title: "Analytics steward",
  version: "0.1.0",
  description:
    "Concrete accountable analyst, reviewer, or system steward implementing core.Agent for governed metrics, studies, and publications.",
  implements: [Agent],
  properties: Schema.Struct({
    id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const CertifiedMetric = Norn.type({
  name: "bi.CertifiedMetric",
  role: "thing",
  title: "Certified metric",
  version: "0.1.0",
  description:
    "Governed BI metric contract declaring exact additivity, W3C Data Cube grain, UCUM unit code, freshness SLA, and LIFE-SOUND-1..8 certification lifecycle.",
  properties: Schema.Struct({
    id: Schema.String,
    metric_name: Norn.ref<Schema.String>("core.code"),
    additivity: Schema.Literals(["additive", "semi_additive", "non_additive"]),
    grain: Norn.ref<Schema.String>("core.code"),
    unit_code: Norn.ref<Schema.String>("ucum.unit_code"),
    freshness_sla_ms: Schema.Int,
    status: Schema.Literals(["draft", "certified", "deprecated"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "draft",
  },
  links: [
    {
      id: "owner",
      description: "Accountable owner of this certified metric contract.",
      target: Analyst,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["deprecated"],
      immutable: ["deprecated"],
      endings: {
        certified: "resting",
        deprecated: "cancelled",
      },
      transitions: [
        { id: "certify", from: ["draft"], to: "certified" },
        { id: "deprecate", from: ["draft", "certified"], to: "deprecated" },
      ],
    },
  ],
})

export const certifiedMetricFreshnessPositive = Norn.law("freshness_sla_positive", {
  on: CertifiedMetric,
  must: (self) => self.freshness_sla_ms > 0,
  because: "DATA-FRESH: every certified metric must declare a strictly positive freshness SLA bound in milliseconds.",
})

export const certifiedMetricFreshnessHolds = Norn.example("certified metric with positive freshness SLA holds", {
  of: certifiedMetricFreshnessPositive,
  given: {
    id: "metric-scrap-yield",
    metric_name: "scrap.yield.weekly",
    additivity: "non_additive",
    grain: "conversion_id,iso_week,metal",
    unit_code: "1",
    freshness_sla_ms: 5000,
    status: "certified",
  },
  expect: "holds",
})

export const certifiedMetricZeroFreshnessViolated = Norn.example("certified metric with zero freshness SLA is refused", {
  of: certifiedMetricFreshnessPositive,
  given: {
    id: "metric-bad-sla",
    metric_name: "scrap.yield.weekly",
    additivity: "non_additive",
    grain: "conversion_id,iso_week,metal",
    unit_code: "1",
    freshness_sla_ms: 0,
    status: "draft",
  },
  expect: "violated",
})

export const RegisterCertifiedMetric = Norn.action({
  id: "bi.certified_metric.register",
  title: "Register a certified metric contract",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    id: Schema.String,
    owner_id: Schema.String,
    metric_name: Schema.String,
    additivity: Schema.Literals(["additive", "semi_additive", "non_additive"]),
    grain: Schema.String,
    unit_code: Schema.String,
    freshness_sla_ms: Schema.Int,
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "owner", type: Analyst, key: { property: { from: "input", property: "owner_id" } } },
  ],
  createLinks: [
    { link: "owner", targetType: Analyst, targetIdField: "owner_id" },
  ],
  writes: [{ type: CertifiedMetric, change: "created" }],
  effect: { kind: "upsert_input_object", type: CertifiedMetric, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const CertifyMetric = Norn.action({
  id: "bi.certified_metric.certify",
  title: "Certify a metric contract",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CertifiedMetric, change: "modified" }],
  effect: { kind: "fire_transition", type: CertifiedMetric, objectIdField: "id", lifecycle: "status", transition: "certify" },
})

export const DeprecateMetric = Norn.action({
  id: "bi.certified_metric.deprecate",
  title: "Deprecate a metric contract",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: CertifiedMetric, change: "modified" }],
  effect: { kind: "fire_transition", type: CertifiedMetric, objectIdField: "id", lifecycle: "status", transition: "deprecate" },
})

export const MetricSlice = Norn.type({
  name: "bi.MetricSlice",
  role: "happening",
  title: "Metric slice",
  version: "0.1.0",
  description:
    "Observed W3C Data Cube (qb:Slice) measurement linked to bi.CertifiedMetric, carrying exact numerator, denominator, and value so non-additive ratios preserve exact weighted-ratio consistency and refuse zero denominators (BI-ADD-1).",
  properties: Schema.Struct({
    id: Schema.String,
    slice_key: Norn.ref<Schema.String>("core.code"),
    additivity: Schema.Literals(["additive", "semi_additive", "non_additive"]),
    unit_code: Norn.ref<Schema.String>("ucum.unit_code"),
    effective_at: Norn.ref<Schema.String>("iso8601.timestamp"),
    known_at: Norn.ref<Schema.String>("iso8601.timestamp"),
    numerator: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit_code"] }),
    denominator: Norn.decimal(6, { additivity: { kind: "additive" }, unitSource: ["unit_code"] }),
    value: Norn.decimal(6, { additivity: { kind: "non_additive" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    {
      id: "metric",
      description: "Certified metric contract governing this observed slice.",
      target: CertifiedMetric,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

export const metricSliceDenominatorNonZero = Norn.law("denominator_non_zero", {
  on: MetricSlice,
  must: (self) => self.denominator !== 0,
  because: "BI-ADD-1: every metric slice must carry a non-zero denominator; zero-denominator slices refuse rather than synthesizing zero or infinity.",
})

export const metricSliceNonAdditiveRatioConsistent = Norn.law("non_additive_ratio_consistent", {
  on: MetricSlice,
  when: (self) => self.additivity === "non_additive",
  must: (self) =>
    self.denominator > 0 &&
    (self.numerator - self.value * self.denominator) * 2000000 <= self.denominator &&
    (self.value * self.denominator - self.numerator) * 2000000 <= self.denominator,
  because:
    "BI-ADD-1: a non-additive ratio slice requires a strictly positive denominator and value within half a scale-6 step of numerator / denominator.",
})

export const metricSliceDenominatorNonZeroHolds = Norn.example("metric slice with positive denominator holds", {
  of: metricSliceDenominatorNonZero,
  given: {
    id: "slice-w15-gold",
    slice_key: "2026-W43:gold",
    additivity: "non_additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 294,
    denominator: 300,
    value: 0.98,
  },
  expect: "holds",
})

export const metricSliceZeroDenominatorViolated = Norn.example("metric slice with zero denominator is refused", {
  of: metricSliceDenominatorNonZero,
  given: {
    id: "slice-zero-den",
    slice_key: "2026-W43:empty",
    additivity: "non_additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 0,
    denominator: 0,
    value: 0,
  },
  expect: "violated",
})

export const metricSliceRatioConsistentHolds = Norn.example("non-additive ratio slice with 294/300 = 0.98 holds", {
  of: metricSliceNonAdditiveRatioConsistent,
  given: {
    id: "slice-ratio-ok",
    slice_key: "2026-W43:gold",
    additivity: "non_additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 294,
    denominator: 300,
    value: 0.98,
  },
  expect: "holds",
})

export const metricSliceNonTerminatingRatioHolds = Norn.example("non-additive ratio slice with non-terminating 1/3 rounded to 0.333333 holds", {
  of: metricSliceNonAdditiveRatioConsistent,
  given: {
    id: "slice-ratio-third",
    slice_key: "2026-W43:third",
    additivity: "non_additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 1,
    denominator: 3,
    value: 0.333333,
  },
  expect: "holds",
})

export const metricSliceRoundedRatioOutOfToleranceViolated = Norn.example("non-additive ratio slice with 1/3 off by 1 ULP (0.333332) is refused", {
  of: metricSliceNonAdditiveRatioConsistent,
  given: {
    id: "slice-ratio-third-bad",
    slice_key: "2026-W43:third-bad",
    additivity: "non_additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 1,
    denominator: 3,
    value: 0.333332,
  },
  expect: "violated",
})

export const metricSliceRatioMismatchViolated = Norn.example("non-additive ratio slice with unweighted average 0.50 instead of 0.25 is refused", {
  of: metricSliceNonAdditiveRatioConsistent,
  given: {
    id: "slice-ratio-bad",
    slice_key: "2026-W43:skewed",
    additivity: "non_additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 1,
    denominator: 4,
    value: 0.5,
  },
  expect: "violated",
})

export const metricSliceAdditiveNotApplicable = Norn.example("additive slice is unaffected by non-additive ratio law", {
  of: metricSliceNonAdditiveRatioConsistent,
  given: {
    id: "slice-additive",
    slice_key: "2026-W43:mass",
    additivity: "additive",
    unit_code: "g",
    effective_at: "2026-10-26T00:00:00Z",
    known_at: "2026-10-26T00:05:00Z",
    numerator: 300,
    denominator: 1,
    value: 300,
  },
  expect: "not_applicable",
})

const RecordMetricSliceInput = Schema.Struct({
  id: Schema.String,
  metric_id: Schema.String,
  slice_key: Schema.String,
  additivity: Schema.Literals(["additive", "semi_additive", "non_additive"]),
  unit_code: Schema.String,
  effective_at: Schema.String,
  known_at: Schema.String,
  numerator: Norn.decimal(6),
  denominator: Norn.decimal(6),
  value: Norn.decimal(6),
})

export const RecordMetricSlice = Norn.action({
  id: "bi.metric_slice.record",
  title: "Record an observed metric slice",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: RecordMetricSliceInput,
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "metric", type: CertifiedMetric, key: { property: { from: "input", property: "metric_id" } } },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        input: RecordMetricSliceInput,
        slice: Schema.Struct({
          metric: Schema.Struct({
            additivity: Schema.Literals(["additive", "semi_additive", "non_additive"]),
            unit_code: Schema.String,
            status: Schema.Literals(["draft", "certified", "deprecated"]),
          }),
        }),
      }),
      (ctx) =>
        ctx.slice.metric.status === "certified" &&
        ctx.input.additivity === ctx.slice.metric.additivity &&
        ctx.input.unit_code === ctx.slice.metric.unit_code,
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "metric", targetType: CertifiedMetric, targetIdField: "metric_id" },
  ],
  writes: [{ type: MetricSlice, change: "created" }],
  effect: { kind: "upsert_input_object", type: MetricSlice, objectIdField: "id", writeMode: { kind: "create_only" } },
})

// -------------------------------------------------- Analytics preregistration

export const Study = Norn.type({
  name: "analytics.Study",
  role: "happening",
  title: "Preregistered analytical study",
  version: "0.1.0",
  description:
    "Preregistered analytical, experimentation, or ML study pinning temporal split boundaries (ANALYTICS-SPLIT-1) and diagnostics-gated publication lifecycle (DATA-PUBLISH-GUARD, LIFE-SOUND-1..8).",
  properties: Schema.Struct({
    id: Schema.String,
    study_code: Norn.ref<Schema.String>("core.code"),
    population_digest: Norn.ref<Schema.String>("core.code"),
    method_digest: Norn.ref<Schema.String>("core.code"),
    receipt_digest: Norn.ref<Schema.String>("core.code"),
    train_cutoff_at: Norn.ref<Schema.String>("iso8601.timestamp"),
    holdout_start_at: Norn.ref<Schema.String>("iso8601.timestamp"),
    diagnostics_passed: Schema.Boolean,
    publication_status: Schema.Literals(["preregistered", "evaluated", "published", "withheld"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    receipt_digest: "pending",
    diagnostics_passed: false,
    publication_status: "preregistered",
  },
  links: [
    {
      id: "owner",
      description: "Accountable analyst or investigator owning this preregistered study.",
      target: Analyst,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "target_metric",
      description: "Optional certified metric evaluated by this study.",
      target: CertifiedMetric,
      cardinality: { to: "one", from: "many" },
    },
  ],
  lifecycles: [
    {
      property: "publication_status",
      initial: ["preregistered"],
      terminal: ["published", "withheld"],
      immutable: ["published", "withheld"],
      endings: {
        published: "completed",
        withheld: "cancelled",
      },
      transitions: [
        { id: "evaluate", from: ["preregistered"], to: "evaluated" },
        { id: "publish", from: ["evaluated"], to: "published" },
        { id: "withhold", from: ["preregistered", "evaluated"], to: "withheld" },
      ],
    },
  ],
})

export const studyTemporalSplitOrdered = Norn.law("temporal_split_ordered", {
  on: Study,
  must: (self) => Norn.timestampAtOrBefore(self.train_cutoff_at, self.holdout_start_at),
  because: "ANALYTICS-SPLIT-1: train_cutoff_at must be at or before holdout_start_at so training features never overlap holdout outcomes.",
})

export const studyPublishRequiresDiagnostics = Norn.law("publish_requires_diagnostics", {
  on: Study,
  must: (self) => self.publication_status !== "published" || self.diagnostics_passed === true,
  because: "DATA-PUBLISH-GUARD: an analytical study may enter the published state only when all preregistered diagnostics have passed.",
})

export const studyTemporalSplitAdjacentHolds = Norn.example("study with adjacent train_cutoff_at == holdout_start_at holds", {
  of: studyTemporalSplitOrdered,
  given: {
    id: "study-k1-ols",
    study_code: "SCRAP-YIELD-OLS-K1",
    population_digest: "22cee20298906d6f",
    method_digest: "ols-intercept-v1",
    train_cutoff_at: "2026-09-28T00:00:00Z",
    holdout_start_at: "2026-09-28T00:00:00Z",
    diagnostics_passed: true,
    publication_status: "published",
  },
  expect: "holds",
})

export const studyTemporalSplitOverlapViolated = Norn.example("study with train_cutoff_at after holdout_start_at is refused", {
  of: studyTemporalSplitOrdered,
  given: {
    id: "study-leaky",
    study_code: "SCRAP-YIELD-LEAKY",
    population_digest: "22cee20298906d6f",
    method_digest: "ols-intercept-v1",
    train_cutoff_at: "2026-10-05T00:00:00Z",
    holdout_start_at: "2026-09-28T00:00:00Z",
    diagnostics_passed: false,
    publication_status: "preregistered",
  },
  expect: "violated",
})

export const studyPublishPassedHolds = Norn.example("published study with passing diagnostics holds", {
  of: studyPublishRequiresDiagnostics,
  given: {
    id: "study-pub-ok",
    study_code: "SCRAP-YIELD-OLS-K1",
    population_digest: "22cee20298906d6f",
    method_digest: "ols-intercept-v1",
    train_cutoff_at: "2026-09-28T00:00:00Z",
    holdout_start_at: "2026-09-28T00:00:00Z",
    diagnostics_passed: true,
    publication_status: "published",
  },
  expect: "holds",
})

export const studyPublishFailedDiagnosticsViolated = Norn.example("published study with failing diagnostics is refused", {
  of: studyPublishRequiresDiagnostics,
  given: {
    id: "study-pub-bad",
    study_code: "SCRAP-YIELD-OLS-FAIL",
    population_digest: "22cee20298906d6f",
    method_digest: "ols-intercept-v1",
    train_cutoff_at: "2026-09-28T00:00:00Z",
    holdout_start_at: "2026-09-28T00:00:00Z",
    diagnostics_passed: false,
    publication_status: "published",
  },
  expect: "violated",
})

export const studyWithheldFailedDiagnosticsHolds = Norn.example("withheld study with failing diagnostics holds", {
  of: studyPublishRequiresDiagnostics,
  given: {
    id: "study-withheld-ok",
    study_code: "SCRAP-YIELD-OLS-FAIL",
    population_digest: "22cee20298906d6f",
    method_digest: "ols-intercept-v1",
    train_cutoff_at: "2026-09-28T00:00:00Z",
    holdout_start_at: "2026-09-28T00:00:00Z",
    diagnostics_passed: false,
    publication_status: "withheld",
  },
  expect: "holds",
})

export const PreregisterStudy = Norn.action({
  id: "analytics.study.preregister",
  title: "Preregister an analytical study",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    id: Schema.String,
    owner_id: Schema.String,
    study_code: Schema.String,
    population_digest: Schema.String,
    method_digest: Schema.String,
    train_cutoff_at: Schema.String,
    holdout_start_at: Schema.String,
  }),
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "owner", type: Analyst, key: { property: { from: "input", property: "owner_id" } } },
  ],
  createLinks: [
    { link: "owner", targetType: Analyst, targetIdField: "owner_id" },
  ],
  writes: [{ type: Study, change: "created" }],
  effect: { kind: "upsert_input_object", type: Study, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const EvaluateStudy = Norn.action({
  id: "analytics.study.evaluate",
  title: "Evaluate a preregistered study from a verified diagnostic receipt",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String, receipt: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Study, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Study,
    objectIdField: "id",
    lifecycle: "publication_status",
    transition: "evaluate",
    acceptReceipt: {
      referenceField: "receipt",
      manifestDigest: "sha256:6b688d7ccd5b5e58cf8fef4a682cc87ec4033f0d445d42214f648dbfc406bee4",
      digestProperty: "receipt_digest",
      resultBindings: { diagnostics_passed: "diagnostics_passed" },
    },
  },
})

export const PublishStudy = Norn.action({
  id: "analytics.study.publish",
  title: "Publish an evaluated study",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Study, change: "modified" }],
  effect: { kind: "fire_transition", type: Study, objectIdField: "id", lifecycle: "publication_status", transition: "publish" },
})

export const WithholdStudy = Norn.action({
  id: "analytics.study.withhold",
  title: "Withhold a study from publication",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Study, change: "modified" }],
  effect: { kind: "fire_transition", type: Study, objectIdField: "id", lifecycle: "publication_status", transition: "withhold" },
})

export const Publication = Norn.type({
  name: "analytics.Publication",
  role: "happening",
  title: "Analytical publication record",
  version: "0.1.0",
  description:
    "Governed decision record for publishing or withholding a preregistered study, retaining exact population and receipt digests (DATA-PUBLISH-GUARD).",
  properties: Schema.Struct({
    id: Schema.String,
    study_code: Norn.ref<Schema.String>("core.code"),
    receipt_digest: Norn.ref<Schema.String>("core.code"),
    population_digest: Norn.ref<Schema.String>("core.code"),
    published_at: Norn.ref<Schema.String>("iso8601.timestamp"),
    diagnostics_passed: Schema.Boolean,
    decision: Schema.Literals(["published", "withheld"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    {
      id: "study",
      description: "Preregistered study governed by this publication decision.",
      target: Study,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "reviewer",
      description: "Accountable reviewer who authorized or withheld publication.",
      target: Analyst,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

export const publicationRequiresDiagnostics = Norn.law("publication_requires_diagnostics", {
  on: Publication,
  must: (self) => self.decision !== "published" || self.diagnostics_passed === true,
  because: "DATA-PUBLISH-GUARD: an analytical publication decision may be published only when diagnostics passed.",
})

export const publicationPassedHolds = Norn.example("published record with passing diagnostics holds", {
  of: publicationRequiresDiagnostics,
  given: {
    id: "pub-1",
    study_code: "SCRAP-YIELD-OLS-K1",
    receipt_digest: "fn-receipt-001",
    population_digest: "22cee20298906d6f",
    published_at: "2026-10-26T01:00:00Z",
    diagnostics_passed: true,
    decision: "published",
  },
  expect: "holds",
})

export const publicationFailedViolated = Norn.example("published record with failing diagnostics is refused", {
  of: publicationRequiresDiagnostics,
  given: {
    id: "pub-2",
    study_code: "SCRAP-YIELD-OLS-FAIL",
    receipt_digest: "fn-receipt-002",
    population_digest: "22cee20298906d6f",
    published_at: "2026-10-26T01:00:00Z",
    diagnostics_passed: false,
    decision: "published",
  },
  expect: "violated",
})

const RecordPublicationInput = Schema.Struct({
  id: Schema.String,
  study_id: Schema.String,
  reviewer_id: Schema.String,
  study_code: Schema.String,
  receipt_digest: Schema.String,
  population_digest: Schema.String,
  published_at: Schema.String,
  diagnostics_passed: Schema.Boolean,
  decision: Schema.Literals(["published", "withheld"]),
})

export const RecordPublication = Norn.action({
  id: "analytics.publication.record",
  title: "Record a study publication decision",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: RecordPublicationInput,
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "study", type: Study, key: { property: { from: "input", property: "study_id" } } },
    { name: "reviewer", type: Analyst, key: { property: { from: "input", property: "reviewer_id" } } },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        input: RecordPublicationInput,
        slice: Schema.Struct({
          study: Schema.Struct({
            diagnostics_passed: Schema.Boolean,
            receipt_digest: Schema.String,
            study_code: Schema.String,
            population_digest: Schema.String,
            publication_status: Schema.Literals(["preregistered", "evaluated", "published", "withheld"]),
          }),
        }),
      }),
      (ctx) =>
        ctx.slice.study.publication_status === ctx.input.decision &&
        ctx.input.diagnostics_passed === ctx.slice.study.diagnostics_passed &&
        ctx.input.receipt_digest === ctx.slice.study.receipt_digest &&
        ctx.input.study_code === ctx.slice.study.study_code &&
        ctx.input.population_digest === ctx.slice.study.population_digest,
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "study", targetType: Study, targetIdField: "study_id" },
    { link: "reviewer", targetType: Analyst, targetIdField: "reviewer_id" },
  ],
  writes: [{ type: Publication, change: "created" }],
  effect: { kind: "upsert_input_object", type: Publication, objectIdField: "id", writeMode: { kind: "create_only" } },
})
