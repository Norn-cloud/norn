import { readFileSync } from "node:fs"
import { join } from "node:path"
import { buildQuestion, type Draft, type QuestionCatalog } from "../../src/builder.tsx"
import type { DefinitionStore, StoredDashboard, Tile } from "../../src/lib/definitions.ts"

const fixture = (path: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../../fixtures/demo", path), "utf8"))
const objectType = fixture("answers/scrap-yield.object-type.json")
const metrics = fixture("answers/scrap-yield.metrics.json")
export const cell = fixture("answers/scrap-yield.question.json")
/** The catalogue as GET /api/questions/catalog serves it for the demo analytics world. */
export const catalog: QuestionCatalog = {
  objectSet: "demo.Conversion",
  properties: objectType.properties.map(({ id, type }: { id: string; type: { kind: string; unit?: { code: string } } }) => ({ name: id, kind: type.kind, unit: type.unit?.code ?? null })),
  metrics: metrics.map(({ name, label, type }: { name: string; label: string; type: string }) => ({ name, label, type })),
  declarations: { metrics, objectType },
  grains: ["day", "week", "month", "quarter", "year"], operators: ["eq", "ne", "lt", "le", "gt", "ge"], example: cell,
}
export const weekly: Draft = { title: "Gold Yield, weekly", measures: ["demo.yield"], by: "occurred_at", grain: "week", filters: [{ property: "metal", operator: "eq", value: "gold" }, { property: "input", operator: "ge", value: "50.5" }], start: "2026-07-06", end: "2026-10-26" }
export const tile = (id: string, w = 2): Tile => ({ id, title: weekly.title, question: buildQuestion(weekly, catalog), view: { kind: "chart", chart: "line" }, layout: { x: 0, y: 0, w, h: 1 } })

export /** The cell's definition route as CELL-DO's contract states it: admit (no side effects), install a version, retire. */
function cellDouble(): DefinitionStore & { readonly kept: Map<string, StoredDashboard> } {
  const kept = new Map<string, StoredDashboard>()
  const receipt = (id: string, version: number) => ({ receiptHash: `r-${id}-${version}`, commitSeq: version, recordedAtMs: 1 })
  return {
    kept,
    list: async () => ({ items: [...kept.values()].map(({ id, version, dashboard }) => ({ id, title: dashboard.title, version, lifecycle: "active", updatedAtMs: 1 })), cursor: null }),
    get: async (id) => kept.get(id),
    admit: async (_id, dashboard) => dashboard.title.trim() === "" ? { admitted: false, findings: [{ code: "dashboard.title", message: "A dashboard needs a title.", pointer: "/title", law: null }] } : { admitted: true },
    install: async (id, dashboard, expectedVersion) => {
      const current = kept.get(id)?.version
      if (expectedVersion !== undefined && current !== expectedVersion) return { installed: false, conflict: true }
      const version = (current ?? 0) + 1
      kept.set(id, { id, version, dashboard })
      return { installed: true, id, version, receipt: receipt(id, version) }
    },
    retire: async (id, expectedVersion) => {
      const current = kept.get(id)?.version
      if (current !== expectedVersion) return { installed: false, conflict: true }
      kept.delete(id)
      return { installed: true, id, version: (current ?? 0) + 1, receipt: receipt(id, (current ?? 0) + 1) }
    },
  }
}
