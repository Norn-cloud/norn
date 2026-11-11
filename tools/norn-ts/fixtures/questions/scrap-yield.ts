import { Norn } from "@norn/authoring"

export const ScrapYield = Norn.question({
  name: "demo.yield_week",
  of: "demo.Conversion",
  where: [{ property: "metal", operator: "eq", value: "gold" }],
  measure: "demo.yield",
  by: [{ property: "occurred_at", grain: "week" }],
  asOf: {
    basis: "effective",
    from: "2026-07-06T00:00:00Z",
    to: "2026-10-26T00:00:00Z",
    freshness: { kind: "live", maxAgeMs: 5000 },
  },
  population: { kind: "authorized_objects" },
})
