/**
 * `data` constitutional ontology pack: Data, Provenance, Observation & Quality.
 *
 * Grounded in W3C (`PROV-O`, `DCAT v3`, `SOSA/SSN`, `DQV`, `OWL-Time`) and ISO
 * (`ISO/IEC 21838-2` BFO, `ISO/IEC 11179`, `ISO/IEC 25012` / `ISO 8000`, `ISO 8601`).
 *
 * Opt-in pack under `registry/packs/data/`: keeps embedded cell base fixtures
 * untouched while providing reusable interfaces, executable `LAW-LINK-1`
 * temporal causality laws (`DATA-CAUSAL-1`), and concrete reference object types
 * exercising every interface, `acyclic: true` revision chain, and `LIFE-SOUND-1..8`
 * quality-finding resolution lifecycle.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Agent } from "../../core/src/rea.ts"
import { TimeInterval } from "../../core/src/time.ts"

const DatasetProperties = Schema.Struct({
  dataset_id: Schema.String,
  schema_uri: Schema.String,
  freshness_sla_ms: Schema.NullOr(Schema.Int),
  kind: Schema.Literals(["table", "stream", "log", "metric_cube", "document_corpus"]),
})

/**
 * Logical dataset catalog entry (W3C DCAT v3 `dcat:Dataset` + ISO/IEC 11179).
 */
export const Dataset = Norn.interfaceType({
  name: "data.Dataset",
  role: "thing",
  title: "Dataset",
  version: "0.1.0",
  description: "A logical dataset governed by a schema URI and optional freshness SLA (W3C DCAT v3 dcat:Dataset + ISO/IEC 11179).",
  properties: DatasetProperties,
  links: [
    {
      id: "owner",
      description: "Accountable steward or owning agent (W3C DCAT dcterms:publisher / PROV-O prov:Agent).",
      target: Agent,
      cardinality: { to: "one", from: "many" },
    },
  ],
  invariants: [
    {
      id: "positive_freshness_sla",
      message: "freshness_sla_ms must be null or strictly positive.",
      expression: Norn.expression(DatasetProperties, (self) => self.freshness_sla_ms === null || (self.freshness_sla_ms ?? 0) > 0),
    },
    {
      id: "non_empty_schema_uri",
      message: "schema_uri must be non-empty.",
      expression: Norn.expression(DatasetProperties, (self) => self.schema_uri !== ""),
    },
  ],
})

const SnapshotProperties = Schema.Struct({
  snapshot_id: Schema.String,
  content_sha256: Schema.String,
  schema_digest: Schema.String,
  record_count: Schema.Int,
  byte_size: Schema.Int,
  effective_at: Norn.ref("core.time.instant"),
  known_at: Norn.ref("core.time.instant"),
})

const SnapshotInvariantInput = Schema.Struct({
  content_sha256: Schema.String,
  schema_digest: Schema.String,
  record_count: Schema.Int,
  byte_size: Schema.Int,
})

/**
 * Immutable content-addressed snapshot of a dataset (W3C DCAT v3 `dcat:DatasetSeries`
 * member + W3C PROV-O `prov:Entity`). Concrete implementations must declare
 * `acyclic: true` on `supersedes` (enforced on `data.SnapshotRecord`).
 */
export const DatasetSnapshot = Norn.interfaceType({
  name: "data.DatasetSnapshot",
  role: "thing",
  title: "Dataset snapshot",
  version: "0.1.0",
  description: "An immutable content-addressed snapshot of a dataset with bitemporal effective_at and known_at instants (W3C DCAT v3 DatasetSeries member + W3C PROV-O prov:Entity).",
  properties: SnapshotProperties,
  links: [
    {
      id: "dataset",
      description: "Logical dataset this snapshot materializes (W3C DCAT dcat:inSeries).",
      target: Dataset,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "supersedes",
      description: "Prior snapshot superseded by this revision (W3C PROV-O prov:wasRevisionOf; convention: concrete implementations declare acyclic: true, as on data.SnapshotRecord).",
      target: "data.DatasetSnapshot",
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "generated_by",
      description: "Provenance activity that produced this snapshot (W3C PROV-O prov:wasGeneratedBy).",
      target: "data.Activity",
      cardinality: { to: "one", from: "many" },
    },
  ],
  invariants: [
    {
      id: "non_negative_counts",
      message: "record_count and byte_size must be non-negative.",
      expression: Norn.expression(SnapshotInvariantInput, (self) => self.record_count >= 0 && self.byte_size >= 0),
    },
    {
      id: "non_empty_digests",
      message: "content_sha256 and schema_digest must be non-empty.",
      expression: Norn.expression(SnapshotInvariantInput, (self) => self.content_sha256 !== "" && self.schema_digest !== ""),
    },
  ],
})

