/**
 * HR Historical Import Pack (#1594): governed intake pipeline from raw
 * unstructured sources (`BlobRef`) through untrusted OCR/VLM `perform`
 * extraction into `DataQualityFinding` quarantine and 3-timestamp bitemporal
 * promotion.
 *
 * Raw chat/schedule excerpts carry `pii.raw_chat_media` (declared in
 * `registry/packs/hr/markings.ts`) so untrusted workers and unauthorized
 * readers never see raw personal media without audited reveal authority.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { OrgUnit } from "../org/org.ts"

// ---------------------------------------------------------------- Source Batches, Candidates & Findings

const SourceChannel = Schema.Literals([
  "whatsapp_export",
  "spreadsheet_csv",
  "schedule_photo",
  "erp_export",
])

const SourceBatchStatus = Schema.Literals([
  "received",
  "extracting",
  "reviewing",
  "closed",
])

export const SourceBatch = Norn.type({
  name: "hr_import.SourceBatch",
  role: "thing",
  title: "Historical import source batch",
  version: "0.1.0",
  description: "Raw content-addressed upload batch (BlobRef) captured from WhatsApp exports, spreadsheets, schedule photos, or ERP dumps before untrusted extraction.",
  properties: Schema.Struct({
    id: Schema.String,
    channel: SourceChannel,
    blob_ref: Schema.String,
    raw_excerpt: Schema.String,
    captured_at: Norn.timestamp("utc"),
    status: SourceBatchStatus,
  }),
  propertyMarkings: {
    raw_excerpt: ["pii.raw_chat_media"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "received",
  },
  links: [
    {
      id: "unit",
      description: "Branch or organizational unit owning this uploaded source batch.",
      target: OrgUnit,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["received"],
      terminal: ["closed"],
      immutable: ["closed"],
      transitions: [
        { id: "begin_extract", from: ["received"], to: "extracting" },
        { id: "begin_review", from: ["extracting"], to: "reviewing" },
        { id: "close", from: ["received", "extracting", "reviewing"], to: "closed" },
      ],
    },
  ],
})

const CandidateTargetKind = Schema.Literals([
  "person",
  "leave_request",
  "shift_commitment",
  "attendance_event",
])

const CandidateStatus = Schema.Literals([
  "staged",
  "quarantined",
  "promoted",
  "rejected",
])

const ExtractedCandidateProps = Schema.Struct({
  id: Schema.String,
  target_kind: CandidateTargetKind,
  confidence_bps: Schema.Int,
  source_span: Schema.String,
  extracted_at: Norn.timestamp("utc"),
  observed_at: Norn.timestamp("utc"),
  effective_on: Norn.date(),
  open_error_count: Schema.Int,
  review_receipt_digest: Schema.NullOr(Schema.String),
  status: CandidateStatus,
})

export const ExtractedCandidate = Norn.type({
  name: "hr_import.ExtractedCandidate",
  role: "happening",
  title: "Extracted import candidate",
  version: "0.1.0",
  description: "Untrusted OCR/VLM-extracted HR record carrying 3-timestamp bitemporal provenance (extracted_at, observed_at, effective_on) and gated promotion.",
  properties: ExtractedCandidateProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "staged",
    open_error_count: 0,
  },
  links: [
    {
      id: "batch",
      description: "Source upload batch from which this candidate was extracted.",
      target: SourceBatch,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["staged"],
      terminal: ["promoted", "rejected"],
      immutable: ["promoted"],
      transitions: [
        { id: "quarantine", from: ["staged"], to: "quarantined" },
        {
          id: "promote",
          from: ["staged", "quarantined"],
          to: "promoted",
          guards: [
            Norn.expression(
              ExtractedCandidateProps,
              (self) => self.open_error_count === 0 && self.review_receipt_digest !== null,
            ),
          ],
        },
        { id: "reject", from: ["staged", "quarantined"], to: "rejected" },
      ],
    },
  ],
})

const FindingSeverity = Schema.Literals(["warning", "error"])
const FindingStatus = Schema.Literals(["open", "resolved", "waived"])

export const DataQualityFinding = Norn.type({
  name: "hr_import.DataQualityFinding",
  role: "happening",
  title: "Import data quality finding",
  version: "0.1.0",
  description: "Replay-stable quarantine finding attached to an ExtractedCandidate when imported data violates schema, temporal, or domain constraints.",
  properties: Schema.Struct({
    id: Schema.String,
    code: Norn.ref("core.code"),
    severity: FindingSeverity,
    field_path: Schema.String,
    message: Schema.String,
    status: FindingStatus,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "open",
  },
  links: [
    {
      id: "candidate",
      description: "Extracted candidate quarantined or annotated by this finding.",
      target: ExtractedCandidate,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["resolved", "waived"],
      transitions: [
        { id: "resolve", from: ["open"], to: "resolved" },
        { id: "waive", from: ["open"], to: "waived" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Untrusted OCR/VLM Function Handle

export const ExtractHrSourceFn = Norn.fn<
  { readonly batchId: string; readonly blobRef: string; readonly channel: string },
  { readonly candidateCount: number; readonly findingCount: number; readonly extractionDigest: string }
>({
  id: "norn.import.ocr_vlm_extract",
  version: "0.1.0",
})

// ---------------------------------------------------------------- Governed Laws & LAW-EXAMPLE-1 Cases

export const CandidateConfidenceBounds = Norn.law({
  id: "hr_import.ExtractedCandidate.confidence_bounds.law",
  title: "Extracted candidate confidence stays within 0..10000 basis points",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.ExtractedCandidate.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.ExtractedCandidate.confidence_bounds",
    owner: "hr_import",
    severity: "error",
    rationale: "OCR/VLM extraction confidence is expressed in integer basis points in [0, 10000] so downstream review gates never compare malformed scores.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.ExtractedCandidate",
    scope: {
      root: { kind: "entity", id: "hr_import.ExtractedCandidate" },
      objectTypes: ["hr_import.ExtractedCandidate"],
    },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.confidence_bps >= 0 and self.confidence_bps <= 10000",
    },
  },
})

export const CandidateConfidenceValidHolds = Norn.example("9250 bps confidence holds", {
  of: CandidateConfidenceBounds,
  given: {
    id: "cand_ok_1",
    target_kind: "attendance_event",
    confidence_bps: 9250,
    source_span: "row:2",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    open_error_count: 0,
    review_receipt_digest: null,
    status: "staged",
  },
  expect: "holds",
})

export const CandidateConfidenceOverflowViolated = Norn.example("confidence above 10000 bps is refused", {
  of: CandidateConfidenceBounds,
  given: {
    id: "cand_bad_conf",
    target_kind: "attendance_event",
    confidence_bps: 10500,
    source_span: "row:3",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    open_error_count: 0,
    review_receipt_digest: null,
    status: "staged",
  },
  expect: "violated",
})

export const PromotedCandidateRequiresCleanReview = Norn.law({
  id: "hr_import.ExtractedCandidate.promoted_requires_clean_review.law",
  title: "Promoted candidates must have zero open errors and an accepted review receipt",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.ExtractedCandidate.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.ExtractedCandidate.promoted_requires_clean_review",
    owner: "hr_import",
    severity: "error",
    rationale: "Untrusted OCR/VLM candidates cannot be promoted into authoritative HR records while error findings remain open or without an authenticated review receipt.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.ExtractedCandidate",
    scope: {
      root: { kind: "entity", id: "hr_import.ExtractedCandidate" },
      objectTypes: ["hr_import.ExtractedCandidate"],
    },
    condition: 'self.status == "promoted"',
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.open_error_count == 0 and self.review_receipt_digest != null",
    },
  },
})

export const StagedCandidateNotApplicable = Norn.example("staged candidate with open errors is not yet governed", {
  of: PromotedCandidateRequiresCleanReview,
  given: {
    id: "cand_staged",
    target_kind: "attendance_event",
    confidence_bps: 7400,
    source_span: "row:4",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    open_error_count: 1,
    review_receipt_digest: null,
    status: "staged",
  },
  expect: "not_applicable",
})

export const PromotedCandidateCleanHolds = Norn.example("promoted candidate with clean review holds", {
  of: PromotedCandidateRequiresCleanReview,
  given: {
    id: "cand_promoted",
    target_kind: "shift_commitment",
    confidence_bps: 9600,
    source_span: "line:1-3",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T18:05:00Z",
    effective_on: "2026-11-24",
    open_error_count: 0,
    review_receipt_digest: "sha256:rev001",
    status: "promoted",
  },
  expect: "holds",
})

export const PromotedCandidateWithOpenErrorViolated = Norn.example("promoted candidate with open error finding is refused", {
  of: PromotedCandidateRequiresCleanReview,
  given: {
    id: "cand_bad_promo",
    target_kind: "attendance_event",
    confidence_bps: 8100,
    source_span: "row:4",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    open_error_count: 1,
    review_receipt_digest: "sha256:rev002",
    status: "promoted",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Candidate Actions

const CandidateTransitionInput = Schema.Struct({
  candidateId: Schema.String,
})

const CandidateTransitionOutput = Schema.Struct({
  candidateId: Schema.String,
  status: Schema.String,
})

export const QuarantineCandidate = Norn.action({
  id: "hr_import.candidate.quarantine",
  title: "Quarantine extracted candidate",
  version: "0.1.0",
  description: "Move a staged OCR/VLM candidate into quarantine when one or more data quality findings require human resolution.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  input: CandidateTransitionInput,
  output: CandidateTransitionOutput,
  writes: [{ type: ExtractedCandidate, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExtractedCandidate,
    objectIdField: "candidateId",
    lifecycle: "status",
    transition: "quarantine",
  },
})

export const PromoteCandidate = Norn.action({
  id: "hr_import.candidate.promote",
  title: "Promote extracted candidate",
  version: "0.1.0",
  description: "Promote a reviewed candidate into authoritative HR state once zero error findings remain and a review receipt is recorded.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    {
      name: "candidate",
      type: "hr_import.ExtractedCandidate",
      key: { property: { from: "input", property: "candidateId" } },
    },
  ],
  preconditions: [
    Norn.unsafeZen<boolean>(
      "slice.candidate.open_error_count == 0 and slice.candidate.review_receipt_digest != null",
    ),
  ],
  input: CandidateTransitionInput,
  output: CandidateTransitionOutput,
  writes: [{ type: ExtractedCandidate, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExtractedCandidate,
    objectIdField: "candidateId",
    lifecycle: "status",
    transition: "promote",
  },
})

export const RejectCandidate = Norn.action({
  id: "hr_import.candidate.reject",
  title: "Reject extracted candidate",
  version: "0.1.0",
  description: "Reject an invalid or duplicate extracted candidate while preserving the source batch and findings as evidence.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  input: CandidateTransitionInput,
  output: CandidateTransitionOutput,
  writes: [{ type: ExtractedCandidate, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExtractedCandidate,
    objectIdField: "candidateId",
    lifecycle: "status",
    transition: "reject",
  },
})
