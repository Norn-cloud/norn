import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { QuestionCard, planWords, questionVerdictOf, type QuestionVerdict } from "../src/agent/question.tsx"

// The demo cell's own admitted question (demo.yield_week) and the plan the checker returns for it.
const question = JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo/answers/scrap-yield.question.json"), "utf8"))
const admitted: QuestionVerdict = {
  kind: "question", question, admitted: true, findings: [],
  plan: { grain: "demo.Conversion", question, sums: [{ metric: "demo.recovered", property: "recovered" }, { metric: "demo.input", property: "input" }], metrics: [{ name: "demo.input", label: "Input" }, { name: "demo.recovered", label: "Recovered" }, { name: "demo.yield", label: "Yield" }] },
}

test("an admitted question reads in Business words: what, of what, by what, where and when", () => {
  expect(Object.fromEntries(planWords(admitted.plan ?? (() => { throw new Error("plan") })()))).toEqual({
    Measures: "Yield: recovered ÷ input",
    Of: "Conversion records",
    By: "week of occurred at",
    "Only where": "metal is gold",
    Over: "6 Jul 2026 until 26 Oct 2026",
  })
})

test("an admitted question is marked not yet run and shows no number of its own", () => {
  const html = renderToStaticMarkup(<QuestionCard verdict={admitted} />)
  expect(html).toContain("Admitted, not yet run")
  const [shown = ""] = html.split("<details")
  expect(shown).not.toMatch(/\d+(\.\d+)?%|= ?\d/)
})

test("a refused question shows the checker's reason; only the Worker's question verdicts are read as such", () => {
  const refused: QuestionVerdict = { kind: "question", question: {}, admitted: false, plan: null, findings: [{ code: "metric.unknown", message: "demo.purity is not a declared metric" }] }
  const html = renderToStaticMarkup(<QuestionCard verdict={refused} />)
  expect(html).toContain("Question refused by the checker")
  expect(html.split("<details")[0]).toContain("demo.purity is not a declared metric")
  expect(html.split("<details")[0]).not.toContain("metric.unknown")
  expect(questionVerdictOf(refused)).toBe(refused)
  expect(questionVerdictOf({ admitted: true })).toBeUndefined()
})

test("null, primitives and an admitted verdict without a readable plan are not question verdicts", () => {
  for (const output of [null, undefined, "question", 7, [], { ...admitted, plan: null }, { ...admitted, plan: { grain: "demo.Conversion" } }]) {
    expect(questionVerdictOf(output)).toBeUndefined()
  }
  expect(questionVerdictOf(admitted)).toBe(admitted)
})

test("a filter whose operator has no Business word reads as matches, never undefined", () => {
  const plan = admitted.plan ?? (() => { throw new Error("plan") })()
  for (const operator of [undefined, "like"]) {
    const words = Object.fromEntries(planWords({ ...plan, question: { ...plan.question, filter: [{ path: { property: "metal" }, operator, value: "gold" }] } }))
    expect(words["Only where"]).toBe("metal matches gold")
  }
})
