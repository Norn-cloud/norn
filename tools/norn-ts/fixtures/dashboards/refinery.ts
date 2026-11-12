import { Norn } from "@norn/authoring"

const window = { basis: "effective", from: "2026-07-06T00:00:00Z", to: "2026-10-26T00:00:00Z", freshness: { kind: "live", maxAgeMs: 5000 } } as const

export const GoldYield = Norn.question({
  name: "demo.gold_yield_weekly", of: "demo.Conversion", measure: "demo.yield",
  where: [{ property: "metal", operator: "eq", value: "gold" }],
  by: [{ property: "occurred_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

export const Recovered = Norn.question({
  name: "demo.recovered_total", of: "demo.Conversion", measure: "demo.recovered",
  where: [], by: [], asOf: window, population: { kind: "authorized_objects" },
})

export const RecoveredByMetal = Norn.question({
  name: "demo.recovered_by_metal", of: "demo.Conversion", measure: "demo.recovered",
  where: [], by: [{ property: "metal" }], asOf: window, population: { kind: "authorized_objects" },
})

export const Refinery = Norn.dashboard({
  title: "Refinery weekly",
  tiles: [
    { title: "Recovered in total", question: Recovered, view: "number" },
    { title: "Recovered by metal", question: RecoveredByMetal, view: "pie" },
    { title: "Gold yield, weekly", question: GoldYield, view: "line" },
  ],
})
