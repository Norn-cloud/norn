// Our consumer contract for OASIS UBL 2.3 Invoice: hand-written, listing only the fields records.ts reads; pinned by registry/standards/oasis-ubl-2-3.source.json.
// `norn-format-read --check` verifies against the hash-pinned originals that every field exists upstream with this type and is required only where upstream requires it.
import { Norn } from "@norn/authoring"

export const Source = Norn.sourceSchema({
  id: "ubl.invoice.source",
  title: "UBL 2.3 Invoice: fields the UBL mapping reads",
  version: "0.1.0",
  source: {system: "XSD", instance: "source.oasis.ubl", version: "2.3"},
  observedAt: "2026-11-11T00:00:00Z",
  adapter: {name: "norn.formats.pack", version: "0.1.0"},
  rawMetadata: {uri: "urn:norn:xsd-bundle:sha256:5f3504fd2b639fc23eb86b47d0764373ef0c1354186034e7e7567356f7c6a84b", sha256: "5f3504fd2b639fc23eb86b47d0764373ef0c1354186034e7e7567356f7c6a84b"},
  datasets: [{id: "InvoiceType", nativeName: "{urn:oasis:names:specification:ubl:schema:xsd:Invoice-2}InvoiceType", kind: "document"}],
  fields: [
    {dataset: "InvoiceType", name: "ID", ordinal: 0, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}IDType", typeCandidate: {type: "string", origin: "inferred"}, required: true},
    {dataset: "InvoiceType", name: "IssueDate", ordinal: 1, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}IssueDateType", typeCandidate: {type: "date", origin: "inferred"}, required: true},
    {dataset: "InvoiceType", name: "DueDate", ordinal: 2, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}DueDateType", typeCandidate: {type: "date", origin: "inferred"}, required: false},
    {dataset: "InvoiceType", name: "DocumentCurrencyCode", ordinal: 3, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}DocumentCurrencyCodeType", typeCandidate: {type: "string", origin: "inferred"}, required: false},
    {dataset: "InvoiceType", name: "payable_amount", ordinal: 4, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}PayableAmountType", typeCandidate: {type: "json", origin: "inferred"}, required: true},
    {dataset: "InvoiceType", name: "supplier_endpoint", ordinal: 5, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}EndpointIDType", typeCandidate: {type: "string", origin: "inferred"}, required: false},
    {dataset: "InvoiceType", name: "supplier_name", ordinal: 6, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}NameType", typeCandidate: {type: "string", origin: "inferred"}, required: false},
    {dataset: "InvoiceType", name: "customer_endpoint", ordinal: 7, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}EndpointIDType", typeCandidate: {type: "string", origin: "inferred"}, required: false},
    {dataset: "InvoiceType", name: "customer_name", ordinal: 8, nativeType: "{urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2}NameType", typeCandidate: {type: "string", origin: "inferred"}, required: false},
  ],
})
