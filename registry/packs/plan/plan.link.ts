import { Norn } from "@norn/authoring"

const REVIEWED_AT = "2026-11-28T00:00:00Z"

export const Pack = Norn.packLink({
  id: "norn.link.plan.canon",
  title: "Merchandise Planning and OTB to the Norn canon",
  version: "0.1.0",
  fromPack: { packId: "norn.ontology-pack.plan", version: "0.1.0" },
  toPack: { packId: "norn.ontology-pack.canon", version: "0.1.0" },
  correspondences: [
    {
      correspondenceId: "corr.plan.guard.otb.exceeded",
      fromTermId: "norn.plan.guard.otb.exceeded",
      relation: "subsumed_by",
      toTermIds: ["norn.finance.guard.allocation.exceeds.outstanding"],
      justification: "OTB budget consumption guard prevents committing purchase value above the remaining approved budget envelope.",
      evidenceRefs: ["source.plan"],
      confidenceBasisPoints: 8000,
      reviewedBy: "lane-a (agent, proposed)",
      reviewedAt: REVIEWED_AT,
    },
  ],
  residuals: [
    { termId: "norn.plan.version", reason: "canon_gap", note: "The canon ontology pack has no bitemporal merchandise plan-version container class yet." },
    { termId: "norn.plan.spread.target", reason: "canon_gap", note: "The canon ontology pack has no top-down spread conservation parent target class yet." },
    { termId: "norn.plan.line", reason: "canon_gap", note: "Internal merchandise plan target at entity x store x category x brand x ISO month grain; intentionally disjoint from norn.core.event and norn.core.commitment." },
    { termId: "norn.plan.line.planned.receipts.cost", reason: "canon_gap", note: "Internal purchase-at-cost budget target; not a financial claim or commitment amount." },
    { termId: "norn.plan.line.planned.eom.stock.cost", reason: "canon_gap", note: "Semi-additive planned end-of-month stock valuation target; the canon pack has no planned snapshot stock property yet." },
    { termId: "norn.plan.otb.envelope", reason: "canon_gap", note: "Internal OTB purchase-at-cost budget ceiling; explicitly NOT norn.core.commitment (open supply.OrderLines remain the commitments consuming it)." },
    { termId: "norn.plan.otb.po.binding", reason: "system_specific", note: "Pack-level OTB budget consumption ledger binding supply.OrderLine commitments to plan.OtbEnvelope ceilings." },
    { termId: "norn.plan.published.forecast", reason: "canon_gap", note: "The canon ontology pack has no pinned plan-grain ML forecast revision class yet." },
    { termId: "norn.plan.guard.spread.drift", reason: "canon_gap", note: "The canon ontology pack has no top-down plan spread conservation guard predicate yet." },
  ],
})
