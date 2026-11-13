import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { SourceEvidence, SourceDocument as ScanProvenance } from "../../core/src/evidence.ts"

/**
 * Scanned source documents (#588, #343): receipts, invoices and expense
 * reports read by an extractor, each extracted value carrying the evidence of
 * where it was read. All documents are deterministic synthetic scans
 * (`demo/documents/generate.py`); see `contract.md`.
 */

const Money = Norn.decimal(2, { currency: "currency" })

export const SourceDocument = Norn.interfaceType({
  name: "docs.SourceDocument", title: "Extracted source document", version: "0.1.0",
  description: "A document whose printed values were read by an extractor. The values are observations of what the document says, not posted books.",
  properties: Schema.Struct({
    id: Schema.String, doc_number: Schema.String, kind: Schema.Literals(["receipt", "invoice", "expense_report"]),
    currency: Schema.Literals(["USD"]), total: Money,
  }),
})

export const SourceDocumentLine = Norn.interfaceType({
  name: "docs.SourceDocumentLine", title: "Extracted document line", version: "0.1.0",
  description: "One printed line of an extracted document.",
  properties: Schema.Struct({ id: Schema.String, line_no: Schema.Int, description: Schema.String, currency: Schema.Literals(["USD"]), amount: Money }),
})

export const BookedEntry = Norn.interfaceType({
  name: "docs.BookedEntry", title: "Booked journal entry", version: "0.1.0", role: "happening",
  description: "The journal entry a document was booked to, as the ledger reports it: one signed amount, positive for a debit.",
  properties: Schema.Struct({ id: Schema.String, entry_number: Schema.String, currency: Schema.Literals(["USD"]), amount: Money }),
})

