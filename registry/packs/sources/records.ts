import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Connected source systems and their sync runs (#921). report.json is one
// report of each dataset, as the cell writes it. The cell's own sync
// reports each run as an observation of the `norn-connect` source, and this
// mapping lands it as typed objects, so Studio reads connections, freshness
// and quarantine counts through the ordinary object routes. Times are what
// the cell's clock read; nothing here implies a fresher view than the last
// completed run.

export const Synced = Norn.interfaceType({
  name: "source.Synced", title: "A record of the cell's own source sync", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String }),
})

export const Connection = Norn.type({
  name: "source.Connection", title: "Connected source system", version: "0.1.0", implements: [Synced],
  properties: Schema.Struct({
    id: Schema.String,
    system: Schema.String,
    label: Schema.String,
    target: Schema.String,
    auth: Schema.Literals(["api_key", "client_credentials", "presigned_upload"]),
    status: Schema.Literals(["syncing", "ok", "partial", "failed"]),
    last_sync_started_at: Norn.ref("iso8601.timestamp"),
    last_sync_ended_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
    last_trigger: Schema.Literals(["schedule", "manual", "upload"]),
    watermark: Schema.String,
    rows_read: Schema.Int,
    rows_landed: Schema.Int,
    rows_quarantined: Schema.Int,
    quarantine_reasons: Schema.String,
    last_error_code: Schema.NullOr(Schema.String),
    last_error_message: Schema.NullOr(Schema.String),
    connector_id: Schema.String,
    source_schema_id: Schema.String,
    mapping_id: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const SyncRun = Norn.type({
  name: "source.SyncRun", title: "Source sync run", version: "0.1.0", implements: [Synced],
  properties: Schema.Struct({
    id: Schema.String,
    connection_id: Schema.String,
    trigger: Schema.Literals(["schedule", "manual", "upload"]),
    actor: Schema.String,
    started_at: Norn.ref("iso8601.timestamp"),
    ended_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
    outcome: Schema.Literals(["running", "ok", "partial", "failed"]),
    watermark_from: Schema.String,
    watermark_to: Schema.String,
    rows_read: Schema.Int,
    rows_landed: Schema.Int,
    rows_quarantined: Schema.Int,
    quarantine_reasons: Schema.String,
    error_code: Schema.NullOr(Schema.String),
    error_message: Schema.NullOr(Schema.String),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "connection", target: Connection, cardinality: { to: "one", from: "many", required: true } },
  ],
})

export const RunSource = Norn.sourceSchema({
  id: "norn.connect.source",
  title: "The cell's own source sync reports",
  version: "0.1.0",
  source: {system: "norn-connect", instance: "cell", version: "norn-connect/1"},
  observedAt: "2026-11-12T00:00:00Z",
  adapter: {name: "norn-pull", version: "1"},
  rawMetadata: {uri: "registry/packs/sources/report.json", sha256: "b13999f2a32856777214f172501f668a9800c70304188a01c6f79f8a0a2f6f3c"},
  datasets: [
    {id: "connection", nativeName: "Connection state after a sync run", kind: "document"},
    {id: "sync_run", nativeName: "Sync run report", kind: "document"}
  ],
  fields: [
    {dataset: "connection", name: "id", ordinal: 0, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "system", ordinal: 1, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "label", ordinal: 2, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "target", ordinal: 3, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "auth", ordinal: 4, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "status", ordinal: 5, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "last_sync_started_at", ordinal: 6, nativeType: "rfc3339", typeCandidate: {type: "datetime", origin: "declared"}, required: true},
    {dataset: "connection", name: "last_sync_ended_at", ordinal: 7, nativeType: "rfc3339", typeCandidate: {type: "datetime", origin: "declared"}, required: false},
    {dataset: "connection", name: "last_trigger", ordinal: 8, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "watermark", ordinal: 9, nativeType: "json", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "rows_read", ordinal: 10, nativeType: "count", typeCandidate: {type: "integer", origin: "declared"}, required: true},
    {dataset: "connection", name: "rows_landed", ordinal: 11, nativeType: "count", typeCandidate: {type: "integer", origin: "declared"}, required: true},
    {dataset: "connection", name: "rows_quarantined", ordinal: 12, nativeType: "count", typeCandidate: {type: "integer", origin: "declared"}, required: true},
    {dataset: "connection", name: "quarantine_reasons", ordinal: 13, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "last_error_code", ordinal: 14, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: false},
    {dataset: "connection", name: "last_error_message", ordinal: 15, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: false},
    {dataset: "connection", name: "connector_id", ordinal: 16, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "source_schema_id", ordinal: 17, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "connection", name: "mapping_id", ordinal: 18, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "id", ordinal: 0, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "connection_id", ordinal: 1, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "trigger", ordinal: 2, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "actor", ordinal: 3, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "started_at", ordinal: 4, nativeType: "rfc3339", typeCandidate: {type: "datetime", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "ended_at", ordinal: 5, nativeType: "rfc3339", typeCandidate: {type: "datetime", origin: "declared"}, required: false},
    {dataset: "sync_run", name: "outcome", ordinal: 6, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "watermark_from", ordinal: 7, nativeType: "json", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "watermark_to", ordinal: 8, nativeType: "json", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "rows_read", ordinal: 9, nativeType: "count", typeCandidate: {type: "integer", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "rows_landed", ordinal: 10, nativeType: "count", typeCandidate: {type: "integer", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "rows_quarantined", ordinal: 11, nativeType: "count", typeCandidate: {type: "integer", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "quarantine_reasons", ordinal: 12, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sync_run", name: "error_code", ordinal: 13, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: false},
    {dataset: "sync_run", name: "error_message", ordinal: 14, nativeType: "text", typeCandidate: {type: "string", origin: "declared"}, required: false}
  ],
  constraints: [
    {id: "connection_id", kind: "primary_key", dataset: "connection", fields: ["id"], origin: "declared"},
    {id: "sync_run_id", kind: "primary_key", dataset: "sync_run", fields: ["id"], origin: "declared"}
  ]
})

export const RunMapping = Norn.mapping({
  id: "norn.connect.mapping",
  title: "Sync reports to connections and runs",
  version: "0.1.0",
  source: {schemaId: "norn.connect.source", version: "0.1.0", sha256: "bf3e24044948782b5030ea5ed67eb1fcc92c7d7fe9d198678c9b5f5da57cb017"},
  sourceContract: {kind: "generic"},
  interfaces: [Synced],
  maps: [
    {
      id: "connection",
      source: {dataset: "connection"},
      target: {type: Connection, interface: Synced},
      subject: "input.id",
      sourceRecord: "'norn-connect/cell/connection/' + input.id",
      properties: [
        {property: "id", value: "input.id"}, {property: "system", value: "input.system"}, {property: "label", value: "input.label"},
        {property: "target", value: "input.target"}, {property: "auth", value: "input.auth"}, {property: "status", value: "input.status"},
        {property: "last_sync_started_at", value: "timestamp(input.last_sync_started_at)"},
        {property: "last_sync_ended_at", value: "input.last_sync_ended_at == null ? null : timestamp(input.last_sync_ended_at)"},
        {property: "last_trigger", value: "input.last_trigger"}, {property: "watermark", value: "input.watermark"},
        {property: "rows_read", value: "input.rows_read"}, {property: "rows_landed", value: "input.rows_landed"},
        {property: "rows_quarantined", value: "input.rows_quarantined"}, {property: "quarantine_reasons", value: "input.quarantine_reasons"},
        {property: "last_error_code", value: "input.last_error_code"}, {property: "last_error_message", value: "input.last_error_message"},
        {property: "connector_id", value: "input.connector_id"}, {property: "source_schema_id", value: "input.source_schema_id"},
        {property: "mapping_id", value: "input.mapping_id"}
      ]
    },
    {
      id: "sync_run",
      source: {dataset: "sync_run"},
      target: {type: SyncRun, interface: Synced},
      subject: "input.id",
      sourceRecord: "'norn-connect/cell/sync_run/' + input.id",
      properties: [
        {property: "id", value: "input.id"}, {property: "connection_id", value: "input.connection_id"},
        {property: "trigger", value: "input.trigger"}, {property: "actor", value: "input.actor"},
        {property: "started_at", value: "timestamp(input.started_at)"},
        {property: "ended_at", value: "input.ended_at == null ? null : timestamp(input.ended_at)"},
        {property: "outcome", value: "input.outcome"}, {property: "watermark_from", value: "input.watermark_from"},
        {property: "watermark_to", value: "input.watermark_to"}, {property: "rows_read", value: "input.rows_read"},
        {property: "rows_landed", value: "input.rows_landed"}, {property: "rows_quarantined", value: "input.rows_quarantined"},
        {property: "quarantine_reasons", value: "input.quarantine_reasons"}, {property: "error_code", value: "input.error_code"},
        {property: "error_message", value: "input.error_message"}
      ],
      links: [{link: "connection", targetMap: "connection", key: "input.connection_id"}]
    }
  ]
})
