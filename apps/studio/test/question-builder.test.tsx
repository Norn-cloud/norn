import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { planWords, questionVerdictOf } from "../src/agent/question.tsx"
import { CHECKER_UNSERVED, QuestionBuilder, UNEDITABLE, checkerServedBy, buildQuestion, draftOf, questionFitsDraft, windowFitsDraft, catalogOf, checkedFor, fieldOf, initialDraft, issuesOf, type Draft, type QuestionCatalog } from "../src/builder.tsx"

const fixture = (path: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo", path), "utf8"))
// The catalogue as GET /api/questions/catalog serves it for the demo analytics world.
const objectType = fixture("answers/scrap-yield.object-type.json")
const metrics = fixture("answers/scrap-yield.metrics.json")
const cell = fixture("answers/scrap-yield.question.json")
const catalog: QuestionCatalog = {
  objectSet: "demo.Conversion",
  properties: objectType.properties.map(({ id, type }: { id: string; type: { kind: string; unit?: { code: string } } }) => ({ name: id, kind: type.kind, unit: type.unit?.code ?? null })),
  metrics: metrics.map(({ name, label, type }: { name: string; label: string; type: string }) => ({ name, label, type })),
  declarations: { metrics, objectType },
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

test("a checker response with a malformed finding is not a verdict", () => {
  const verdict = { kind: "question", question: {}, admitted: false, plan: null, findings: [{ code: "q.x", message: "m", pointer: "/questions/0/name" }] }
  expect(questionVerdictOf(verdict)).toBe(verdict as never)
  expect(questionVerdictOf({ ...verdict, findings: [{ code: "q.x", message: "m" }, { code: "q.y", message: "n", pointer: null }] })).toBeDefined()
  for (const finding of [null, "q.x", { code: "q.x" }, { code: 1, message: "m" }, { code: "q.x", message: "m", pointer: 3 }])
    expect(questionVerdictOf({ ...verdict, findings: [finding] })).toBeUndefined()
})

test("a catalogue response missing a field the builder reads is no catalogue", () => {
  const valid = { objectSet: "demo.Melt", properties: [{ name: "at", kind: "timestamp", unit: null }], metrics: [{ name: "yield", label: "Yield", type: "ratio" }], grains: ["week"], operators: ["eq"], example: { irVersion: "1", calendar: "utc", freshness: null } }
  expect(catalogOf(valid)).toBe(valid as never)
  for (const broken of [null, [], { ...valid, properties: null }, { ...valid, metrics: [null] }, { ...valid, grains: "week" }, { ...valid, operators: [1] }, { ...valid, example: {} }, { ...valid, example: null }])
    expect(catalogOf(broken)).toBeUndefined()
})

test("a check result shows only for the draft it was run on, so an edit or a late response for an older draft shows nothing", () => {
  const first = { title: "a" }, edited = { title: "b" }
  const result = { variables: first, data: "verdict", error: null }
  expect(checkedFor(result, first)).toEqual({ data: "verdict", error: null })
  expect(checkedFor(result, edited)).toEqual({ data: undefined, error: null })
  expect(checkedFor({ variables: first, data: undefined, error: new Error("down") }, edited)).toEqual({ data: undefined, error: null })
})

test("undo back to a checked draft does not revive its result: the result belongs to the editor revision it ran at", () => {
  const a = { title: "a" }
  const ranAt = {}, edited = {}, undone = {}
  const result = { variables: a, data: "verdict", error: null }
  expect(checkedFor(result, a, { ranAt, revision: ranAt })).toEqual({ data: "verdict", error: null })
  expect(checkedFor(result, { title: "b" }, { ranAt, revision: edited })).toEqual({ data: undefined, error: null })
  expect(checkedFor(result, a, { ranAt, revision: undone })).toEqual({ data: undefined, error: null })
})

test("a stored window finer than whole days cannot be edited here: the form says so and keeps Use off", () => {
  expect(windowFitsDraft({ window: { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" } } as never)).toBe(true)
  expect(windowFitsDraft({} as never)).toBe(true)
  expect(windowFitsDraft({ window: { start: "2026-07-06T12:30:00Z", end: "2026-10-26T00:00:00Z" } } as never)).toBe(false)
  expect(windowFitsDraft({ window: { start: "2026-07-06T00:00:00.000Z", end: "2026-10-26T00:00:00Z" } } as never)).toBe(false)
  const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><QuestionBuilder catalog={catalog} unsupported={UNEDITABLE} onUse={() => {}} /></QueryClientProvider>)
  expect(html).toContain("This question cannot be edited without changing it.")
})

test("a stored question the form would rebuild differently cannot be edited here; one it built round-trips", () => {
  const built = buildQuestion({ ...initialDraft(catalog), title: "Yield", start: "2026-07-06", end: "2026-10-26" }, catalog)
  expect(questionFitsDraft(built, catalog)).toBe(true)
  expect(questionFitsDraft({ ...built, freshness: { maxLagMs: 1 } }, catalog)).toBe(false)
  expect(questionFitsDraft({ ...built, groupBy: [{ path: { property: "occurred_at" } }, { path: { property: "metal" } }] } as never, catalog)).toBe(false)
  expect(draftOf(built).title).toBe("yield")
})

test("when the deployment does not serve the question checker, the builder says so and is disabled", () => {
  expect(checkerServedBy(404)).toBe(false)
  expect(checkerServedBy(503)).toBe(false)
  expect(checkerServedBy("unreachable")).toBe(false)
  expect(checkerServedBy(405)).toBe(true)
  const off = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><QuestionBuilder catalog={catalog} checkerServed={false} /></QueryClientProvider>)
  // Server rendering escapes the apostrophe.
  const shown = CHECKER_UNSERVED.replaceAll("'", "&#x27;")
  expect(off).toContain(shown)
  expect(off).toMatch(/<fieldset disabled=""/)
  const on = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><QuestionBuilder catalog={catalog} checkerServed={true} /></QueryClientProvider>)
  expect(on).not.toContain(shown)
  expect(on).not.toMatch(/<fieldset disabled=""/)
})
