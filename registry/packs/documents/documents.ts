import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { SourceEvidence } from "../../core/src/evidence.ts"

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

export const ScannedDocument = Norn.type({
  name: "docs.ScannedDocument", title: "Scanned document", version: "0.1.0", role: "thing",
  description: "A scanned receipt, invoice or expense report as its extractor read it. `lines_total` is the mapping's sum of the printed line amounts (null when the document prints no lines). A correction is a governed patch that records its reason, its evidence and the verified actor.",
  implements: [SourceDocument],
  properties: Schema.Struct({
    id: Schema.String, doc_number: Schema.String, kind: Schema.Literals(["receipt", "invoice", "expense_report"]),
    vendor: Schema.String, issued_on: Norn.date(), currency: Schema.Literals(["USD"]),
    total: Money, lines_total: Schema.NullOr(Money), line_count: Schema.Int,
    document_uri: Schema.String, document_digest: Schema.String, media_type: Schema.String,
    extractor: Schema.String, extractor_version: Schema.String,
    correction_reason: Schema.NullOr(Schema.String), corrected_by: Schema.NullOr(Schema.String), correction_evidence: Schema.NullOr(Schema.String),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const DocumentLine = Norn.type({
  name: "docs.DocumentLine", title: "Scanned document line", version: "0.1.0", role: "thing",
  implements: [SourceDocumentLine],
  properties: Schema.Struct({ id: Schema.String, line_no: Schema.Int, description: Schema.String, currency: Schema.Literals(["USD"]), amount: Money }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "document", target: ScannedDocument, cardinality: { to: "one", from: "many", required: true } }],
})

export const FieldEvidence = Norn.type({
  name: "docs.FieldEvidence", title: "Extracted field evidence", version: "0.1.0", role: "thing",
  implements: [SourceEvidence],
  properties: Schema.Struct({
    id: Schema.String, subject_property: Schema.String,
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
  because: "A document's printed total equals the sum of its printed lines.",
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
  ],
  fields: [
    { dataset: "Scanned Document", name: "name", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "doc_number", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "kind", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "vendor", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Scanned Document", name: "issued_on", ordinal: 4, nativeType: "Date", typeCandidate: { type: "date", origin: "declared" }, required: true },
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
  ],
  codeSets: [
    { id: "kind", dataset: "Scanned Document", field: "kind", origin: "declared", values: [{ value: "receipt" }, { value: "invoice" }, { value: "expense_report" }] },
    { id: "currency", dataset: "Scanned Document", field: "currency", origin: "declared", values: [{ value: "USD" }] },
  ],
  constraints: [
    { id: "document_identity", kind: "primary_key", dataset: "Scanned Document", fields: ["name"], origin: "declared" },
    { id: "line_identity", kind: "primary_key", dataset: "Document Line", fields: ["name"], origin: "declared" },
    { id: "evidence_identity", kind: "primary_key", dataset: "Field Evidence", fields: ["name"], origin: "declared" },
    { id: "lines", kind: "parent_child", dataset: "Document Line", fields: ["parent"], target: { dataset: "Scanned Document", fields: ["name"] }, collection: "lines", origin: "declared" },
    { id: "evidence", kind: "parent_child", dataset: "Field Evidence", fields: ["parent"], target: { dataset: "Scanned Document", fields: ["name"] }, collection: "evidence", origin: "declared" },
  ],
})

export const Mapping = Norn.mapping({
  id: "docs.scans.mapping", title: "Extracted documents, lines and field evidence", version: "0.1.0",
  source: { schemaId: "docs.scans.source", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  sourceContract: { kind: "generic" }, interfaces: [SourceDocument, SourceDocumentLine, SourceEvidence],
  maps: [
    {
      id: "document", source: { dataset: "Scanned Document" }, target: { type: ScannedDocument, interface: SourceDocument },
      subject: "input.name", sourceRecord: "'synthetic-scans/demo/' + input.name",
      properties: [
        { property: "id", value: "input.name" }, { property: "doc_number", value: "input.doc_number" },
        { property: "kind", value: "input.kind" }, { property: "vendor", value: "input.vendor" },
        { property: "issued_on", value: "date(input.issued_on)" }, { property: "currency", value: "input.currency" },
        { property: "total", value: "input.total" },
        { property: "lines_total", value: "input.line_count == 0 ? null : sum(map(input.lines, #.amount))" },
        { property: "line_count", value: "input.line_count" },
        { property: "document_uri", value: "input.document_uri" }, { property: "document_digest", value: "input.document_digest" },
        { property: "media_type", value: "input.media_type" },
        { property: "extractor", value: "input.extractor" }, { property: "extractor_version", value: "input.extractor_version" },
        { property: "correction_reason", value: "null" }, { property: "corrected_by", value: "null" }, { property: "correction_evidence", value: "null" },
      ],
      ignoredFields: [
        { field: "ledger_ref", reason: "The linked ledger entry is checked by LAW B (#343), a later unit; until then the reference stays in the source document only." },
        { field: "evidence", reason: "Mapped row by row by the evidence map." },
      ],
    },
    {
      id: "line", source: { dataset: "Document Line", nestedIn: { dataset: "Scanned Document", field: "lines" } },
      target: { type: DocumentLine, interface: SourceDocumentLine },
      subject: "input.name", sourceRecord: "'synthetic-scans/demo/' + input.name",
      properties: [
        { property: "id", value: "input.name" }, { property: "line_no", value: "input.line_no" },
        { property: "description", value: "input.description" }, { property: "currency", value: "'USD'" },
        { property: "amount", value: "input.amount" },
      ],
      links: [{ link: "document", targetMap: "document", key: "input.parent" }],
    },
    {
      id: "evidence", source: { dataset: "Field Evidence", nestedIn: { dataset: "Scanned Document", field: "evidence" } },
      target: { type: FieldEvidence, interface: SourceEvidence },
      subject: "input.name", sourceRecord: "'synthetic-scans/demo/' + input.name",
      properties: [
        { property: "id", value: "input.name" }, { property: "subject_property", value: "input.field" },
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
  slice: [{ name: "document", type: ScannedDocument, key: { property: { from: "input", property: "id" } } }],
  derive: [{ field: "corrected_by", inputRequired: false,
    expression: Norn.expression(Schema.Struct({ actor: Schema.Struct({ subjectId: Schema.String }) }), context => context.actor.subjectId, { root: "context" }) }],
  preconditions: [Norn.expression(Schema.Struct({ input: CorrectionInput }), x => x.input.correction_reason !== "" && x.input.correction_evidence !== "")],
  writes: [{ type: ScannedDocument, change: "modified" }, { type: ScannedDocument, change: "created" }],
  effect: { kind: "upsert_input_object", type: ScannedDocument, objectIdField: "id", writeMode: { kind: "upsert" } },
})

export const CorrectionAccess = Norn.access({
  id: "docs.scanned_document.access", title: "Scanned document steward", version: "0.1.0",
  rules: [{ ruleId: "steward", actions: [CorrectScannedDocument], resourceType: ScannedDocument,
    scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["DataSteward"] } }],
  }],
})
