import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { ChoiceColumn, PathSummary, pathNumbers } from "../src/components/choice-columns.tsx"
import { buildQuestion } from "../src/builder.tsx"
import { QuestionTree, recommendedDraft, selection, type Recommendations } from "../src/question-tree.tsx"
import { catalog, cell } from "./support/definitions.ts"

// GET /api/questions/recommendations for the demo world, as the Worker serves it (worker/question-tree.ts).
const gold = { path: { property: "metal" }, operator: "eq", value: "gold" }
const tree: Recommendations = {
  basis: "From the catalog's declarations and the cell's admitted question demo.yield_week; never from row values.",
  measures: [
    { id: "demo.input", label: "Input", detail: "sum of input · g", recommended: false, value: { aggregate: [{ metric: "demo.input" }] } },
    { id: "demo.recovered", label: "Recovered", detail: "sum of recovered · g", recommended: false, value: { aggregate: [{ metric: "demo.recovered" }] } },
    { id: "demo.yield", label: "Yield", detail: "recovered ÷ input", recommended: true, value: { aggregate: [{ metric: "demo.yield" }] } },
  ],
  groupBy: [
    { id: "", label: "In total", detail: "one answer, not grouped", recommended: false, value: { groupBy: [] } },
    { id: "occurred_at", label: "occurred at", detail: "a time, grouped by a grain", recommended: true, value: { groupBy: [{ path: { property: "occurred_at" } }] } },
    { id: "metal", label: "metal", detail: "each value apart", recommended: false, value: { groupBy: [{ path: { property: "metal" } }] } },
  ],
  grains: { occurred_at: [{ id: "week", label: "Week", detail: "each week of occurred at", recommended: true, value: { grain: "week" } }] },
  conditions: [
    { id: "metal:eq:gold", label: "metal is gold", detail: "as the cell's admitted question asks", recommended: true, value: { filter: [gold] } },
    { id: "every", label: "Every record", detail: "no condition", recommended: false, value: { filter: [] } },
  ],
  filterable: [{ name: "metal", kind: "text", unit: null, values: [] }, { name: "input", kind: "decimal", unit: "g", values: [] }],
  windows: [{ id: "cell", label: "from 6 Jul to 26 Oct 2026 (26 Oct not included)", detail: "the cell's admitted window", recommended: true, value: { window: cell.window } }],
}

test("the tree opens on the top pick of every column, which is exactly the cell's admitted question", () => {
  const draft = recommendedDraft(tree)
  expect(draft).toEqual({ title: "", measures: ["demo.yield"], by: "occurred_at", grain: "week", filters: [{ property: "metal", operator: "eq", value: "gold" }], start: "2026-07-06", end: "2026-10-26" })
  const { name: _built, ...built } = buildQuestion(draft, catalog)
  const { name: _cell, ...expected } = cell
  expect(built).toEqual(expected)
  expect(selection(tree, draft)).toEqual({ measure: "demo.yield", by: "occurred_at", grain: "week", condition: "metal:eq:gold", window: "cell" })
})

test("an edit beyond the offered options reads as that column's own choice", () => {
  const draft = { ...recommendedDraft(tree), filters: [{ property: "input", operator: "ge", value: "50" }], start: "2026-08-03" }
  expect(selection(tree, draft)).toMatchObject({ condition: "custom", window: "custom" })
  expect(selection(tree, { ...draft, filters: [] })).toMatchObject({ condition: "every" })
})

test("columns number their options and mark the server's top pick and the agent's; the path reads by number", () => {
  const html = renderToStaticMarkup(<ChoiceColumn title="Measure" selected="b" onSelect={() => {}} options={[{ id: "a", label: "Input", marks: ["recommended"] }, { id: "b", label: "Yield", marks: ["agent"] }]} />)
  expect(html).toMatch(/>1<\/span><span[^>]*>Input<\/span><span[^>]*>Recommended</)
  expect(html).toMatch(/>2<\/span><span[^>]*>Yield<\/span><span[^>]*>Agent&#x27;s pick</)
  expect(html).toMatch(/aria-pressed="true"[^>]*>.*Yield/)
  expect(pathNumbers([{ options: [{ id: "a" }, { id: "b" }], selected: "b" }, { options: [{ id: "x" }], selected: undefined }])).toEqual([2, undefined])
  expect(renderToStaticMarkup(<PathSummary numbers={[3, 2, 1, 1]} />).replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim()).toBe("Then 3 – 2 – 1 – 1")
})

test("the tree shows the four columns, the recommended path and keeps Run closed until sign-off", () => {
  const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><QuestionTree tree={tree} catalog={catalog} /></QueryClientProvider>)
  for (const title of [">Measure<", ">Group by<", "Every (grain of occurred at)", ">Only where<", ">Window<", "Recommended", "never from row values"]) expect(html).toContain(title)
  expect(html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ")).toContain("Then 3 – 2 – 1 – 1 – 1")
  expect(html).toContain("Run opens once you have signed off on the question in plain words.")
  expect(html).not.toContain(">Run<")
})
