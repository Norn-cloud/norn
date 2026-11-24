/**
 * HR Historical Import Pack (#1594): governed intake pipeline from raw
 * unstructured sources (`norn.SourceDocument`) through untrusted OCR/VLM
 * `perform` extraction and `norn.SourceEvidence` field anchors into immutable
 * `DataQualityFinding` quarantine (`FEED-CONFLICT` / supersession) and
 * 3-timestamp bitemporal promotion.
 *
 * Strictly jurisdiction-agnostic: zero country/city/currency hardcoding.
 * Raw chat/schedule excerpts carry `pii.raw_chat_media` (declared in
 * `registry/packs/hr/markings.ts`) so untrusted workers and unauthorized
 * readers never see raw personal media without audited reveal authority.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { SourceDocument, SourceEvidence } from "../../core/src/evidence.ts"
import { OrgUnit } from "../org/org.ts"

// ---------------------------------------------------------------- Source Batches (Core 07 norn.SourceDocument)

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

const SourceBatchProps = Schema.Struct({
  id: Schema.String,
  channel: SourceChannel,
  document_uri: Schema.String,
  document_digest: Schema.String,
  media_type: Schema.String,
  extractor: Schema.String,
  extractor_version: Schema.String,
  raw_excerpt: Schema.String,
  captured_at: Norn.timestamp("utc"),
  status: SourceBatchStatus,
})

export const SourceBatch = Norn.type({
  name: "hr_import.SourceBatch",
  role: "thing",
  title: "Historical import source batch",
  version: "0.1.0",
  description: "Content-addressed upload batch implementing Core 07 norn.SourceDocument before untrusted extraction.",
  implements: [SourceDocument],
  properties: SourceBatchProps,
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

// ---------------------------------------------------------------- Extracted Candidates & Supersession

const CandidateTargetKind = Schema.Literals([
  "person",
  "leave_request",
  "shift_commitment",
  "attendance_event",
])

const CandidateDisputeState = Schema.Literals([
  "clean",
  "quarantined",
  "disputed",
])

const CandidateStatus = Schema.Literals([
  "staged",
  "quarantined",
  "promoted",
  "rejected",
  "superseded",
])

const ExtractedCandidateProps = Schema.Struct({
  id: Schema.String,
  target_kind: CandidateTargetKind,
  source_record_key: Schema.String,
  payload_digest: Schema.String,
  confidence_bp: Schema.Int,
  source_span: Schema.String,
  extracted_at: Norn.timestamp("utc"),
  observed_at: Norn.timestamp("utc"),
  effective_on: Norn.date(),
  dispute_state: CandidateDisputeState,
  review_receipt_digest: Schema.NullOr(Schema.String),
  reviewed_by_user_id: Schema.NullOr(Schema.String),
  status: CandidateStatus,
})

export const ExtractedCandidate = Norn.type({
  name: "hr_import.ExtractedCandidate",
  role: "happening",
  title: "Extracted import candidate",
  version: "0.1.0",
  description: "Untrusted extracted HR proposal carrying 3-timestamp bitemporal provenance, content digest, explicit dispute state, and immutable terminal promotion/rejection/supersession.",
  properties: ExtractedCandidateProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    dispute_state: "clean",
    status: "staged",
  },
  links: [
    {
      id: "batch",
      description: "Source upload batch from which this candidate was extracted.",
      target: SourceBatch,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "supersedes",
      description: "Prior quarantined or disputed candidate superseded by this corrected candidate (append-only Core 07 lineage).",
      target: "hr_import.ExtractedCandidate",
      cardinality: { to: "one", from: "one" },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["staged"],
      terminal: ["promoted", "rejected", "superseded"],
      immutable: ["promoted", "rejected", "superseded"],
      transitions: [
        { id: "quarantine", from: ["staged"], to: "quarantined" },
        {
          id: "promote",
          from: ["staged", "quarantined"],
          to: "promoted",
          guards: [
            Norn.expression(
              ExtractedCandidateProps,
              (self) =>
                self.dispute_state === "clean" &&
                self.review_receipt_digest !== null &&
                self.reviewed_by_user_id !== null,
            ),
          ],
        },
        { id: "reject", from: ["staged", "quarantined"], to: "rejected" },
        { id: "supersede", from: ["staged", "quarantined"], to: "superseded" },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Field-Level Source Evidence (Core 07 norn.SourceEvidence)

const FieldEvidenceProps = Schema.Struct({
  id: Schema.String,
  subject_property: Schema.String,
  subject_type: Schema.NullOr(Schema.String),
  subject_key: Schema.NullOr(Schema.String),
  document_uri: Schema.String,
  document_digest: Schema.String,
  media_type: Schema.String,
  page: Schema.Int,
  bbox_x: Norn.decimal(4),
  bbox_y: Norn.decimal(4),
  bbox_w: Norn.decimal(4),
  bbox_h: Norn.decimal(4),
  text: Schema.String,
  extractor: Schema.String,
  extractor_version: Schema.String,
  confidence_bp: Schema.NullOr(Schema.Int),
  source_anchor: Schema.String,
  record_digest: Schema.String,
})

export const FieldEvidence = Norn.type({
  name: "hr_import.FieldEvidence",
  role: "thing",
  title: "Extracted field evidence",
  version: "0.1.0",
  description: "Field-level provenance anchor implementing Core 07 norn.SourceEvidence (exact document digest, page, normalized bounding box, line/row anchor, and extractor pin).",
  implements: [SourceEvidence],
  properties: FieldEvidenceProps,
  propertyMarkings: {
    text: ["pii.raw_chat_media"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    {
      id: "batch",
      description: "Source batch document from which this field evidence was anchored.",
      target: SourceBatch,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "candidate",
      description: "Extracted candidate supported or disputed by this field evidence.",
      target: ExtractedCandidate,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
})

// ---------------------------------------------------------------- Immutable Data Quality & FEED-CONFLICT Findings

const FindingKind = Schema.Literals([
  "schema_violation",
  "missing_anchor",
  "low_confidence",
  "duplicate_key",
  "feed_conflict",
  "domain_law_violation",
])

const FindingSeverity = Schema.Literals(["warning", "error"])
const QuarantineDisposition = Schema.Literals(["quarantined", "disputed", "advisory"])
const FindingStatus = Schema.Literals(["open", "resolved", "superseded", "rejected"])
const ResolutionAction = Schema.Literals([
  "accept_first",
  "accept_second_as_correction",
  "reject_candidate",
  "revalidated_correction",
])

const DataQualityFindingSubject = Schema.Struct({
  finding_kind: FindingKind,
  severity: FindingSeverity,
  quarantine_disposition: QuarantineDisposition,
  conflicting_payload_digest: Schema.NullOr(Schema.String),
  resolution_action: Schema.NullOr(ResolutionAction),
  resolution_receipt_digest: Schema.NullOr(Schema.String),
  resolved_by_user_id: Schema.NullOr(Schema.String),
  resolution_reason: Schema.NullOr(Schema.String),
  status: FindingStatus,
})

const DataQualityFindingProps = Schema.Struct({
  id: Schema.String,
  code: Norn.ref("core.code"),
  finding_kind: FindingKind,
  severity: FindingSeverity,
  quarantine_disposition: QuarantineDisposition,
  field_path: Schema.String,
  law_pin: Schema.NullOr(Schema.String),
  conflicting_payload_digest: Schema.NullOr(Schema.String),
  message: Schema.String,
  resolution_action: Schema.NullOr(ResolutionAction),
  resolution_receipt_digest: Schema.NullOr(Schema.String),
  resolved_by_user_id: Schema.NullOr(Schema.String),
  resolution_reason: Schema.NullOr(Schema.String),
  status: FindingStatus,
})

export const DataQualityFinding = Norn.type({
  name: "hr_import.DataQualityFinding",
  role: "happening",
  title: "Import data quality finding",
  version: "0.1.0",
  description: "Replay-stable quarantine or Core 07 FEED-CONFLICT finding attached to an ExtractedCandidate; terminal resolution states are strictly immutable and require audited receipt evidence (never silently waived).",
  properties: DataQualityFindingProps,
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: {
    status: "open",
  },
  links: [
    {
      id: "candidate",
      description: "Extracted candidate quarantined or disputed by this finding.",
      target: ExtractedCandidate,
      cardinality: { to: "one", from: "many", required: true },
    },
    {
      id: "evidence",
      description: "Optional field evidence anchor pinpointing where the violation or conflict was observed.",
      target: FieldEvidence,
      cardinality: { to: "one", from: "many" },
    },
    {
      id: "superseded_by",
      description: "Optional superseding candidate that resolved this finding via append-only correction.",
      target: ExtractedCandidate,
      cardinality: { to: "one", from: "many" },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["open"],
      terminal: ["resolved", "superseded", "rejected"],
      immutable: ["resolved", "superseded", "rejected"],
      transitions: [
        {
          id: "resolve",
          from: ["open"],
          to: "resolved",
          guards: [
            Norn.expression(
              DataQualityFindingSubject,
              (self) =>
                self.resolution_action !== null &&
                self.resolution_receipt_digest !== null &&
                self.resolved_by_user_id !== null &&
                self.resolution_reason !== null,
            ),
          ],
        },
        {
          id: "supersede",
          from: ["open"],
          to: "superseded",
          guards: [
            Norn.expression(
              DataQualityFindingSubject,
              (self) =>
                (self.resolution_action ?? "") === "accept_second_as_correction" &&
                self.resolution_receipt_digest !== null &&
                self.resolved_by_user_id !== null &&
                self.resolution_reason !== null,
            ),
          ],
        },
        {
          id: "reject",
          from: ["open"],
          to: "rejected",
          guards: [
            Norn.expression(
              DataQualityFindingSubject,
              (self) =>
                (self.resolution_action ?? "") === "reject_candidate" &&
                self.resolution_receipt_digest !== null &&
                self.resolved_by_user_id !== null &&
                self.resolution_reason !== null,
            ),
          ],
        },
      ],
    },
  ],
})

// ---------------------------------------------------------------- Untrusted OCR/VLM Function Handle

export const ExtractHrSourceFn = Norn.fn<
  { readonly batchId: string; readonly documentDigest: string; readonly channel: string },
  { readonly candidateCount: number; readonly findingCount: number; readonly extractionDigest: string }
>({
  id: "norn.import.ocr_vlm_extract",
  version: "0.1.0",
})

// ---------------------------------------------------------------- Governed Laws & LAW-EXAMPLE-1 Cases

export const SourceBatchContentAddressed = Norn.law({
  id: "hr_import.SourceBatch.content_addressed.law",
  title: "Source batches carry non-empty Core 07 SourceDocument provenance",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.SourceBatch.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.SourceBatch.content_addressed",
    owner: "hr_import",
    severity: "error",
    rationale: "Every ingested source batch must bind its exact document URI, content digest, media type, and extractor version per Core 07 OBS-EVIDENCE-1.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.SourceBatch",
    scope: {
      root: { kind: "entity", id: "hr_import.SourceBatch" },
      objectTypes: ["hr_import.SourceBatch"],
    },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        SourceBatchProps,
        (self) =>
          self.document_uri !== "" &&
          self.document_digest !== "" &&
          self.media_type !== "" &&
          self.extractor !== "" &&
          self.extractor_version !== "",
      ),
    },
  },
})

export const SourceBatchValidHolds = Norn.example("content-addressed source batch holds", {
  of: SourceBatchContentAddressed,
  given: {
    id: "batch_ok_1",
    channel: "spreadsheet_csv",
    document_uri: "evidence://imports/attendance-2026-11.csv",
    document_digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    media_type: "text/csv",
    extractor: "norn-hr-import-worker",
    extractor_version: "0.1.0",
    raw_excerpt: "BRANCH-01,EMP-001,2026-11-22,09:00,17:00",
    captured_at: "2026-11-22T20:30:00Z",
    status: "received",
  },
  expect: "holds",
})

export const SourceBatchBlankDigestViolated = Norn.example("source batch missing document_digest is refused", {
  of: SourceBatchContentAddressed,
  given: {
    id: "batch_bad_1",
    channel: "spreadsheet_csv",
    document_uri: "evidence://imports/attendance-2026-11.csv",
    document_digest: "",
    media_type: "text/csv",
    extractor: "norn-hr-import-worker",
    extractor_version: "0.1.0",
    raw_excerpt: "BRANCH-01,EMP-001,2026-11-22,09:00,17:00",
    captured_at: "2026-11-22T20:30:00Z",
    status: "received",
  },
  expect: "violated",
})

export const FieldEvidenceValidAnchor = Norn.law({
  id: "hr_import.FieldEvidence.valid_anchor.law",
  title: "Field evidence carries valid page, normalized bounding box, and confidence bounds",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.FieldEvidence.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.FieldEvidence.valid_anchor",
    owner: "hr_import",
    severity: "error",
    rationale: "Core 07 norn.SourceEvidence boxes are resolution-independent page fractions in [0, 1] on page >= 1 with optional basis-point confidence in [0, 10000].",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.FieldEvidence",
    scope: {
      root: { kind: "entity", id: "hr_import.FieldEvidence" },
      objectTypes: ["hr_import.FieldEvidence"],
    },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        FieldEvidenceProps,
        (self) =>
          self.page >= 1 &&
          self.bbox_x >= 0 &&
          self.bbox_x <= 1 &&
          self.bbox_y >= 0 &&
          self.bbox_y <= 1 &&
          self.bbox_w >= 0 &&
          self.bbox_w <= 1 &&
          self.bbox_h >= 0 &&
          self.bbox_h <= 1 &&
          self.document_digest !== "" &&
          self.source_anchor !== "" &&
          self.record_digest !== "" &&
          (self.confidence_bp === null ||
            ((self.confidence_bp ?? 0) >= 0 && (self.confidence_bp ?? 0) <= 10000)),
      ),
    },
  },
})

export const FieldEvidenceValidHolds = Norn.example("anchored field evidence with 9500 bp confidence holds", {
  of: FieldEvidenceValidAnchor,
  given: {
    id: "ev_ok_1",
    subject_property: "quantity",
    subject_type: "hr.AttendanceEvent",
    subject_key: "cand_ok_1",
    document_uri: "evidence://imports/attendance-2026-11.csv",
    document_digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    media_type: "text/csv",
    page: 1,
    bbox_x: 0,
    bbox_y: 0,
    bbox_w: 1,
    bbox_h: 0.05,
    text: "row 2: 09:00-17:00",
    extractor: "norn-hr-import-worker",
    extractor_version: "0.1.0",
    confidence_bp: 9500,
    source_anchor: "row:2:col:clock_in",
    record_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  },
  expect: "holds",
})

export const FieldEvidenceOutOfBoundsBboxViolated = Norn.example("field evidence with bbox_x > 1 is refused", {
  of: FieldEvidenceValidAnchor,
  given: {
    id: "ev_bad_bbox",
    subject_property: "quantity",
    subject_type: "hr.AttendanceEvent",
    subject_key: "cand_ok_1",
    document_uri: "evidence://imports/schedule-photo.png",
    document_digest: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
    media_type: "image/png",
    page: 1,
    bbox_x: 1.25,
    bbox_y: 0.1,
    bbox_w: 0.2,
    bbox_h: 0.05,
    text: "09:00-17:00",
    extractor: "norn-hr-import-worker",
    extractor_version: "0.1.0",
    confidence_bp: 9200,
    source_anchor: "page:1:bbox",
    record_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  },
  expect: "violated",
})

export const FieldEvidenceZeroPageViolated = Norn.example("field evidence with page 0 is refused", {
  of: FieldEvidenceValidAnchor,
  given: {
    id: "ev_bad_page",
    subject_property: "quantity",
    subject_type: "hr.AttendanceEvent",
    subject_key: "cand_ok_1",
    document_uri: "evidence://imports/schedule-photo.png",
    document_digest: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
    media_type: "image/png",
    page: 0,
    bbox_x: 0.1,
    bbox_y: 0.1,
    bbox_w: 0.2,
    bbox_h: 0.05,
    text: "09:00-17:00",
    extractor: "norn-hr-import-worker",
    extractor_version: "0.1.0",
    confidence_bp: null,
    source_anchor: "page:0:bbox",
    record_digest: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  },
  expect: "violated",
})

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
      expression: Norn.expression(
        ExtractedCandidateProps,
        (self) => self.confidence_bp >= 0 && self.confidence_bp <= 10000,
      ),
    },
  },
})

export const CandidateConfidenceValidHolds = Norn.example("9250 bp confidence holds", {
  of: CandidateConfidenceBounds,
  given: {
    id: "cand_ok_1",
    target_kind: "attendance_event",
    source_record_key: "spreadsheet_csv/row:2",
    payload_digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    confidence_bp: 9250,
    source_span: "row:2",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    dispute_state: "clean",
    review_receipt_digest: null,
    reviewed_by_user_id: null,
    status: "staged",
  },
  expect: "holds",
})

export const CandidateConfidenceOverflowViolated = Norn.example("confidence above 10000 bp is refused", {
  of: CandidateConfidenceBounds,
  given: {
    id: "cand_bad_conf",
    target_kind: "attendance_event",
    source_record_key: "spreadsheet_csv/row:3",
    payload_digest: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
    confidence_bp: 10500,
    source_span: "row:3",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    dispute_state: "clean",
    review_receipt_digest: null,
    reviewed_by_user_id: null,
    status: "staged",
  },
  expect: "violated",
})

export const CandidateBitemporalOrder = Norn.law({
  id: "hr_import.ExtractedCandidate.bitemporal_order.law",
  title: "Extracted candidate observation time never post-dates extraction time",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.ExtractedCandidate.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.ExtractedCandidate.bitemporal_order",
    owner: "hr_import",
    severity: "error",
    rationale: "A candidate cannot be extracted before the source observation existed (observed_at <= extracted_at).",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.ExtractedCandidate",
    scope: {
      root: { kind: "entity", id: "hr_import.ExtractedCandidate" },
      objectTypes: ["hr_import.ExtractedCandidate"],
    },
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        ExtractedCandidateProps,
        (self) => Norn.parseTimestamp(self.observed_at) <= Norn.parseTimestamp(self.extracted_at),
      ),
    },
  },
})

export const CandidateBitemporalValidHolds = Norn.example("observed_at before extracted_at holds", {
  of: CandidateBitemporalOrder,
  given: {
    id: "cand_bt_ok",
    target_kind: "attendance_event",
    source_record_key: "spreadsheet_csv/row:2",
    payload_digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    confidence_bp: 9500,
    source_span: "row:2",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    dispute_state: "clean",
    review_receipt_digest: null,
    reviewed_by_user_id: null,
    status: "staged",
  },
  expect: "holds",
})

export const CandidateBitemporalReversedViolated = Norn.example("observed_at after extracted_at is refused", {
  of: CandidateBitemporalOrder,
  given: {
    id: "cand_bt_bad",
    target_kind: "attendance_event",
    source_record_key: "spreadsheet_csv/row:2",
    payload_digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    confidence_bp: 9500,
    source_span: "row:2",
    extracted_at: "2026-11-22T10:15:00Z",
    observed_at: "2026-11-23T20:30:00Z",
    effective_on: "2026-11-22",
    dispute_state: "clean",
    review_receipt_digest: null,
    reviewed_by_user_id: null,
    status: "staged",
  },
  expect: "violated",
})

export const PromotedCandidateRequiresCleanReview = Norn.law({
  id: "hr_import.ExtractedCandidate.promoted_requires_clean_review.law",
  title: "Promoted candidates must be clean with an authenticated review receipt and reviewer",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.ExtractedCandidate.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.ExtractedCandidate.promoted_requires_clean_review",
    owner: "hr_import",
    severity: "error",
    rationale: "Untrusted extracted candidates cannot be promoted into authoritative HR records while quarantined or disputed, or without an authenticated review receipt and reviewer identity.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.ExtractedCandidate",
    scope: {
      root: { kind: "entity", id: "hr_import.ExtractedCandidate" },
      objectTypes: ["hr_import.ExtractedCandidate"],
    },
    condition: Norn.expression(ExtractedCandidateProps, (self) => self.status === "promoted"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        ExtractedCandidateProps,
        (self) =>
          self.dispute_state === "clean" &&
          self.review_receipt_digest !== null &&
          self.reviewed_by_user_id !== null,
      ),
    },
  },
})

export const StagedCandidateNotApplicable = Norn.example("staged disputed candidate is not yet promoted", {
  of: PromotedCandidateRequiresCleanReview,
  given: {
    id: "cand_staged",
    target_kind: "attendance_event",
    source_record_key: "spreadsheet_csv/row:4",
    payload_digest: "sha256:3333333333333333333333333333333333333333333333333333333333333333",
    confidence_bp: 7400,
    source_span: "row:4",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    dispute_state: "disputed",
    review_receipt_digest: null,
    reviewed_by_user_id: null,
    status: "staged",
  },
  expect: "not_applicable",
})

export const PromotedCandidateCleanHolds = Norn.example("promoted clean candidate with receipt and reviewer holds", {
  of: PromotedCandidateRequiresCleanReview,
  given: {
    id: "cand_promoted",
    target_kind: "shift_commitment",
    source_record_key: "whatsapp_export/line:4-5",
    payload_digest: "sha256:4444444444444444444444444444444444444444444444444444444444444444",
    confidence_bp: 9600,
    source_span: "line:4-5",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T18:05:00Z",
    effective_on: "2026-11-24",
    dispute_state: "clean",
    review_receipt_digest: "sha256:rev001",
    reviewed_by_user_id: "principal:hr_manager",
    status: "promoted",
  },
  expect: "holds",
})

export const PromotedCandidateDisputedViolated = Norn.example("promoted disputed candidate is refused", {
  of: PromotedCandidateRequiresCleanReview,
  given: {
    id: "cand_bad_promo",
    target_kind: "attendance_event",
    source_record_key: "spreadsheet_csv/row:4",
    payload_digest: "sha256:5555555555555555555555555555555555555555555555555555555555555555",
    confidence_bp: 8100,
    source_span: "row:4",
    extracted_at: "2026-11-23T10:15:00Z",
    observed_at: "2026-11-22T20:30:00Z",
    effective_on: "2026-11-22",
    dispute_state: "disputed",
    review_receipt_digest: "sha256:rev002",
    reviewed_by_user_id: "principal:hr_manager",
    status: "promoted",
  },
  expect: "violated",
})

export const FindingTerminalRequiresAuditEvidence = Norn.law({
  id: "hr_import.DataQualityFinding.terminal_requires_audit_evidence.law",
  title: "Terminal data quality findings require explicit resolution action, receipt, reviewer, and reason",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.DataQualityFinding.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.DataQualityFinding.terminal_requires_audit_evidence",
    owner: "hr_import",
    severity: "error",
    rationale: "Findings are never silently waived; every resolved, superseded, or rejected finding must record its Core 07 resolution action, receipt digest, reviewer identity, and reason.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.DataQualityFinding",
    scope: {
      root: { kind: "entity", id: "hr_import.DataQualityFinding" },
      objectTypes: ["hr_import.DataQualityFinding"],
    },
    condition: Norn.expression(DataQualityFindingSubject, (self) => self.status !== "open"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        DataQualityFindingSubject,
        (self) =>
          self.resolution_action !== null &&
          self.resolution_receipt_digest !== null &&
          self.resolved_by_user_id !== null &&
          self.resolution_reason !== null,
      ),
    },
  },
})

export const FindingOpenNotApplicable = Norn.example("open finding does not require resolution fields yet", {
  of: FindingTerminalRequiresAuditEvidence,
  given: {
    id: "dqf_open_1",
    code: "missing_clock_out",
    finding_kind: "schema_violation",
    severity: "error",
    quarantine_disposition: "quarantined",
    field_path: "clock_out_at",
    law_pin: null,
    conflicting_payload_digest: null,
    message: "Missing clock-out timestamp; quarantined pending review.",
    resolution_action: null,
    resolution_receipt_digest: null,
    resolved_by_user_id: null,
    resolution_reason: null,
    status: "open",
  },
  expect: "not_applicable",
})

export const FindingResolvedWithAuditHolds = Norn.example("resolved finding with receipt and reason holds", {
  of: FindingTerminalRequiresAuditEvidence,
  given: {
    id: "dqf_res_1",
    code: "missing_clock_out",
    finding_kind: "schema_violation",
    severity: "error",
    quarantine_disposition: "quarantined",
    field_path: "clock_out_at",
    law_pin: null,
    conflicting_payload_digest: null,
    message: "Missing clock-out timestamp; quarantined pending review.",
    resolution_action: "accept_second_as_correction",
    resolution_receipt_digest: "sha256:res001",
    resolved_by_user_id: "principal:hr_manager",
    resolution_reason: "Superseded by supervisor-verified clock-out export.",
    status: "superseded",
  },
  expect: "holds",
})

export const FindingResolvedWithoutReceiptViolated = Norn.example("resolved finding without receipt digest is refused", {
  of: FindingTerminalRequiresAuditEvidence,
  given: {
    id: "dqf_bad_res",
    code: "missing_clock_out",
    finding_kind: "schema_violation",
    severity: "error",
    quarantine_disposition: "quarantined",
    field_path: "clock_out_at",
    law_pin: null,
    conflicting_payload_digest: null,
    message: "Missing clock-out timestamp; quarantined pending review.",
    resolution_action: "revalidated_correction",
    resolution_receipt_digest: null,
    resolved_by_user_id: "principal:hr_manager",
    resolution_reason: "Fixed offline",
    status: "resolved",
  },
  expect: "violated",
})

export const FeedConflictIsDisputed = Norn.law({
  id: "hr_import.DataQualityFinding.feed_conflict_is_disputed.law",
  title: "FEED-CONFLICT findings must be error-severity, disputed, and carry the conflicting digest",
  version: "0.1.0",
  dependencies: [
    { artifactId: "hr_import.DataQualityFinding.entity", kind: "entity", version: "0.1.0", required: true },
  ],
  law: {
    lawVersion: "0.1",
    id: "hr_import.DataQualityFinding.feed_conflict_is_disputed",
    owner: "hr_import",
    severity: "error",
    rationale: "Core 07 FEED-CONFLICT requires same-key differing-payload observations to be marked error-severity disputed with the conflicting payload digest retained.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/hr-import/hr_import.ts" },
    subject: "hr_import.DataQualityFinding",
    scope: {
      root: { kind: "entity", id: "hr_import.DataQualityFinding" },
      objectTypes: ["hr_import.DataQualityFinding"],
    },
    condition: Norn.expression(DataQualityFindingSubject, (self) => self.finding_kind === "feed_conflict"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(
        DataQualityFindingSubject,
        (self) =>
          self.severity === "error" &&
          self.quarantine_disposition === "disputed" &&
          self.conflicting_payload_digest !== null,
      ),
    },
  },
})

export const NonConflictFindingNotApplicable = Norn.example("schema_violation finding is outside FEED-CONFLICT law", {
  of: FeedConflictIsDisputed,
  given: {
    id: "dqf_schema_1",
    code: "missing_clock_out",
    finding_kind: "schema_violation",
    severity: "error",
    quarantine_disposition: "quarantined",
    field_path: "clock_out_at",
    law_pin: null,
    conflicting_payload_digest: null,
    message: "Missing clock-out timestamp.",
    resolution_action: null,
    resolution_receipt_digest: null,
    resolved_by_user_id: null,
    resolution_reason: null,
    status: "open",
  },
  expect: "not_applicable",
})

export const FeedConflictDisputedHolds = Norn.example("error disputed FEED-CONFLICT with conflicting digest holds", {
  of: FeedConflictIsDisputed,
  given: {
    id: "dqf_fc_ok",
    code: "feed_conflict",
    finding_kind: "feed_conflict",
    severity: "error",
    quarantine_disposition: "disputed",
    field_path: "payload_digest",
    law_pin: "core.boundary.import_finding",
    conflicting_payload_digest: "sha256:9999999999999999999999999999999999999999999999999999999999999999",
    message: "Duplicate source_record_key arrived with conflicting payload_digest.",
    resolution_action: null,
    resolution_receipt_digest: null,
    resolved_by_user_id: null,
    resolution_reason: null,
    status: "open",
  },
  expect: "holds",
})

export const FeedConflictAdvisoryViolated = Norn.example("warning advisory FEED-CONFLICT without digest is refused", {
  of: FeedConflictIsDisputed,
  given: {
    id: "dqf_fc_bad",
    code: "feed_conflict",
    finding_kind: "feed_conflict",
    severity: "warning",
    quarantine_disposition: "advisory",
    field_path: "payload_digest",
    law_pin: "core.boundary.import_finding",
    conflicting_payload_digest: null,
    message: "Silently downgraded conflict.",
    resolution_action: null,
    resolution_receipt_digest: null,
    resolved_by_user_id: null,
    resolution_reason: null,
    status: "open",
  },
  expect: "violated",
})

// ---------------------------------------------------------------- Governed Candidate & Finding Actions (SliceKey::Link bound)

const CandidateBatchInput = Schema.Struct({
  candidateId: Schema.String,
  batchId: Schema.String,
})

const CandidatePromoteInput = Schema.Struct({
  candidateId: Schema.String,
  batchId: Schema.String,
  evidenceId: Schema.String,
})

const CandidateSupersedeInput = Schema.Struct({
  candidateId: Schema.String,
  replacementCandidateId: Schema.String,
  batchId: Schema.String,
})

const CandidateTransitionOutput = Schema.Struct({
  candidateId: Schema.String,
  status: Schema.String,
})

const CandidateBatchLinkSlice = Schema.Struct({
  slice: Schema.Struct({
    batch: Schema.Struct({ id: Schema.String }),
    candidate_batch: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

const QuarantineStateSlice = Schema.Struct({
  slice: Schema.Struct({
    candidate: Schema.Struct({ status: CandidateStatus }),
  }),
})

const PromoteEvidenceLinkSlice = Schema.Struct({
  slice: Schema.Struct({
    candidate: Schema.Struct({
      id: Schema.String,
      dispute_state: CandidateDisputeState,
      review_receipt_digest: Schema.NullOr(Schema.String),
      reviewed_by_user_id: Schema.NullOr(Schema.String),
    }),
    batch: Schema.Struct({
      id: Schema.String,
      document_digest: Schema.String,
    }),
    evidence: Schema.Struct({
      id: Schema.String,
      document_digest: Schema.String,
    }),
    candidate_batch: Schema.Array(Schema.Struct({ id: Schema.String })),
    evidence_candidate: Schema.Array(Schema.Struct({ id: Schema.String })),
    evidence_batch: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

const SupersedeCandidateSlice = Schema.Struct({
  input: Schema.Struct({
    candidateId: Schema.String,
    replacementCandidateId: Schema.String,
  }),
  slice: Schema.Struct({
    candidate: Schema.Struct({ id: Schema.String }),
    replacement: Schema.Struct({
      id: Schema.String,
      dispute_state: CandidateDisputeState,
    }),
    batch: Schema.Struct({ id: Schema.String }),
    candidate_batch: Schema.Array(Schema.Struct({ id: Schema.String })),
    replacement_supersedes: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

export const QuarantineCandidate = Norn.action({
  id: "hr_import.candidate.quarantine",
  title: "Quarantine extracted candidate",
  version: "0.1.0",
  description: "Move a staged extracted candidate bound to its source batch into quarantine when data quality or FEED-CONFLICT findings require human resolution.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "batch", type: SourceBatch, key: { property: { from: "input", property: "batchId" } } },
    { name: "candidate_batch", type: SourceBatch, key: { link: { from: "candidate", link: "batch" } } },
  ],
  preconditions: [
    Norn.expression(
      CandidateBatchLinkSlice,
      (ctx) => ctx.slice.candidate_batch.map((b) => b.id).includes(ctx.slice.batch.id),
      { root: "context" },
    ),
    Norn.expression(
      QuarantineStateSlice,
      (ctx) => ctx.slice.candidate.status === "staged",
      { root: "context" },
    ),
  ],
  input: CandidateBatchInput,
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
  description: "Promote a clean reviewed candidate anchored to verified norn.SourceEvidence matching its SourceBatch document digest.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "batch", type: SourceBatch, key: { property: { from: "input", property: "batchId" } } },
    { name: "evidence", type: FieldEvidence, key: { property: { from: "input", property: "evidenceId" } } },
    { name: "candidate_batch", type: SourceBatch, key: { link: { from: "candidate", link: "batch" } } },
    { name: "evidence_candidate", type: ExtractedCandidate, key: { link: { from: "evidence", link: "candidate" } } },
    { name: "evidence_batch", type: SourceBatch, key: { link: { from: "evidence", link: "batch" } } },
  ],
  preconditions: [
    Norn.expression(
      PromoteEvidenceLinkSlice,
      (ctx) =>
        ctx.slice.candidate_batch.map((b) => b.id).includes(ctx.slice.batch.id) &&
        ctx.slice.evidence_candidate.map((c) => c.id).includes(ctx.slice.candidate.id) &&
        ctx.slice.evidence_batch.map((b) => b.id).includes(ctx.slice.batch.id),
      { root: "context" },
    ),
    Norn.expression(
      PromoteEvidenceLinkSlice,
      (ctx) =>
        ctx.slice.candidate.dispute_state === "clean" &&
        ctx.slice.candidate.review_receipt_digest !== null &&
        ctx.slice.candidate.reviewed_by_user_id !== null &&
        ctx.slice.evidence.document_digest === ctx.slice.batch.document_digest,
      { root: "context" },
    ),
  ],
  input: CandidatePromoteInput,
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
  description: "Permanently reject a nonconforming extracted candidate bound to its source batch while preserving all source evidence and findings.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "batch", type: SourceBatch, key: { property: { from: "input", property: "batchId" } } },
    { name: "candidate_batch", type: SourceBatch, key: { link: { from: "candidate", link: "batch" } } },
  ],
  preconditions: [
    Norn.expression(
      CandidateBatchLinkSlice,
      (ctx) => ctx.slice.candidate_batch.map((b) => b.id).includes(ctx.slice.batch.id),
      { root: "context" },
    ),
  ],
  input: CandidateBatchInput,
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

export const SupersedeCandidate = Norn.action({
  id: "hr_import.candidate.supersede",
  title: "Supersede extracted candidate",
  version: "0.1.0",
  description: "Mark a staged or quarantined candidate immutably superseded once a clean replacement candidate explicitly links to it via `supersedes`.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "replacement", type: ExtractedCandidate, key: { property: { from: "input", property: "replacementCandidateId" } } },
    { name: "batch", type: SourceBatch, key: { property: { from: "input", property: "batchId" } } },
    { name: "candidate_batch", type: SourceBatch, key: { link: { from: "candidate", link: "batch" } } },
    { name: "replacement_supersedes", type: ExtractedCandidate, key: { link: { from: "replacement", link: "supersedes" } } },
  ],
  preconditions: [
    Norn.expression(
      SupersedeCandidateSlice,
      (ctx) =>
        ctx.input.candidateId !== ctx.input.replacementCandidateId &&
        ctx.slice.candidate_batch.map((b) => b.id).includes(ctx.slice.batch.id) &&
        ctx.slice.replacement_supersedes.map((c) => c.id).includes(ctx.slice.candidate.id) &&
        ctx.slice.replacement.dispute_state === "clean",
      { root: "context" },
    ),
  ],
  input: CandidateSupersedeInput,
  output: CandidateTransitionOutput,
  writes: [{ type: ExtractedCandidate, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: ExtractedCandidate,
    objectIdField: "candidateId",
    lifecycle: "status",
    transition: "supersede",
  },
})

const FindingCandidateInput = Schema.Struct({
  findingId: Schema.String,
  candidateId: Schema.String,
})

const FindingSupersedeInput = Schema.Struct({
  findingId: Schema.String,
  candidateId: Schema.String,
  replacementCandidateId: Schema.String,
})

const FindingTransitionOutput = Schema.Struct({
  findingId: Schema.String,
  status: Schema.String,
})

const FindingCandidateSlice = Schema.Struct({
  slice: Schema.Struct({
    finding: Schema.Struct({
      status: FindingStatus,
      resolution_action: Schema.NullOr(ResolutionAction),
      resolution_receipt_digest: Schema.NullOr(Schema.String),
      resolved_by_user_id: Schema.NullOr(Schema.String),
      resolution_reason: Schema.NullOr(Schema.String),
    }),
    candidate: Schema.Struct({ id: Schema.String }),
    finding_candidate: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

const FindingSupersedeSlice = Schema.Struct({
  slice: Schema.Struct({
    finding: Schema.Struct({
      status: FindingStatus,
      resolution_action: Schema.NullOr(ResolutionAction),
      resolution_receipt_digest: Schema.NullOr(Schema.String),
      resolved_by_user_id: Schema.NullOr(Schema.String),
      resolution_reason: Schema.NullOr(Schema.String),
    }),
    candidate: Schema.Struct({ id: Schema.String }),
    replacement: Schema.Struct({ id: Schema.String }),
    finding_candidate: Schema.Array(Schema.Struct({ id: Schema.String })),
    finding_superseded_by: Schema.Array(Schema.Struct({ id: Schema.String })),
  }),
})

export const ResolveFinding = Norn.action({
  id: "hr_import.finding.resolve",
  title: "Resolve data quality finding",
  version: "0.1.0",
  description: "Immutably resolve an open finding bound to its candidate after recording an audited resolution action, receipt digest, reviewer, and reason.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "finding", type: DataQualityFinding, key: { property: { from: "input", property: "findingId" } } },
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "finding_candidate", type: ExtractedCandidate, key: { link: { from: "finding", link: "candidate" } } },
  ],
  preconditions: [
    Norn.expression(
      FindingCandidateSlice,
      (ctx) => ctx.slice.finding_candidate.map((c) => c.id).includes(ctx.slice.candidate.id),
      { root: "context" },
    ),
    Norn.expression(
      FindingCandidateSlice,
      (ctx) =>
        ctx.slice.finding.status === "open" &&
        ctx.slice.finding.resolution_action !== null &&
        ctx.slice.finding.resolution_receipt_digest !== null &&
        ctx.slice.finding.resolved_by_user_id !== null &&
        ctx.slice.finding.resolution_reason !== null,
      { root: "context" },
    ),
  ],
  input: FindingCandidateInput,
  output: FindingTransitionOutput,
  writes: [{ type: DataQualityFinding, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: DataQualityFinding,
    objectIdField: "findingId",
    lifecycle: "status",
    transition: "resolve",
  },
})

export const SupersedeFinding = Norn.action({
  id: "hr_import.finding.supersede",
  title: "Supersede data quality finding",
  version: "0.1.0",
  description: "Immutably supersede an open finding bound to its candidate and superseding correction candidate (`accept_second_as_correction`).",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "finding", type: DataQualityFinding, key: { property: { from: "input", property: "findingId" } } },
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "replacement", type: ExtractedCandidate, key: { property: { from: "input", property: "replacementCandidateId" } } },
    { name: "finding_candidate", type: ExtractedCandidate, key: { link: { from: "finding", link: "candidate" } } },
    { name: "finding_superseded_by", type: ExtractedCandidate, key: { link: { from: "finding", link: "superseded_by" } } },
  ],
  preconditions: [
    Norn.expression(
      FindingSupersedeSlice,
      (ctx) =>
        ctx.slice.finding_candidate.map((c) => c.id).includes(ctx.slice.candidate.id) &&
        ctx.slice.finding_superseded_by.map((c) => c.id).includes(ctx.slice.replacement.id),
      { root: "context" },
    ),
    Norn.expression(
      FindingSupersedeSlice,
      (ctx) =>
        ctx.slice.finding.status === "open" &&
        (ctx.slice.finding.resolution_action ?? "") === "accept_second_as_correction" &&
        ctx.slice.finding.resolution_receipt_digest !== null &&
        ctx.slice.finding.resolved_by_user_id !== null &&
        ctx.slice.finding.resolution_reason !== null,
      { root: "context" },
    ),
  ],
  input: FindingSupersedeInput,
  output: FindingTransitionOutput,
  writes: [{ type: DataQualityFinding, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: DataQualityFinding,
    objectIdField: "findingId",
    lifecycle: "status",
    transition: "supersede",
  },
})

export const RejectFinding = Norn.action({
  id: "hr_import.finding.reject",
  title: "Reject candidate finding",
  version: "0.1.0",
  description: "Immutably close an open finding bound to a rejected candidate (`reject_candidate`) with audited receipt and reason.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  access: { artifactId: "hr_import.access.policy", version: "0.1.0" },
  slice: [
    { name: "finding", type: DataQualityFinding, key: { property: { from: "input", property: "findingId" } } },
    { name: "candidate", type: ExtractedCandidate, key: { property: { from: "input", property: "candidateId" } } },
    { name: "finding_candidate", type: ExtractedCandidate, key: { link: { from: "finding", link: "candidate" } } },
  ],
  preconditions: [
    Norn.expression(
      FindingCandidateSlice,
      (ctx) => ctx.slice.finding_candidate.map((c) => c.id).includes(ctx.slice.candidate.id),
      { root: "context" },
    ),
    Norn.expression(
      FindingCandidateSlice,
      (ctx) =>
        ctx.slice.finding.status === "open" &&
        (ctx.slice.finding.resolution_action ?? "") === "reject_candidate" &&
        ctx.slice.finding.resolution_receipt_digest !== null &&
        ctx.slice.finding.resolved_by_user_id !== null &&
        ctx.slice.finding.resolution_reason !== null,
      { root: "context" },
    ),
  ],
  input: FindingCandidateInput,
  output: FindingTransitionOutput,
  writes: [{ type: DataQualityFinding, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: DataQualityFinding,
    objectIdField: "findingId",
    lifecycle: "status",
    transition: "reject",
  },
})