export const LedgerEntry = Norn.type({
  name: "docs.LedgerEntry", title: "Ledger entry", version: "0.1.0", role: "happening",
  description: "A posted journal entry a scanned document was booked to, as the ledger exports it. It is evidence of the booking, not a posting command.",
  implements: [BookedEntry],
  properties: Schema.Struct({ id: Schema.String, entry_number: Schema.String, account_code: Schema.String, currency: Schema.Literals(["USD"]), amount: Money, posted_on: Norn.date() }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const ScannedDocument = Norn.type({
  name: "docs.ScannedDocument", title: "Scanned document", version: "0.1.0", role: "thing",
  description: "A scanned receipt, invoice or expense report as its extractor read it. `lines_total` is the mapping's sum of the printed line amounts (null when the document prints no lines). A correction is a governed patch that records its reason, its evidence and the verified actor.",
  implements: [SourceDocument, ScanProvenance],
  properties: Schema.Struct({
    id: Schema.String, doc_number: Schema.String, kind: Schema.Literals(["receipt", "invoice", "expense_report"]),
    vendor: Schema.String, issued_on: Schema.NullOr(Norn.date()), currency: Schema.Literals(["USD"]),
    total: Money, lines_total: Schema.NullOr(Money), line_count: Schema.Int,
    document_uri: Schema.String, document_digest: Schema.String, media_type: Schema.String,
    extractor: Schema.String, extractor_version: Schema.String,
    correction_reason: Schema.NullOr(Schema.String), corrected_by: Schema.NullOr(Schema.String), correction_evidence: Schema.NullOr(Schema.String),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "ledger_entry", target: LedgerEntry, cardinality: { to: "one", from: "one" } }],
})

export const DocumentLine = Norn.type({
  name: "docs.DocumentLine", title: "Scanned document line", version: "0.1.0", role: "thing",
  description: "One printed line as its extractor read it. `review` is `extracted` until a governed correction fixes a misread amount; the correction records its reason, its evidence and the verified actor, and restates the document's sum of lines in the same commit.",
  implements: [SourceDocumentLine],
  properties: Schema.Struct({
    id: Schema.String, document_id: Schema.String, line_no: Schema.Int, description: Schema.String, currency: Schema.Literals(["USD"]), amount: Money,
    review: Schema.Literals(["extracted", "corrected"]),
    correction_reason: Schema.NullOr(Schema.String), corrected_by: Schema.NullOr(Schema.String), correction_evidence: Schema.NullOr(Schema.String),
  }),
  lifecycles: [{ property: "review", initial: ["extracted"], terminal: ["corrected"], transitions: [{ id: "correct", from: ["extracted"], to: "corrected" }] }],
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "document", target: ScannedDocument, cardinality: { to: "one", from: "many", required: true } }],
})

export const FieldEvidence = Norn.type({
  name: "docs.FieldEvidence", title: "Extracted field evidence", version: "0.1.0", role: "thing",
  implements: [SourceEvidence],
  properties: Schema.Struct({
    id: Schema.String, subject_property: Schema.String, subject_type: Schema.NullOr(Schema.String), subject_key: Schema.NullOr(Schema.String),
    document_uri: Schema.String, document_digest: Schema.String, media_type: Schema.String, page: Schema.Int,
    bbox_x: Norn.decimal(4), bbox_y: Norn.decimal(4), bbox_w: Norn.decimal(4), bbox_h: Norn.decimal(4),
    text: Schema.String, extractor: Schema.String, extractor_version: Schema.String, confidence_bp: Schema.NullOr(Schema.Int),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "of", target: ScannedDocument, cardinality: { to: "one", from: "many", required: true } }],
})

/** LAW A: caught at the source. A document that prints lines must print their sum as its total. */
export const TotalMatchesLines = Norn.law("total_matches_lines", {
  on: ScannedDocument,
  when: self => self.lines_total !== null,
  must: self => self.total === (self.lines_total ?? self.total),
  because: "A document's printed total must equal the sum of its printed lines; when they differ, a digit was usually misread or a decimal point dropped.",
})

/**
 * LAW B (LAW-LINK-1): a document booked to a journal entry must carry that
 * entry's amount. It reads the one entry the document links to, in the same
 * snapshot; a document with no booked entry is not bound by it.
 */
export const TotalMatchesLedger = Norn.law({
  id: "docs.ScannedDocument.total_matches_ledger.law", title: "A booked document's total equals its journal entry", version: "0.1.0",
  dependencies: [{ artifactId: "docs.ScannedDocument.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "docs.ScannedDocument.total_matches_ledger", owner: "docs",
    severity: "error", rationale: "A document booked to a journal entry must carry that entry's amount; a document that disagrees with its own booking was misread or mis-booked.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/documents/documents.ts" },
    subject: "docs.ScannedDocument",
    scope: { root: { kind: "entity", id: "docs.ScannedDocument" }, objectTypes: ["docs.ScannedDocument"] },
    condition: "self.ledger_entry != null",
    modality: { kind: "always" }, response: { kind: "predicate", expression: "self.total == self.ledger_entry.amount" },
  },
})

export const Source = Norn.sourceSchema({
  id: "docs.scans.source", title: "Synthetic scanned documents, as extracted", version: "0.1.0",
  source: { system: "synthetic-scans", instance: "demo", version: "scans-v1" },
  observedAt: "2026-11-12T00:00:00Z", adapter: { name: "norn-extractor", version: "0.1.0" },
  rawMetadata: { uri: "evidence://documents/scans-contract-v1", sha256: "53cc7b7cd97151d9e4d5bca9275cd5c00c9e8e8b572e43d65a3c2c13784c21a4" },
  datasets: [
    { id: "Scanned Document", nativeName: "Extracted document", kind: "document" },
    { id: "Document Line", nativeName: "Extracted printed line", kind: "child_document" },
    { id: "Field Evidence", nativeName: "Where one value was read", kind: "child_document" },
    { id: "Journal Entry", nativeName: "The posted journal entry a document was booked to", kind: "document" },
  ],
  fields: [
    { dataset: "Scanned Document", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "doc_number", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "kind", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "vendor", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "issued_on", ordinal: 4, nativeType: "Date", typeCandidate: { type: "date", origin: "declared" }, required: false },
    { dataset: "Scanned Document", name: "currency", ordinal: 5, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "total", ordinal: 6, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "line_count", ordinal: 7, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "ledger_ref", ordinal: 8, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "Scanned Document", name: "document_uri", ordinal: 9, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "document_digest", ordinal: 10, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "media_type", ordinal: 11, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "extractor", ordinal: 12, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "extractor_version", ordinal: 13, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "lines", ordinal: 14, nativeType: "Array", typeCandidate: { type: "json", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "evidence", ordinal: 15, nativeType: "Array", typeCandidate: { type: "json", origin: "declared" }, required: true },
    { dataset: "Document Line", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Document Line", name: "parent", ordinal: 1, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: true },
    { dataset: "Document Line", name: "line_no", ordinal: 2, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Document Line", name: "description", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Document Line", name: "amount", ordinal: 4, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "parent", ordinal: 1, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "field", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "document_uri", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "document_digest", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "media_type", ordinal: 5, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "page", ordinal: 6, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "bbox_x", ordinal: 7, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "bbox_y", ordinal: 8, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "bbox_w", ordinal: 9, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "bbox_h", ordinal: 10, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "text", ordinal: 11, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "extractor", ordinal: 12, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "extractor_version", ordinal: 13, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Field Evidence", name: "confidence_bp", ordinal: 14, nativeType: "Integer", typeCandidate: { type: "integer", origin: "declared" }, required: false },
    { dataset: "Field Evidence", name: "row", ordinal: 15, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "Journal Entry", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Journal Entry", name: "entry_number", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Journal Entry", name: "account_code", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Journal Entry", name: "currency", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Journal Entry", name: "amount", ordinal: 4, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Journal Entry", name: "posted_on", ordinal: 5, nativeType: "Date", typeCandidate: { type: "date", origin: "declared" }, required: true },
  ],
  codeSets: [
    { id: "kind", dataset: "Scanned Document", field: "kind", origin: "declared", values: [{ value: "receipt" }, { value: "invoice" }, { value: "expense_report" }] },
    { id: "currency", dataset: "Scanned Document", field: "currency", origin: "declared", values: [{ value: "USD" }] },
    { id: "entry_currency", dataset: "Journal Entry", field: "currency", origin: "declared", values: [{ value: "USD" }] },
  ],
  constraints: [
    { id: "document_identity", kind: "primary_key", dataset: "Scanned Document", fields: ["name"], origin: "declared" },
    { id: "line_identity", kind: "primary_key", dataset: "Document Line", fields: ["name"], origin: "declared" },
    { id: "evidence_identity", kind: "primary_key", dataset: "Field Evidence", fields: ["name"], origin: "declared" },
    { id: "entry_identity", kind: "primary_key", dataset: "Journal Entry", fields: ["name"], origin: "declared" },
    { id: "lines", kind: "parent_child", dataset: "Document Line", fields: ["parent"], target: { dataset: "Scanned Document", fields: ["name"] }, collection: "lines", origin: "declared" },
    { id: "evidence", kind: "parent_child", dataset: "Field Evidence", fields: ["parent"], target: { dataset: "Scanned Document", fields: ["name"] }, collection: "evidence", origin: "declared" },
  ],
})

export const Mapping = Norn.mapping({
  id: "docs.scans.mapping", title: "Extracted documents, lines and field evidence", version: "0.1.0",
  source: { schemaId: "docs.scans.source", version: "0.1.0", sha256: "a25827b9d52152c28fdb79150e256c611c330f30f9f0b4bf6784a0c3e8f1dd9d" },
  sourceContract: { kind: "generic" }, interfaces: [SourceDocument, SourceDocumentLine, SourceEvidence, BookedEntry],
  maps: [
    {
      id: "document", source: { dataset: "Scanned Document" }, target: { type: ScannedDocument, interface: SourceDocument },
      subject: "input.name", sourceRecord: "'synthetic-scans/demo/' + input.name",
      properties: [
        { property: "id", value: "input.name" }, { property: "doc_number", value: "input.doc_number" },
        { property: "kind", value: "input.kind" }, { property: "vendor", value: "input.vendor" },
        { property: "issued_on", value: "input.issued_on" }, { property: "currency", value: "input.currency" },
        { property: "total", value: "input.total" },
        { property: "lines_total", value: "input.line_count == 0 ? null : sum(map(input.lines, #.amount))" },
        { property: "line_count", value: "input.line_count" },
        { property: "document_uri", value: "input.document_uri" }, { property: "document_digest", value: "input.document_digest" },
        { property: "media_type", value: "input.media_type" },
        { property: "extractor", value: "input.extractor" }, { property: "extractor_version", value: "input.extractor_version" },
        { property: "correction_reason", value: "null" }, { property: "corrected_by", value: "null" }, { property: "correction_evidence", value: "null" },
      ],
      links: [{ link: "ledger_entry", targetMap: "ledger_entry", key: "input.ledger_ref" }],
      ignoredFields: [
        { field: "evidence", reason: "Mapped row by row by the evidence map." },
      ],
    },
    {
      id: "ledger_entry", source: { dataset: "Journal Entry" }, target: { type: LedgerEntry, interface: BookedEntry },
      subject: "input.name", sourceRecord: "'synthetic-scans/ledger/' + input.entry_number",
      properties: [
        { property: "id", value: "input.name" }, { property: "entry_number", value: "input.entry_number" },
        { property: "account_code", value: "input.account_code" }, { property: "currency", value: "input.currency" },
        { property: "amount", value: "input.amount" }, { property: "posted_on", value: "input.posted_on" },
      ],
    },
    {
      id: "line", source: { dataset: "Document Line", nestedIn: { dataset: "Scanned Document", field: "lines" } },
      target: { type: DocumentLine, interface: SourceDocumentLine },
      subject: "input.name", sourceRecord: "'synthetic-scans/demo/' + input.name",
      properties: [
        { property: "id", value: "input.name" }, { property: "document_id", value: "input.parent" }, { property: "line_no", value: "input.line_no" },
        { property: "description", value: "input.description" }, { property: "currency", value: "'USD'" },
        { property: "amount", value: "input.amount" },
        { property: "correction_reason", value: "null" }, { property: "corrected_by", value: "null" }, { property: "correction_evidence", value: "null" },
      ],
      lifecycles: [{ property: "review", state: "'extracted'" }],
      links: [{ link: "document", targetMap: "document", key: "input.parent" }],
    },
    {
      id: "evidence", source: { dataset: "Field Evidence", nestedIn: { dataset: "Scanned Document", field: "evidence" } },
      target: { type: FieldEvidence, interface: SourceEvidence },
      subject: "input.name", sourceRecord: "'synthetic-scans/demo/' + input.name",
      properties: [
        // A printed line's value is evidence for that line's amount; the line is its subject.
        { property: "id", value: "input.name" },
        { property: "subject_property", value: "input.row == null ? input.field : 'amount'" },
        { property: "subject_type", value: "input.row == null ? null : 'docs.DocumentLine'" },
        { property: "subject_key", value: "input.row" },
        { property: "document_uri", value: "input.document_uri" }, { property: "document_digest", value: "input.document_digest" },
        { property: "media_type", value: "input.media_type" }, { property: "page", value: "input.page" },
        { property: "bbox_x", value: "input.bbox_x" }, { property: "bbox_y", value: "input.bbox_y" },
        { property: "bbox_w", value: "input.bbox_w" }, { property: "bbox_h", value: "input.bbox_h" },
        { property: "text", value: "input.text" }, { property: "extractor", value: "input.extractor" },
        { property: "extractor_version", value: "input.extractor_version" }, { property: "confidence_bp", value: "input.confidence_bp" },
      ],
      links: [{ link: "of", targetMap: "document", key: "input.parent" }],
    },
  ],
})

const CorrectionInput = Schema.Struct({
  id: Schema.String, currency: Schema.Literals(["USD"]), total: Money,
  correction_reason: Schema.String, correction_evidence: Schema.String,
  corrected_by: Schema.optionalKey(Schema.String),
})

/**
 * The governed correction: a patch of the total that records why, from which
 * evidence object, and who (the verified actor, never caller input). The
 * document's laws run on the corrected values, so a correction that breaks
 * them is refused with the law's pin.
 */
export const CorrectScannedDocument = Norn.action({
  id: "docs.scanned_document.correct", title: "Correct an extracted total", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: CorrectionInput, output: Schema.Struct({ id: Schema.String }),
  inputLabels: {
    id: { title: "Document", description: "The scanned document to correct." },
    total: { title: "Corrected total", description: "The total as printed on the scanned page. It must equal the sum of the printed lines and, for a booked document, its journal entry; a misread line is corrected on that line." },
    correction_reason: { title: "Reason", description: "Why the imported value is wrong, kept with the correction." },
    correction_evidence: { title: "Evidence", description: "The evidence object the correction rests on, for example SCAN-INV-0042~total; copy it from the evidence beside the value." },
  },
  slice: [
    { name: "document", type: ScannedDocument, key: { property: { from: "input", property: "id" } } },
    // The cited evidence is read in the same commit: a correction that cites
    // no evidence object refuses, and a catalog reader learns the field takes one.
    { name: "evidence", type: FieldEvidence, key: { property: { from: "input", property: "correction_evidence" } } },
  ],
  derive: [{ field: "corrected_by", inputRequired: false,
    expression: Norn.expression(Schema.Struct({ actor: Schema.Struct({ subjectId: Schema.String }) }), context => context.actor.subjectId, { root: "context" }) }],
  preconditions: [Norn.expression(Schema.Struct({ input: CorrectionInput }), x => x.input.correction_reason !== "" && x.input.correction_evidence !== "")],
  writes: [{ type: ScannedDocument, change: "modified" }, { type: ScannedDocument, change: "created" }],
  effect: { kind: "upsert_input_object", type: ScannedDocument, objectIdField: "id", writeMode: { kind: "upsert" } },
  access: { artifactId: "docs.scanned_document.access.policy", version: "0.1.0" },
})

const LineCorrectionInput = Schema.Struct({
  id: Schema.String, currency: Schema.Literals(["USD"]), amount: Money, document_total: Schema.optionalKey(Money),
  correction_reason: Schema.String, correction_evidence: Schema.String,
})
// The writer's numbers are the stored decimals (ZEN computes them exactly); the
// commit holds each written amount to its property's scale and currency.
const Amount = Schema.Finite
const WriterInput = Schema.Struct({
  id: Schema.String, currency: Schema.Literals(["USD"]), amount: Amount, document_total: Schema.optionalKey(Amount),
  correction_reason: Schema.String, correction_evidence: Schema.String,
})
// The writer reads whole slice records (a closed schema names every member).
const LineValues = Schema.Struct({
  id: Schema.String, document_id: Schema.String, line_no: Schema.Int, description: Schema.String, currency: Schema.Literals(["USD"]), amount: Amount,
  review: Schema.Literals(["extracted", "corrected"]), correction_reason: Schema.NullOr(Schema.String), corrected_by: Schema.NullOr(Schema.String), correction_evidence: Schema.NullOr(Schema.String),
})
const DocumentValues = Schema.Struct({
  id: Schema.String, doc_number: Schema.String, kind: Schema.Literals(["receipt", "invoice", "expense_report"]),
  vendor: Schema.String, issued_on: Schema.NullOr(Schema.String), currency: Schema.Literals(["USD"]),
  total: Amount, lines_total: Schema.NullOr(Amount), line_count: Schema.Int,
  document_uri: Schema.String, document_digest: Schema.String, media_type: Schema.String,
  extractor: Schema.String, extractor_version: Schema.String, correction_reason: Schema.NullOr(Schema.String), corrected_by: Schema.NullOr(Schema.String), correction_evidence: Schema.NullOr(Schema.String),
})
const EvidenceValues = Schema.Struct({
  id: Schema.String, subject_property: Schema.String, subject_type: Schema.NullOr(Schema.String), subject_key: Schema.NullOr(Schema.String),
  document_uri: Schema.String, document_digest: Schema.String, media_type: Schema.String, page: Schema.Int,
  bbox_x: Amount, bbox_y: Amount, bbox_w: Amount, bbox_h: Amount,
  text: Schema.String, extractor: Schema.String, extractor_version: Schema.String, confidence_bp: Schema.NullOr(Schema.Int),
})
const LineSlice = Schema.Struct({ line: LineValues, document: DocumentValues, evidence: EvidenceValues })
const Patch = Schema.Struct({
  currency: Schema.Literals(["USD"]), amount: Schema.optionalKey(Amount), total: Schema.optionalKey(Amount), lines_total: Schema.optionalKey(Amount),
  correction_reason: Schema.optionalKey(Schema.String), correction_evidence: Schema.optionalKey(Schema.String), corrected_by: Schema.optionalKey(Schema.String),
})

/**
 * The line correction's writer: the line takes the printed amount, and its
 * document's sum of lines moves by the same difference (and its total, when
 * that was misread too), in one commit, so the document's laws judge the
 * corrected whole. Who corrected is the verified actor.
 */
export const LineCorrectionWriter = Norn.decision({
  id: "docs.document_line.writer", title: "Restate a corrected line and its document", version: "0.1.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: LineSlice, event: Schema.Struct({ input: WriterInput, actor: Schema.String, operationId: Schema.String }) }),
  output: Schema.Struct({
    facts: Schema.Array(Schema.Struct({ op: Schema.Literals(["patch"]), objectType: Schema.Literals(["docs.DocumentLine", "docs.ScannedDocument"]), key: Schema.String, patch: Patch })),
    commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})),
  }),
}, input => ({
  facts: [
    { op: "patch", objectType: "docs.DocumentLine", key: input.event.input.id, patch: {
      currency: input.slice.line.currency, amount: input.event.input.amount, correction_reason: input.event.input.correction_reason,
      correction_evidence: input.event.input.correction_evidence, corrected_by: input.event.actor } },
    { op: "patch", objectType: "docs.ScannedDocument", key: input.slice.line.document_id, patch: {
      currency: input.slice.document.currency,
      lines_total: (input.slice.document.lines_total ?? input.slice.line.amount) - input.slice.line.amount + input.event.input.amount,
      total: input.event.input.document_total ?? input.slice.document.total,
      correction_reason: input.event.input.correction_reason, correction_evidence: input.event.input.correction_evidence, corrected_by: input.event.actor } },
  ],
  commands: [], timers: [],
}))

/**
 * The governed correction of one printed line: the line's amount as printed,
 * cited by the evidence read for that line. The document's sum of lines is
 * restated with it; when the document's printed total was misread too, the
 * same correction fixes it, so the laws judge both at once.
 */
export const CorrectDocumentLine = Norn.action({
  id: "docs.document_line.correct", title: "Correct an extracted line", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: LineCorrectionInput, output: Schema.Struct({ id: Schema.String }), synchronousRule: LineCorrectionWriter,
  inputLabels: {
    id: { title: "Line", description: "The printed line to correct." },
    amount: { title: "Corrected amount", description: "The line's amount as printed on the scanned page." },
    document_total: { title: "Corrected document total", description: "Only when the document's printed total was misread too: the total as printed." },
    correction_reason: { title: "Reason", description: "Why the imported value is wrong, kept with the correction." },
    correction_evidence: { title: "Evidence", description: "The evidence object read for this line, for example SCAN-INV-0043~lines.1.amount." },
  },
  slice: [
    { name: "line", type: DocumentLine, key: { property: { from: "input", property: "id" } } },
    { name: "document", type: ScannedDocument, key: { property: { from: "line", property: "document_id" } } },
    { name: "evidence", type: FieldEvidence, key: { property: { from: "input", property: "correction_evidence" } } },
  ],
  preconditions: [Norn.expression(Schema.Struct({ input: LineCorrectionInput, slice: LineSlice }),
    x => x.input.correction_reason !== "" && (x.slice.evidence.subject_key ?? "") === x.input.id)],
  writes: [{ type: DocumentLine, change: "modified" }, { type: ScannedDocument, change: "modified" }],
  effect: { kind: "fire_transition", type: DocumentLine, objectIdField: "id", lifecycle: "review", transition: "correct" },
  access: { artifactId: "docs.scanned_document.access.policy", version: "0.1.0" },
})

/** Who may correct: a data steward, or (on the alpha) any member of the org. */
export const CorrectionAccess = Norn.access({
  id: "docs.scanned_document.access", title: "Scanned document steward", version: "0.1.0",
  rules: [
    { ruleId: "steward", actions: [CorrectScannedDocument], resourceType: ScannedDocument,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["DataSteward", "member", "admin", "owner"] } }] },
    { ruleId: "steward_lines", actions: [CorrectDocumentLine], resourceType: DocumentLine,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["DataSteward", "member", "admin", "owner"] } }] },
  ],
})
