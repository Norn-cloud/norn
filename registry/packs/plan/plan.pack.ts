import { Norn } from "@norn/authoring"

const PLAN_SOURCE_FILE = "registry/packs/plan/plan.ts"
const ACTIONS_SOURCE_FILE = "registry/packs/plan/actions.ts"
const REVIEWED_AT = "2026-11-28T00:00:00Z"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.plan",
  title: "Merchandise Planning and Open-to-Buy ontology pack",
  version: "0.1.0",
  sources: [
    {
      sourceId: "source.plan",
      releaseId: "0.1.0@328ba87e23b74f1da49fd9701af266466376aa604e45736ce3a10fd72e12b8c4",
      releasedAt: "2026-11-28",
      retrievedAt: "2026-11-28",
      canonicalUrl: "https://norn.cloud/registry/packs/plan/0.1.0",
      license: "Apache-2.0",
      redistribution: "vendored",
      importMode: "map_only",
      files: [
        {
          path: PLAN_SOURCE_FILE,
          sha256: "328ba87e23b74f1da49fd9701af266466376aa604e45736ce3a10fd72e12b8c4",
        },
        {
          path: ACTIONS_SOURCE_FILE,
          sha256: "2b41dc0bc0de17caf6d3d68dc332cd5956b23665efd085e5ef7e1e17393663ca",
        },
      ],
    },
  ],
  terms: [
    { termId: "norn.plan.version", label: "Plan version", definition: "A versioned, bitemporal merchandise plan container with lifecycle draft -> submitted -> approved -> locked -> superseded, immutable once locked.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.spread.target", label: "Plan spread target", definition: "A top-down parent merchandise target at entity x category x brand x ISO month grain enforcing exact child spread conservation and explicit rounding remainder rules.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.line", label: "Plan line", definition: "An atomic merchandise plan line at entity x store x category x brand x ISO month grain with additive flow targets and semi-additive EOM stock, disjoint from actuals.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.line.planned.receipts.cost", label: "Planned receipts at cost (PlanLine)", definition: "The additive purchase-at-cost budget target for one merchandise plan grain.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.plan.line" },
    { termId: "norn.plan.line.planned.eom.stock.cost", label: "Planned EOM stock at cost (PlanLine)", definition: "The semi-additive end-of-month stock balance target; never summed across months.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.plan.line" },
    { termId: "norn.plan.otb.envelope", label: "OTB envelope", definition: "An approved purchase-at-cost budget ceiling (planned or unplanned with override approval); an internal planning target, never a core.Commitment.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.otb.po.binding", label: "OTB PO binding", definition: "A governed binding between an open supply.OrderLine commitment and an active OTB envelope conserving recognized receipts, unreceived authorized PO value, and cancelled releases.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.published.forecast", label: "Published plan forecast", definition: "A plan-grain forecast revision pinned to a production ml.ModelVersion and explicit forecast_revision for Forecast - Plan variance questions.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.guard.otb.exceeded", label: "OTB budget ceiling guard", definition: "Authorizing a purchase order line would push total consumed OTB above the envelope's approved purchase-at-cost budget or target an unapproved/absent envelope.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.plan.guard.spread.drift", label: "Top-down spread conservation guard", definition: "Child plan lines under a top-down spread target do not sum exactly to the parent target and explicit decimal rounding remainder.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
  ],
  mappings: [
    { mappingId: "map.norn.plan.version", termId: "norn.plan.version", sourceId: "source.plan", externalIri: "https://norn.cloud/registry/packs/plan/0.1.0#plan.PlanVersion", relation: "close_match", justification: "Declares `plan.PlanVersion` in `registry/packs/plan/plan.ts`.", evidenceRefs: [PLAN_SOURCE_FILE], confidenceBasisPoints: 10000, reviewedBy: "lane-a (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.plan.spread.target", termId: "norn.plan.spread.target", sourceId: "source.plan", externalIri: "https://norn.cloud/registry/packs/plan/0.1.0#plan.PlanSpreadTarget", relation: "close_match", justification: "Declares `plan.PlanSpreadTarget` in `registry/packs/plan/plan.ts`.", evidenceRefs: [PLAN_SOURCE_FILE], confidenceBasisPoints: 10000, reviewedBy: "lane-a (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.plan.line", termId: "norn.plan.line", sourceId: "source.plan", externalIri: "https://norn.cloud/registry/packs/plan/0.1.0#plan.PlanLine", relation: "close_match", justification: "Declares `plan.PlanLine` in `registry/packs/plan/plan.ts`.", evidenceRefs: [PLAN_SOURCE_FILE], confidenceBasisPoints: 10000, reviewedBy: "lane-a (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.plan.otb.envelope", termId: "norn.plan.otb.envelope", sourceId: "source.plan", externalIri: "https://norn.cloud/registry/packs/plan/0.1.0#plan.OtbEnvelope", relation: "close_match", justification: "Declares `plan.OtbEnvelope` in `registry/packs/plan/plan.ts`.", evidenceRefs: [PLAN_SOURCE_FILE], confidenceBasisPoints: 10000, reviewedBy: "lane-a (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.plan.otb.po.binding", termId: "norn.plan.otb.po.binding", sourceId: "source.plan", externalIri: "https://norn.cloud/registry/packs/plan/0.1.0#plan.OtbPoBinding", relation: "close_match", justification: "Declares `plan.OtbPoBinding` in `registry/packs/plan/plan.ts`.", evidenceRefs: [PLAN_SOURCE_FILE], confidenceBasisPoints: 10000, reviewedBy: "lane-a (agent, proposed)", reviewedAt: REVIEWED_AT },
    { mappingId: "map.norn.plan.published.forecast", termId: "norn.plan.published.forecast", sourceId: "source.plan", externalIri: "https://norn.cloud/registry/packs/plan/0.1.0#plan.PublishedForecast", relation: "close_match", justification: "Declares `plan.PublishedForecast` in `registry/packs/plan/plan.ts`.", evidenceRefs: [PLAN_SOURCE_FILE], confidenceBasisPoints: 10000, reviewedBy: "lane-a (agent, proposed)", reviewedAt: REVIEWED_AT },
  ],
  shapes: [
    { shapeId: "shape.norn.plan.version", termId: "norn.plan.version", closed: true, fields: [{ fieldId: "plan_version_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "effective_from", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.plan.spread.target", termId: "norn.plan.spread.target", closed: true, fields: [{ fieldId: "spread_target_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "target_receipts_cost", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.plan.line", termId: "norn.plan.line", closed: true, fields: [{ fieldId: "plan_line_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "planned_receipts_cost", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.plan.otb.envelope", termId: "norn.plan.otb.envelope", closed: true, fields: [{ fieldId: "envelope_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "approved_budget_cost", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.plan.otb.po.binding", termId: "norn.plan.otb.po.binding", closed: true, fields: [{ fieldId: "binding_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "otb_consumed_cost", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.plan.published.forecast", termId: "norn.plan.published.forecast", closed: true, fields: [{ fieldId: "forecast_id", valueType: "string", minCount: 1, maxCount: 1 }, { fieldId: "forecast_sales_retail", valueType: "decimal", minCount: 1, maxCount: 1 }] },
  ],
})
