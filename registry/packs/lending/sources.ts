import { Norn } from "@norn/authoring"
import { Application, ApplicationEvent, CompletionScore, Offer, SourceEvent, SourceRecord } from "./lending.ts"

/**
 * The two sources the lending pack reads: the loan-application log (one row
 * per application, offer and event, prepared from the public BPIC 2017 log)
 * and the offline model's scores (one row per scored application, carrying
 * the model version). Each lands through the cell's own mapping, so every
 * object keeps the source record it came from.
 */

export const LogSource = Norn.sourceSchema({
  id: "lending.log.source", title: "Loan-application log, one row per application, offer and event", version: "0.1.0",
  source: { system: "public-loan-log", instance: "demo", version: "bpic2017-v1" },
  observedAt: "2017-02-01T15:00:00Z", adapter: { name: "lending-prepare", version: "0.1.0" },
  rawMetadata: { uri: "https://doi.org/10.4121/uuid:5f3067df-f10b-45da-b98b-86ae4c7a310b", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  datasets: [
    { id: "Application", nativeName: "Application case", kind: "document" },
    { id: "Offer", nativeName: "Offer", kind: "document" },
    { id: "Application Event", nativeName: "Log event", kind: "document" },
  ],
  fields: [
    { dataset: "Application", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application", name: "loan_goal", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application", name: "application_type", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application", name: "requested_amount", ordinal: 3, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Application", name: "created_at", ordinal: 4, nativeType: "Datetime", typeCandidate: { type: "datetime", origin: "declared" }, required: true },
    { dataset: "Application", name: "last_event_at", ordinal: 5, nativeType: "Datetime", typeCandidate: { type: "datetime", origin: "declared" }, required: true },
    { dataset: "Application", name: "stage", ordinal: 6, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application", name: "outcome", ordinal: 7, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application", name: "offer_count", ordinal: 8, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Application", name: "event_count", ordinal: 9, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Application", name: "completed_within_24h", ordinal: 10, nativeType: "Boolean", typeCandidate: { type: "boolean", origin: "declared" }, required: true },
    { dataset: "Application", name: "minutes_to_last_event", ordinal: 11, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Application", name: "minutes_to_complete", ordinal: 12, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: false },
    { dataset: "Application", name: "minutes_to_outcome", ordinal: 13, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: false },
    { dataset: "Offer", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Offer", name: "application_id", ordinal: 1, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: true },
    { dataset: "Offer", name: "offered_amount", ordinal: 2, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Offer", name: "monthly_cost", ordinal: 3, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Offer", name: "first_withdrawal", ordinal: 4, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Offer", name: "terms", ordinal: 5, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Offer", name: "credit_score", ordinal: 6, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Offer", name: "selected", ordinal: 7, nativeType: "Boolean", typeCandidate: { type: "boolean", origin: "declared" }, required: true },
    { dataset: "Offer", name: "status", ordinal: 8, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Offer", name: "created_at", ordinal: 9, nativeType: "Datetime", typeCandidate: { type: "datetime", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "application_id", ordinal: 1, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "activity", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "origin", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "lifecycle", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "resource", ordinal: 5, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Application Event", name: "offer_id", ordinal: 6, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "Application Event", name: "occurred_at", ordinal: 7, nativeType: "Datetime", typeCandidate: { type: "datetime", origin: "declared" }, required: true },
  ],
  codeSets: [
    { id: "application_type", dataset: "Application", field: "application_type", origin: "declared", values: [{ value: "New credit" }, { value: "Limit raise" }] },
    { id: "stage", dataset: "Application", field: "stage", origin: "declared", values: [{ value: "submitted" }, { value: "validated" }, { value: "offered" }, { value: "accepted" }, { value: "denied" }, { value: "cancelled" }] },
    { id: "outcome", dataset: "Application", field: "outcome", origin: "declared", values: [{ value: "accepted" }, { value: "denied" }, { value: "cancelled" }, { value: "open" }] },
    { id: "offer_status", dataset: "Offer", field: "status", origin: "declared", values: [{ value: "created" }, { value: "sent" }, { value: "returned" }, { value: "accepted" }, { value: "refused" }, { value: "cancelled" }] },
    { id: "origin", dataset: "Application Event", field: "origin", origin: "declared", values: [{ value: "Application" }, { value: "Offer" }, { value: "Workflow" }] },
  ],
  constraints: [
    { id: "application_identity", kind: "primary_key", dataset: "Application", fields: ["id"], origin: "declared" },
    { id: "offer_identity", kind: "primary_key", dataset: "Offer", fields: ["id"], origin: "declared" },
    { id: "event_identity", kind: "primary_key", dataset: "Application Event", fields: ["id"], origin: "declared" },
  ],
})

export const LogMapping = Norn.mapping({
  id: "lending.log.mapping", title: "Loan applications, offers and events", version: "0.1.0",
  source: { schemaId: "lending.log.source", version: "0.1.0", sha256: "c5dc7b305f7f96ca91313a7ef9febbeba3c6bd2f44a31162d2111d1c7d8bb8b6" },
  sourceContract: { kind: "generic" }, interfaces: [SourceRecord, SourceEvent],
  maps: [
    {
      id: "application", source: { dataset: "Application" }, target: { type: Application, interface: SourceRecord },
      subject: "input.id", sourceRecord: "'public-loan-log/application/' + input.id",
      properties: [
        { property: "id", value: "input.id" }, { property: "loan_goal", value: "input.loan_goal" },
        { property: "application_type", value: "input.application_type" }, { property: "currency", value: "'EUR'" },
        { property: "requested_amount", value: "input.requested_amount" },
        { property: "created_at", value: "timestamp(input.created_at)" }, 
        
        { property: "last_event_at", value: "timestamp(input.last_event_at)" },
        { property: "stage", value: "input.stage" }, { property: "outcome", value: "input.outcome" },
        { property: "offer_count", value: "input.offer_count" }, { property: "event_count", value: "input.event_count" },
        { property: "completed_within_24h", value: "input.completed_within_24h" },
        { property: "minutes_to_last_event", value: "input.minutes_to_last_event" },
        { property: "minutes_to_complete", value: "input.minutes_to_complete" },
        { property: "minutes_to_outcome", value: "input.minutes_to_outcome" },
      ],
      lifecycles: [{ property: "review", state: "'pending'" }],
    },
    {
      id: "offer", source: { dataset: "Offer" }, target: { type: Offer, interface: SourceRecord },
      subject: "input.id", sourceRecord: "'public-loan-log/offer/' + input.id",
      properties: [
        { property: "id", value: "input.id" }, { property: "application_id", value: "input.application_id" },
        { property: "currency", value: "'EUR'" },
        { property: "offered_amount", value: "input.offered_amount" }, { property: "monthly_cost", value: "input.monthly_cost" },
        { property: "first_withdrawal", value: "input.first_withdrawal" }, { property: "terms", value: "input.terms" },
        { property: "credit_score", value: "input.credit_score" }, { property: "selected", value: "input.selected" },
        { property: "status", value: "input.status" }, { property: "created_at", value: "timestamp(input.created_at)" },
      ],
      links: [{ link: "application", targetMap: "application", key: "input.application_id" }],
    },
    {
      id: "event", source: { dataset: "Application Event" }, target: { type: ApplicationEvent, interface: SourceEvent },
      subject: "input.id", sourceRecord: "'public-loan-log/event/' + input.id",
      properties: [
        { property: "id", value: "input.id" }, { property: "application_id", value: "input.application_id" },
        { property: "activity", value: "input.activity" }, { property: "origin", value: "input.origin" },
        { property: "lifecycle", value: "input.lifecycle" }, { property: "resource", value: "input.resource" },
        { property: "offer_id", value: "input.offer_id" }, { property: "occurred_at", value: "timestamp(input.occurred_at)" },
      ],
      links: [{ link: "application", targetMap: "application", key: "input.application_id" }],
    },
  ],
})

export const ScoreSource = Norn.sourceSchema({
  id: "lending.scores.source", title: "Offline completion-model scores", version: "0.1.0",
  source: { system: "offline-model", instance: "demo", version: "scores-v1" },
  observedAt: "2026-11-14T00:00:00Z", adapter: { name: "lending-score", version: "0.1.0" },
  rawMetadata: { uri: "evidence://lending/completion-model", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  datasets: [{ id: "Model Score", nativeName: "One scored application", kind: "document" }],
  fields: [
    { dataset: "Model Score", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Model Score", name: "application_id", ordinal: 1, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: true },
    { dataset: "Model Score", name: "model_version", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Model Score", name: "score", ordinal: 3, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Model Score", name: "scored_at", ordinal: 4, nativeType: "Datetime", typeCandidate: { type: "datetime", origin: "declared" }, required: true },
    { dataset: "Model Score", name: "features_digest", ordinal: 5, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [{ id: "score_identity", kind: "primary_key", dataset: "Model Score", fields: ["id"], origin: "declared" }],
})

export const ScoreMapping = Norn.mapping({
  id: "lending.scores.mapping", title: "Completion scores with their model version", version: "0.1.0",
  source: { schemaId: "lending.scores.source", version: "0.1.0", sha256: "60cba655c13cb648f72e079e9804349d440e1a8edceb69212c5a628adf9c8b0a" },
  sourceContract: { kind: "generic" }, interfaces: [SourceRecord],
  maps: [{
    id: "score", source: { dataset: "Model Score" }, target: { type: CompletionScore, interface: SourceRecord },
    subject: "input.id", sourceRecord: "'offline-model/' + input.id",
    properties: [
      { property: "id", value: "input.id" }, { property: "application_id", value: "input.application_id" },
      { property: "model_version", value: "input.model_version" }, { property: "score", value: "input.score" },
      { property: "scored_at", value: "timestamp(input.scored_at)" }, { property: "features_digest", value: "input.features_digest" },
    ],
  }],
})
