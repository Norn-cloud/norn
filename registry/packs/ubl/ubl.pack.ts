import { Norn } from "@norn/authoring"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.ubl",
  title: "OASIS UBL 2.3 Invoice standard pack (map-only)",
  version: "0.1.0",
  sources: [
    {"sourceId": "source.oasis.ubl", "releaseId": "2.3", "releasedAt": "2021-06-15", "retrievedAt": "2026-11-11", "canonicalUrl": "https://docs.oasis-open.org/ubl/os-UBL-2.3/UBL-2.3.html", "license": "OASIS specification copyright permissions (notices retained in every unmodified source file)", "redistribution": "reference_only", "importMode": "map_only", "importer": {"name": "norn.formats.xsd", "version": "0.1.0"}, "files": [{"path": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/maindoc/UBL-Invoice-2.3.xsd", "sha256": "17be67538966292a3e8e7bd6404c9017b38919184cb7d4c8dc12f0937358e1a4"}, {"path": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/UBL-CommonAggregateComponents-2.3.xsd", "sha256": "9ff2b3cc1ad590f3fd7ee5c43f3556b4f12347c3d1672965b18c72958c054d65"}, {"path": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/UBL-CommonBasicComponents-2.3.xsd", "sha256": "9bc47430a6c550684412269dd65f26351b4c3e00b16d805595af581130c2f03a"}, {"path": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/UBL-QualifiedDataTypes-2.3.xsd", "sha256": "889be90e684097e70d60c8d7d690beeb1d04a5a0c385e6fe49969621f8bab9fd"}, {"path": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/BDNDR-UnqualifiedDataTypes-1.1.xsd", "sha256": "6be0887f677a21d1fa2b363b0abe2385b713a20befee85d1a8fed5ef7d76993c"}, {"path": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/BDNDR-CCTS_CCT_SchemaModule-1.1.xsd", "sha256": "dd546e4809df86b6445589f69f0d6c9df162840ae386574ddfc1da7638103e15"}]},
  ],
  terms: [
    {"termId": "norn.ubl.invoice", "label": "Invoice", "definition": "A UBL document claiming payment for goods or services supplied.", "termKind": "class", "status": "active", "semanticAuthority": "semantic_only"},
    {"termId": "norn.ubl.party", "label": "Party", "definition": "A UBL party: the supplier or customer of a document.", "termKind": "class", "status": "active", "semanticAuthority": "semantic_only"},
    {"termId": "norn.ubl.invoice.line", "label": "InvoiceLine", "definition": "One charged item on a UBL invoice.", "termKind": "class", "status": "active", "semanticAuthority": "semantic_only"},
    {"termId": "norn.ubl.monetary.total", "label": "LegalMonetaryTotal", "definition": "The totals of a UBL invoice, including the payable amount.", "termKind": "class", "status": "active", "semanticAuthority": "semantic_only"},
    {"termId": "norn.ubl.invoice.payable.amount", "label": "PayableAmount", "definition": "The amount due for payment.", "termKind": "property", "status": "active", "semanticAuthority": "semantic_only", "domainTermId": "norn.ubl.monetary.total"},
    {"termId": "norn.ubl.invoice.due.date", "label": "DueDate", "definition": "The date payment is due.", "termKind": "property", "status": "active", "semanticAuthority": "semantic_only", "domainTermId": "norn.ubl.invoice"},
    {"termId": "norn.ubl.invoice.currency", "label": "DocumentCurrencyCode", "definition": "The default currency of the document's amounts.", "termKind": "property", "status": "active", "semanticAuthority": "semantic_only", "domainTermId": "norn.ubl.invoice"},
  ],
  mappings: [
    {"mappingId": "map.norn.ubl.invoice", "termId": "norn.ubl.invoice", "sourceId": "source.oasis.ubl", "externalIri": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/maindoc/UBL-Invoice-2.3.xsd#InvoiceType", "relation": "close_match", "justification": "The term names the Invoice document type of the pinned release.", "evidenceRefs": ["https://docs.oasis-open.org/ubl/os-UBL-2.3/UBL-2.3.html"], "confidenceBasisPoints": 9000, "reviewedBy": "l-atlas (agent, proposed)", "reviewedAt": "2026-11-11T00:00:00Z"},
    {"mappingId": "map.norn.ubl.party", "termId": "norn.ubl.party", "sourceId": "source.oasis.ubl", "externalIri": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/UBL-CommonAggregateComponents-2.3.xsd#PartyType", "relation": "close_match", "justification": "The term names the aggregate Party type of the pinned release.", "evidenceRefs": ["https://docs.oasis-open.org/ubl/os-UBL-2.3/UBL-2.3.html"], "confidenceBasisPoints": 9000, "reviewedBy": "l-atlas (agent, proposed)", "reviewedAt": "2026-11-11T00:00:00Z"},
    {"mappingId": "map.norn.ubl.invoice.line", "termId": "norn.ubl.invoice.line", "sourceId": "source.oasis.ubl", "externalIri": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/UBL-CommonAggregateComponents-2.3.xsd#InvoiceLineType", "relation": "close_match", "justification": "The term names the aggregate InvoiceLine type of the pinned release.", "evidenceRefs": ["https://docs.oasis-open.org/ubl/os-UBL-2.3/UBL-2.3.html"], "confidenceBasisPoints": 9000, "reviewedBy": "l-atlas (agent, proposed)", "reviewedAt": "2026-11-11T00:00:00Z"},
    {"mappingId": "map.norn.ubl.monetary.total", "termId": "norn.ubl.monetary.total", "sourceId": "source.oasis.ubl", "externalIri": "https://docs.oasis-open.org/ubl/os-UBL-2.3/xsd/common/UBL-CommonAggregateComponents-2.3.xsd#MonetaryTotalType", "relation": "close_match", "justification": "The term names the aggregate MonetaryTotal type of the pinned release.", "evidenceRefs": ["https://docs.oasis-open.org/ubl/os-UBL-2.3/UBL-2.3.html"], "confidenceBasisPoints": 9000, "reviewedBy": "l-atlas (agent, proposed)", "reviewedAt": "2026-11-11T00:00:00Z"},
  ],
  shapes: [
    {"shapeId": "shape.norn.ubl.invoice", "termId": "norn.ubl.invoice", "closed": true, "fields": [{"fieldId": "id", "valueType": "string", "minCount": 1, "maxCount": 1}]},
    {"shapeId": "shape.norn.ubl.party", "termId": "norn.ubl.party", "closed": true, "fields": [{"fieldId": "id", "valueType": "string", "minCount": 1, "maxCount": 1}]},
    {"shapeId": "shape.norn.ubl.invoice.line", "termId": "norn.ubl.invoice.line", "closed": true, "fields": [{"fieldId": "id", "valueType": "string", "minCount": 1, "maxCount": 1}]},
    {"shapeId": "shape.norn.ubl.monetary.total", "termId": "norn.ubl.monetary.total", "closed": true, "fields": [{"fieldId": "id", "valueType": "string", "minCount": 1, "maxCount": 1}]},
  ],
})
