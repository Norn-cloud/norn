import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { planWords } from "../src/agent/question.tsx"
import { QuestionBuilder, buildQuestion, fieldOf, initialDraft, issuesOf, type Draft, type QuestionCatalog } from "../src/builder.tsx"

const fixture = (path: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo", path), "utf8"))
// The catalogue as GET /api/questions/catalog serves it for the demo analytics world.
const world = fixture("analytics.world.json")
const cell = fixture("answers/scrap-yield.question.json")
const catalog: QuestionCatalog = {
  objectSet: "demo.Conversion",
  properties: world.artifacts[0].payload.objectType.properties.map(({ id, type }: { id: string; type: { kind: string; unit?: { code: string } } }) => ({ name: id, kind: type.kind, unit: type.unit?.code ?? null })),
  metrics: world.metrics.map(({ name, label, type }: { name: string; label: string; type: string }) => ({ name, label, type })),
  grains: ["day", "week", "month", "quarter", "year"],
  operators: ["eq", "ne", "lt", "le", "gt", "ge"],
  example: cell,
}
const weekly: Draft = { title: "Yield week", measures: ["demo.yield"], by: "occurred_at", grain: "week", filters: [{ property: "metal", operator: "eq", value: "gold" }], start: "2026-07-06", end: "2026-10-26" }

test("picking the cell's own question in the builder writes the cell's QuestionIR, field for field", () => {
  const { name, ...built } = buildQuestion(weekly, catalog)
  const { name: _cellName, ...expected } = cell
  expect(built).toEqual(expected)
  expect(name).toBe("studio.yield_week")
})

test("the builder writes what was picked and judges nothing: grains only on time, numbers typed by the property", () => {
  const draft: Draft = { ...weekly, by: "metal", grain: "month", filters: [{ property: "input", operator: "ge", value: "50.5" }, { property: "metal", operator: "eq", value: "18" }, { property: "input", operator: "lt", value: "lots" }] }
  const built = buildQuestion(draft, catalog)
  expect(built.groupBy).toEqual([{ path: { property: "metal" } }])
  expect(built.filter.map(({ value }) => value)).toEqual([50.5, "18", "lots"])
  expect(buildQuestion({ ...weekly, by: "" }, catalog).groupBy).toEqual([])
  expect(buildQuestion({ ...weekly, grain: "month" }, catalog).groupBy).toEqual([{ path: { property: "occurred_at" }, grain: "month" }])
})

test("the form offers the catalogue in Business words and starts on the time property by week", () => {
  expect(initialDraft(catalog)).toMatchObject({ measures: ["demo.input"], by: "occurred_at", grain: "week", filters: [] })
  const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><QuestionBuilder catalog={catalog} /></QueryClientProvider>)
  for (const text of [">Input<", ">Recovered<", ">Yield<", ">occurred at<", ">Week<", ">Quarter<", "Check with the server", "Nothing is run yet", 'aria-label="Undo"', 'aria-label="Redo"', "All changes saved", "Add measure", "Add condition", 'aria-label="Duplicate measure 1"', 'data-field="window"']) expect(html).toContain(text)
  // Ids stay in option values; the reader sees labels.
  expect(html).not.toMatch(/>demo\.[a-z_]+</)
})

test("an admitted plan reads the IR's own operators in words, never the code", () => {
  const plan = { grain: "demo.Conversion", question: { ...cell, filter: [{ path: { property: "input" }, operator: "le", value: 50 }, { path: { property: "loss" }, operator: "ge", value: 1 }] }, sums: [], metrics: [{ name: "demo.yield", label: "Yield" }] }
  expect(Object.fromEntries(planWords(plan))["Only where"]).toBe("input is at most 50 and loss is at least 1")
})

test("two measures write two aggregates in their order; none writes none", () => {
  expect(buildQuestion({ ...weekly, measures: ["demo.recovered", "demo.input"] }, catalog).aggregate).toEqual([{ metric: "demo.recovered" }, { metric: "demo.input" }])
  expect(buildQuestion({ ...weekly, measures: [] }, catalog).aggregate).toEqual([])
})

test("a checker finding lands on the field its pointer names, or on the question as a whole", () => {
  expect(fieldOf("/questions/0/aggregate/1/metric")).toBe("measure.1")
  expect(fieldOf("/questions/0/metrics")).toBe("measures")
  expect(fieldOf("/questions/0/filters/2/value")).toBe("filter.2")
  expect(fieldOf("/questions/0/filter/0")).toBe("filter.0")
  expect(fieldOf("/questions/0/dimensions/0")).toBe("by")
  expect(fieldOf("/questions/0/window/end")).toBe("window")
  expect(fieldOf("/questions/0")).toBeNull()
  expect(fieldOf(null)).toBeNull()
  const refused = { kind: "question" as const, question: {}, admitted: false, plan: null, findings: [{ code: "metric.unknown", message: "no metric demo.purity", pointer: "/questions/0/aggregate/0/metric" }, { code: "engine.unsupported", message: "weekly only", pointer: null }] }
  expect(issuesOf(refused)).toEqual([{ code: "metric.unknown", message: "no metric demo.purity", field: "measure.0" }, { code: "engine.unsupported", message: "weekly only", field: null }])
  expect(issuesOf({ ...refused, admitted: true })).toEqual([])
})

test("a world without measures shows the empty state, not an empty form", () => {
  const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><QuestionBuilder catalog={{ ...catalog, metrics: [] }} /></QueryClientProvider>)
  expect(html).toContain("declares no measures yet")
  expect(html).not.toContain("Check with the server")
})