const ActivityOwnProperties = Schema.Struct({
  activity_kind: Schema.Literals(["sync", "transform", "training", "evaluation", "restatement", "export"]),
  code_digest: Schema.String,
})

/**
 * Provenance execution activity over a proper half-open UTC interval `[starts_at, ends_at)`
 * (W3C PROV-O `prov:Activity` + W3C OWL-Time via `core.TimeInterval`).
 */
export const Activity = Norn.interfaceType({
  name: "data.Activity",
  role: "happening",
  title: "Activity",
  version: "0.1.0",
  description: "A time-bounded provenance execution activity extending core.TimeInterval so starts_at < ends_at always holds (W3C PROV-O prov:Activity + W3C OWL-Time).",
  extends: [TimeInterval],
  properties: ActivityOwnProperties,
  links: [
    {
      id: "actor",
      description: "Accountable agent associated with this activity (W3C PROV-O prov:wasAssociatedWith).",
      target: Agent,
      cardinality: { to: "one", from: "many" },
    },
  ],
  invariants: [
    {
      id: "non_empty_code_digest",
      message: "code_digest must be non-empty.",
      expression: Norn.expression(ActivityOwnProperties, (self) => self.code_digest !== ""),
    },
  ],
})

const DerivationProperties = Schema.Struct({
  derivation_id: Schema.String,
})

/**
 * Qualified lineage edge connecting an input snapshot, producing activity, and
 * output snapshot (W3C PROV-O `prov:wasDerivedFrom` / `prov:used` + OpenLineage).
 */
