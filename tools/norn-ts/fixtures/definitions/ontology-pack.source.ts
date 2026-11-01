import { Norn } from "@norn/authoring"

export const Commerce = Norn.ontologyPack({
  id: "norn.ontology-pack.fixture-commerce", title: "Fixture commerce ontology", version: "0.1.0",
  sources: [{
    sourceId: "source.schema-org", releaseId: "30.0", releasedAt: "2026-03-19", retrievedAt: "2026-08-25",
    canonicalUrl: "https://schema.org/version/30.0/", license: "CC-BY-SA-3.0", redistribution: "reference_only",
    importMode: "map_only",
    files: [{ path: "https://schema.org/version/latest/schemaorg-current-https.jsonld", sha256: "0f0c97a4f666b2f8563573fe48453782fd51b87a504523cf0c9aff6a71c3eec4" }],
  }],
  terms: [{
    termId: "norn.fixture.order", label: "Order", definition: "A confirmed request to supply goods or services.",
    termKind: "class", status: "active", semanticAuthority: "semantic_only",
  }],
  mappings: [{
    mappingId: "map.fixture.order.schema-org", termId: "norn.fixture.order", sourceId: "source.schema-org",
    externalIri: "https://schema.org/Order", relation: "close_match", justification: "Both denote a confirmed order.",
    evidenceRefs: ["https://schema.org/Order"], confidenceBasisPoints: 8000,
    reviewedBy: "fixture", reviewedAt: "2026-10-30T00:00:00Z",
  }],
  shapes: [{
    shapeId: "shape.fixture.order", termId: "norn.fixture.order", closed: true,
    fields: [{ fieldId: "orderNumber", valueType: "string", minCount: 1, maxCount: 1 }],
  }],
})