export const Derivation = Norn.interfaceType({
  name: "data.Derivation",
  role: "happening",
  title: "Derivation",
  version: "0.1.0",
  description: "A qualified provenance derivation edge linking an input snapshot, an execution activity, and an output snapshot (W3C PROV-O prov:Derivation / prov:wasDerivedFrom + OpenLineage).",
  properties: DerivationProperties,
  links: [
    {
      id: "input_snapshot",
      description: "Upstream dataset snapshot derived from (W3C PROV-O prov:entity on prov:Derivation / prov:used).",
      target: DatasetSnapshot,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "output_snapshot",
      description: "Downstream dataset snapshot produced by the derivation (W3C PROV-O subject of prov:qualifiedDerivation / prov:wasDerivedFrom).",
      target: DatasetSnapshot,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "activity",
      description: "Execution activity bridging input_snapshot and output_snapshot (W3C PROV-O prov:hadActivity).",
      target: Activity,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

// DATA-CAUSAL-1 (LAW-LINK-1 + LAW-INTERFACE-1): temporal causality laws attached
// to `data.Derivation` and inherited by every concrete implementation.

export const inputKnownBeforeActivityEnd = Norn.law("input_known_before_activity_end", {
  on: Derivation,
  must: (self) => Norn.timestampAtOrBefore(self.input_snapshot.known_at, self.activity.ends_at),
  because: "DATA-CAUSAL-1: an input snapshot cannot be known after the deriving activity ended (forbids future-knowledge leakage / ML lookahead).",
})

export const outputKnownAfterActivityStart = Norn.law("output_known_after_activity_start", {
  on: Derivation,
  must: (self) => Norn.timestampAtOrBefore(self.activity.starts_at, self.output_snapshot.known_at),
  because: "DATA-CAUSAL-1: an output snapshot cannot be known before the deriving activity started.",
})

export const monotonicKnowledgeTime = Norn.law("monotonic_knowledge_time", {
  on: Derivation,
  must: (self) => Norn.timestampAtOrBefore(self.input_snapshot.known_at, self.output_snapshot.known_at),
  because: "DATA-CAUSAL-1: knowledge time is monotonically non-decreasing across every derivation edge, preventing transitive future leakage.",
})

export const distinctSnapshots = Norn.law("distinct_snapshots", {
  on: Derivation,
  must: (self) => self.input_snapshot.snapshot_id !== self.output_snapshot.snapshot_id,
  because: "DATA-CAUSAL-1: a derivation must connect two distinct snapshots (non-reflexive lineage edge).",
})

// LAW-EXAMPLE-1 checked valuations for DATA-CAUSAL-1 laws.

export const inputKnownBeforeEndHolds = Norn.example("input known before activity end holds", {
  of: inputKnownBeforeActivityEnd,
  given: {
    derivation_id: "drv-1",
    input_snapshot: { snapshot_id: "snap-in", known_at: "2026-11-01T10:00:00Z" },
    activity: { starts_at: "2026-11-01T10:05:00Z", ends_at: "2026-11-01T10:15:00Z" },
  },
  expect: "holds",
})

export const inputKnownAfterEndViolated = Norn.example("input known after activity end violates lookahead law", {
  of: inputKnownBeforeActivityEnd,
  given: {
    derivation_id: "drv-leak",
    input_snapshot: { snapshot_id: "snap-future", known_at: "2026-11-01T11:00:00Z" },
    activity: { starts_at: "2026-11-01T10:05:00Z", ends_at: "2026-11-01T10:15:00Z" },
  },
  expect: "violated",
})

export const outputKnownAfterStartHolds = Norn.example("output known after activity start holds", {
  of: outputKnownAfterActivityStart,
  given: {
    derivation_id: "drv-1",
    output_snapshot: { snapshot_id: "snap-out", known_at: "2026-11-01T10:15:00Z" },
    activity: { starts_at: "2026-11-01T10:05:00Z", ends_at: "2026-11-01T10:15:00Z" },
  },
  expect: "holds",
})

export const outputKnownBeforeStartViolated = Norn.example("output known before activity start is violated", {
  of: outputKnownAfterActivityStart,
  given: {
    derivation_id: "drv-early",
    output_snapshot: { snapshot_id: "snap-early", known_at: "2026-11-01T09:55:00Z" },
    activity: { starts_at: "2026-11-01T10:05:00Z", ends_at: "2026-11-01T10:15:00Z" },
  },
  expect: "violated",
})

export const monotonicKnowledgeHolds = Norn.example("output known at or after input holds", {
  of: monotonicKnowledgeTime,
  given: {
    derivation_id: "drv-1",
    input_snapshot: { snapshot_id: "snap-in", known_at: "2026-11-01T10:00:00Z" },
    output_snapshot: { snapshot_id: "snap-out", known_at: "2026-11-01T10:15:00Z" },
  },
  expect: "holds",
})

export const monotonicKnowledgeViolated = Norn.example("output known before input violates monotonic knowledge time", {
  of: monotonicKnowledgeTime,
  given: {
    derivation_id: "drv-retro",
    input_snapshot: { snapshot_id: "snap-in", known_at: "2026-11-01T10:30:00Z" },
    output_snapshot: { snapshot_id: "snap-out", known_at: "2026-11-01T10:15:00Z" },
  },
  expect: "violated",
})

export const distinctSnapshotsHolds = Norn.example("distinct input and output snapshots hold", {
  of: distinctSnapshots,
  given: {
    derivation_id: "drv-1",
    input_snapshot: { snapshot_id: "snap-in" },
    output_snapshot: { snapshot_id: "snap-out" },
  },
  expect: "holds",
})

export const reflexiveSnapshotViolated = Norn.example("reflexive derivation to the same snapshot is violated", {
  of: distinctSnapshots,
  given: {
    derivation_id: "drv-self",
    input_snapshot: { snapshot_id: "snap-same" },
    output_snapshot: { snapshot_id: "snap-same" },
  },
  expect: "violated",
})

const ObservationProperties = Schema.Struct({
  observation_id: Schema.String,
  occurred_at: Norn.ref("core.time.instant"),
  observed_at: Norn.ref("core.time.instant"),
  source_system: Schema.String,
  observable_property: Schema.String,
  payload_digest: Schema.String,
})

const ObservationInvariantInput = Schema.Struct({
  occurred_at: Norn.timestamp("utc"),
  observed_at: Norn.timestamp("utc"),
  source_system: Schema.String,
  observable_property: Schema.String,
  payload_digest: Schema.String,
})

/**
 * Immutable append-only observed fact for logs, user events, telemetry, warehouse
 * scans, and external ERP captures (W3C SOSA/SSN `sosa:Observation` + CloudEvents).
 *
 * `occurred_at` (`sosa:phenomenonTime`) may equal or precede `observed_at`
 * (`sosa:resultTime`), allowing late-arriving observations while forbidding
 * observation before occurrence.
 */
// TODO(PR #1781): disjointWith: [EconomicEvent, Commitment, Claim]
export const Observation = Norn.interfaceType({
  name: "data.Observation",
  role: "happening",
  title: "Observation",
  version: "0.1.0",
  description: "An append-only observation record of an external or internal phenomenon (W3C SOSA/SSN sosa:Observation + CloudEvents; append-only immutability is a commit-layer convention). Distinct from posted economic events, commitments, and claims.",
  properties: ObservationProperties,
  links: [
    {
      id: "dataset",
      description: "Optional logical dataset this observation belongs to.",
      target: Dataset,
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "observed_by",
      description: "Sensor, connector, or agent that captured the observation (W3C SOSA sosa:madeBySensor / PROV-O prov:Agent).",
      target: Agent,
      cardinality: { to: "one", from: "many" },
    },
  ],
  invariants: [
    {
      id: "non_empty_digest",
      message: "source_system, observable_property, and payload_digest must be non-empty.",
      expression: Norn.expression(
        ObservationInvariantInput,
        (self) => self.source_system !== "" && self.observable_property !== "" && self.payload_digest !== "",
      ),
    },
    {
      id: "observed_at_or_after_occurred",
      message: "occurred_at must be at or before observed_at (late-arriving observations are allowed, pre-occurrence observations are refused).",
      expression: Norn.expression(
        ObservationInvariantInput,
        (self) => Norn.timestampAtOrBefore(self.occurred_at, self.observed_at),
      ),
    },
  ],
})

const QualityFindingProperties = Schema.Struct({
  finding_id: Schema.String,
  dimension: Schema.Literals(["accuracy", "completeness", "consistency", "credibility", "currentness"]),
  severity: Schema.Literals(["info", "warning", "error"]),
  offending_path: Schema.String,
})

/**
 * Data-quality finding against a dataset snapshot (W3C DQV `dqv:QualityMeasurement`
 * + ISO/IEC 25012 inherent quality dimensions / ISO 8000).
 */
export const QualityFinding = Norn.interfaceType({
  name: "data.QualityFinding",
  role: "happening",
  title: "Quality finding",
  version: "0.1.0",
  description: "A data quality measurement or violation finding anchored to a dataset snapshot across ISO/IEC 25012 inherent quality dimensions (W3C DQV dqv:QualityMeasurement + ISO/IEC 25012 / ISO 8000).",
  properties: QualityFindingProperties,
  links: [
    {
      id: "snapshot",
      description: "Dataset snapshot evaluated by this quality finding (W3C DQV dqv:computedOn).",
      target: DatasetSnapshot,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

export const qualityFindingNonEmptyPath = Norn.law("non_empty_offending_path", {
  on: QualityFinding,
  must: (self) => self.offending_path !== "",
  because: "A data quality finding must pinpoint the non-empty offending path within the evaluated snapshot.",
})

export const qualityFindingPathHolds = Norn.example("quality finding with offending_path holds", {
  of: qualityFindingNonEmptyPath,
  given: { finding_id: "qf-1", dimension: "completeness", severity: "error", offending_path: "/records/42/amount" },
  expect: "holds",
})

export const qualityFindingEmptyPathViolated = Norn.example("quality finding with empty offending_path is violated", {
  of: qualityFindingNonEmptyPath,
  given: { finding_id: "qf-empty", dimension: "completeness", severity: "error", offending_path: "" },
  expect: "violated",
})

// ---------------------------------------------------------------------------
// Concrete reference object types implementing all 6 data.* interfaces
// ---------------------------------------------------------------------------

/**
 * Concrete accountable agent (person, organization, or system) acting as dataset
 * owner, pipeline actor, or observation sensor within the `data` pack.
 */
export const Steward = Norn.type({
  name: "data.Steward",
  role: "thing",
  title: "Data steward",
  version: "0.1.0",
  description: "Concrete accountable agent (person, organization, or system) implementing core.Agent for dataset ownership, activity execution, and observation capture.",
  implements: [Agent],
  properties: Schema.Struct({
    steward_id: Schema.String,
    display_name: Norn.ref("core.display_name"),
    kind: Schema.Literals(["person", "organization", "system"]),
  }),
  identity: [{ name: "primary", properties: ["steward_id"] }],
})

/**
 * Concrete catalog dataset implementing `data.Dataset`.
 */
export const CatalogDataset = Norn.type({
  name: "data.CatalogDataset",
  role: "thing",
  title: "Catalog dataset",
  version: "0.1.0",
  description: "Reference catalog dataset record implementing data.Dataset.",
  implements: [Dataset],
  properties: DatasetProperties,
  identity: [{ name: "primary", properties: ["dataset_id"] }],
  links: [
    {
      id: "owner",
      description: "Owning data steward.",
      target: Steward,
      cardinality: { to: "one", from: "many" },
    },
  ],
})

/**
 * Concrete provenance execution run implementing `data.Activity` (and transitively `core.TimeInterval`).
 */
export const PipelineRun = Norn.type({
  name: "data.PipelineRun",
  role: "happening",
  title: "Pipeline run",
  version: "0.1.0",
  description: "Reference provenance execution activity implementing data.Activity and inheriting core.TimeInterval.proper_interval.",
  implements: [Activity],
  properties: Schema.Struct({
    run_id: Schema.String,
    starts_at: Norn.ref("core.time.instant"),
    ends_at: Norn.ref("core.time.instant"),
    timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
    activity_kind: Schema.Literals(["sync", "transform", "training", "evaluation", "restatement", "export"]),
    code_digest: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["run_id"] }],
  links: [
    {
      id: "actor",
      description: "Steward or system agent that ran the pipeline.",
      target: Steward,
      cardinality: { to: "one", from: "many" },
    },
  ],
})

/**
 * Concrete immutable snapshot record implementing `data.DatasetSnapshot`, with
 * an `acyclic: true` `supersedes` chain.
 */
export const SnapshotRecord = Norn.type({
  name: "data.SnapshotRecord",
  role: "thing",
  title: "Snapshot record",
  version: "0.1.0",
  description: "Reference immutable dataset snapshot implementing data.DatasetSnapshot with an acyclic supersedes revision link.",
  implements: [DatasetSnapshot],
  properties: SnapshotProperties,
  identity: [{ name: "primary", properties: ["snapshot_id"] }],
  links: [
    {
      id: "dataset",
      description: "Catalog dataset materialized by this snapshot.",
      target: CatalogDataset,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "supersedes",
      description: "Prior immutable snapshot superseded by this revision (strictly acyclic).",
      target: "data.SnapshotRecord",
      cardinality: { to: "one", from: "many" },
      acyclic: true,
    },
    {
      id: "generated_by",
      description: "Pipeline run that generated this snapshot.",
      target: PipelineRun,
      cardinality: { to: "one", from: "many" },
    },
  ],
})

/**
 * Concrete derivation edge implementing `data.Derivation` and inheriting all
 * `DATA-CAUSAL-1` (`LAW-LINK-1`) causality laws.
 */
export const DerivationLink = Norn.type({
  name: "data.DerivationLink",
  role: "happening",
  title: "Derivation link",
  version: "0.1.0",
  description: "Reference qualified lineage edge implementing data.Derivation and inheriting all DATA-CAUSAL-1 temporal causality laws.",
  implements: [Derivation],
  properties: DerivationProperties,
  identity: [{ name: "primary", properties: ["derivation_id"] }],
  links: [
    {
      id: "input_snapshot",
      description: "Upstream snapshot read as input.",
      target: SnapshotRecord,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "output_snapshot",
      description: "Downstream snapshot produced as output.",
      target: SnapshotRecord,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "activity",
      description: "Pipeline run that performed the derivation.",
      target: PipelineRun,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

/**
 * Concrete telemetry/log/capture event implementing `data.Observation`.
 */
export const TelemetryEvent = Norn.type({
  name: "data.TelemetryEvent",
  role: "happening",
  title: "Telemetry event",
  version: "0.1.0",
  description: "Reference append-only observation implementing data.Observation.",
  implements: [Observation],
  properties: ObservationProperties,
  identity: [{ name: "primary", properties: ["observation_id"] }],
  links: [
    {
      id: "dataset",
      description: "Optional catalog dataset this telemetry event belongs to.",
      target: CatalogDataset,
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "observed_by",
      description: "Sensor or system steward that captured this event.",
      target: Steward,
      cardinality: { to: "one", from: "many" },
    },
  ],
})

/**
 * Concrete quality-finding resolution case implementing `data.QualityFinding`
 * with a sound `LIFE-SOUND-1..8` resolution lifecycle (`open -> acknowledged |
 * quarantined | resolved`, `acknowledged -> quarantined | resolved`,
 * `quarantined -> resolved`, `resolved` immutable completed ending).
 */
export const QualityCase = Norn.type({
  name: "data.QualityCase",
  role: "happening",
  title: "Quality case",
  version: "0.1.0",
  description: "Reference quality-finding resolution case implementing data.QualityFinding with a LIFE-SOUND-1..8 resolution lifecycle.",
  implements: [QualityFinding],
  properties: Schema.Struct({
    finding_id: Schema.String,
    dimension: Schema.Literals(["accuracy", "completeness", "consistency", "credibility", "currentness"]),
    severity: Schema.Literals(["info", "warning", "error"]),
    offending_path: Schema.String,
    status: Schema.Literals(["open", "acknowledged", "quarantined", "resolved"]),
  }),
  identity: [{ name: "primary", properties: ["finding_id"] }],
  defaults: { status: "open" },
  links: [
    {
      id: "snapshot",
      description: "Snapshot record evaluated by this quality case.",
      target: SnapshotRecord,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: "open",
      states: {
        open: {
          on: {
            acknowledge: "acknowledged",
            quarantine: "quarantined",
            resolve: "resolved",
          },
        },
        acknowledged: {
          on: {
            quarantine: "quarantined",
            resolve: "resolved",
          },
        },
        quarantined: {
          on: {
            resolve: "resolved",
          },
        },
        resolved: {
          final: "completed",
          immutable: true,
        },
      },
    },
  ],
})

// ---------------------------------------------------------------------------
// Governed actions for concrete reference object types and QualityCase lifecycle
// ---------------------------------------------------------------------------

export const RegisterCatalogDataset = Norn.action({
  id: "data.catalog_dataset.register",
  title: "Register a catalog dataset",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: DatasetProperties,
  output: Schema.Struct({ dataset_id: Schema.String }),
  writes: [{ type: CatalogDataset, change: "created" }],
  effect: { kind: "upsert_input_object", type: CatalogDataset, objectIdField: "dataset_id", writeMode: { kind: "create_only" } },
})

export const RecordPipelineRun = Norn.action({
  id: "data.pipeline_run.record",
  title: "Record a provenance pipeline run",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    run_id: Schema.String,
    starts_at: Norn.timestamp("utc"),
    ends_at: Norn.timestamp("utc"),
    timezone: Schema.NullOr(Schema.String),
    activity_kind: Schema.Literals(["sync", "transform", "training", "evaluation", "restatement", "export"]),
    code_digest: Schema.String,
  }),
  output: Schema.Struct({ run_id: Schema.String }),
  writes: [{ type: PipelineRun, change: "created" }],
  effect: { kind: "upsert_input_object", type: PipelineRun, objectIdField: "run_id", writeMode: { kind: "create_only" } },
})

const RecordSnapshotInput = Schema.Struct({
  snapshot_id: Schema.String,
  dataset_id: Schema.String,
  activity_id: Schema.String,
  content_sha256: Schema.String,
  schema_digest: Schema.String,
  record_count: Schema.Int,
  byte_size: Schema.Int,
  effective_at: Norn.timestamp("utc"),
  known_at: Norn.timestamp("utc"),
})

export const RecordSnapshot = Norn.action({
  id: "data.snapshot_record.record",
  title: "Record an immutable dataset snapshot produced by a pipeline run",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: RecordSnapshotInput,
  output: Schema.Struct({ snapshot_id: Schema.String }),
  slice: [
    { name: "dataset", type: CatalogDataset, key: { property: { from: "input", property: "dataset_id" } } },
    { name: "activity", type: PipelineRun, key: { property: { from: "input", property: "activity_id" } } },
  ],
  preconditions: [
    Norn.expression(
      Schema.Struct({
        input: RecordSnapshotInput,
        slice: Schema.Struct({
          activity: Schema.Struct({
            starts_at: Norn.timestamp("utc"),
            ends_at: Norn.timestamp("utc"),
          }),
        }),
      }),
      (ctx) =>
        Norn.timestampAtOrBefore(ctx.slice.activity.ends_at, ctx.input.known_at) &&
        Norn.timestampAtOrBefore(ctx.input.effective_at, ctx.input.known_at),
      { root: "context" },
    ),
  ],
  createLinks: [
    { link: "dataset", targetType: CatalogDataset, targetIdField: "dataset_id" },
    { link: "generated_by", targetType: PipelineRun, targetIdField: "activity_id" },
  ],
  writes: [{ type: SnapshotRecord, change: "created" }],
  effect: { kind: "upsert_input_object", type: SnapshotRecord, objectIdField: "snapshot_id", writeMode: { kind: "create_only" } },
})

export const RecordTelemetryEvent = Norn.action({
  id: "data.telemetry_event.record",
  title: "Record an append-only telemetry observation",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    observation_id: Schema.String,
    occurred_at: Norn.timestamp("utc"),
    observed_at: Norn.timestamp("utc"),
    source_system: Schema.String,
    observable_property: Schema.String,
    payload_digest: Schema.String,
  }),
  output: Schema.Struct({ observation_id: Schema.String }),
  writes: [{ type: TelemetryEvent, change: "created" }],
  effect: { kind: "upsert_input_object", type: TelemetryEvent, objectIdField: "observation_id", writeMode: { kind: "create_only" } },
})

export const OpenQualityCase = Norn.action({
  id: "data.quality_case.open",
  title: "Open a data quality resolution case",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    finding_id: Schema.String,
    snapshot_id: Schema.String,
    dimension: Schema.Literals(["accuracy", "completeness", "consistency", "credibility", "currentness"]),
    severity: Schema.Literals(["info", "warning", "error"]),
    offending_path: Schema.String,
  }),
  output: Schema.Struct({ finding_id: Schema.String }),
  slice: [
    { name: "snapshot", type: SnapshotRecord, key: { property: { from: "input", property: "snapshot_id" } } },
  ],
  createLinks: [
    { link: "snapshot", targetType: SnapshotRecord, targetIdField: "snapshot_id" },
  ],
  writes: [{ type: QualityCase, change: "created" }],
  effect: { kind: "upsert_input_object", type: QualityCase, objectIdField: "finding_id", writeMode: { kind: "create_only" } },
})

export const AcknowledgeQualityCase = Norn.action({
  id: "data.quality_case.acknowledge",
  title: "Acknowledge a data quality case",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ finding_id: Schema.String }),
  output: Schema.Struct({ finding_id: Schema.String }),
  writes: [{ type: QualityCase, change: "modified" }],
  effect: { kind: "fire_transition", type: QualityCase, objectIdField: "finding_id", lifecycle: "status", transition: "acknowledge" },
})

export const QuarantineQualityCase = Norn.action({
  id: "data.quality_case.quarantine",
  title: "Quarantine a dataset snapshot quality case",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ finding_id: Schema.String }),
  output: Schema.Struct({ finding_id: Schema.String }),
  writes: [{ type: QualityCase, change: "modified" }],
  effect: { kind: "fire_transition", type: QualityCase, objectIdField: "finding_id", lifecycle: "status", transition: "quarantine" },
})

export const ResolveQualityCase = Norn.action({
  id: "data.quality_case.resolve",
  title: "Resolve a data quality case",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ finding_id: Schema.String }),
  output: Schema.Struct({ finding_id: Schema.String }),
  writes: [{ type: QualityCase, change: "modified" }],
  effect: { kind: "fire_transition", type: QualityCase, objectIdField: "finding_id", lifecycle: "status", transition: "resolve" },
})
