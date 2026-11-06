import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { questionRecommendations, restate, restatement, windowWords } from "../worker/question-tree.ts"
import { ANALYTICS, isQuestionRoute, questionsRoute } from "../worker/questions.ts"

const cell = ANALYTICS.example as Parameters<typeof restate>[0]

test("the restatement is the question in plain words, generated from the IR alone", () => {
  expect(restate(cell)).toBe("Yield (recovered ÷ input) of Conversion records, by week of occurred at, where metal is gold, from 6 Jul to 26 Oct 2026 (26 Oct not included)")
  expect(restate({ ...cell, aggregate: [{ metric: "demo.input" }, { metric: "demo.recovered" }], groupBy: [], filter: [] }))
    .toBe("Input (sum of input) and Recovered (sum of recovered) of Conversion records, in total, from 6 Jul to 26 Oct 2026 (26 Oct not included)")
  expect(restate({ ...cell, filter: [{ path: { property: "input" }, operator: "ge", value: 50 }, { path: { property: "metal" }, operator: "ne", value: "silver" }] }))
    .toContain("where input is at least 50 and metal is not silver")
  expect(windowWords({ start: "2025-12-29T00:00:00Z", end: "2026-01-05T00:00:00Z" })).toBe("from 29 Dec 2025 to 5 Jan 2026 (5 Jan not included)")
  expect(windowWords({})).toBe("over no stated window")
})

test("a sign-off records the hash of exactly the words read: the same question hashes the same, a changed one does not", async () => {
  const first = await restatement(cell)
  expect(first.sha256).toMatch(/^[0-9a-f]{64}$/)
  expect((await restatement(JSON.parse(JSON.stringify(cell)))).sha256).toBe(first.sha256)
  expect((await restatement({ ...cell, filter: [] })).sha256).not.toBe(first.sha256)
})

// The checker stand-in admits only weekly grouping, as the demo cell's engine does.
const weeklyOnly = (_a: string, _m: string, question: string) => JSON.stringify({ admitted: (JSON.parse(question) as { groupBy?: { grain?: string }[] }).groupBy?.[0]?.grain === "week" })

test("each column offers what the declarations allow, the top pick following the cell's admitted question; grains only as the checker admits", () => {
  const tree = questionRecommendations(weeklyOnly)
  expect(tree.basis).toContain("never from row values")
  expect(tree.measures.map(({ id, recommended }) => `${id}${recommended ? "*" : ""}`)).toEqual(["demo.input", "demo.recovered", "demo.yield*"])
  expect(tree.measures.find(({ id }) => id === "demo.yield")?.detail).toBe("recovered ÷ input")
  expect(tree.measures.find(({ id }) => id === "demo.input")?.detail).toBe("sum of input · g")
  expect(tree.groupBy.map(({ id, recommended }) => `${id}${recommended ? "*" : ""}`)).toEqual(["", "occurred_at*", "metal"])
  expect(tree.grains).toEqual({ occurred_at: [{ id: "week", label: "Week", detail: "each week of occurred at", recommended: true, value: { grain: "week" } }] })
  expect(tree.conditions.map(({ label, recommended }) => `${label}${recommended ? "*" : ""}`)).toEqual(["metal is gold*", "Every record"])
  expect(tree.windows).toEqual([{ id: "cell", label: "from 6 Jul to 26 Oct 2026 (26 Oct not included)", detail: "the cell's admitted window", recommended: true, value: { window: cell.window } }])
})

test("the recommendations route is a question route under the same gate, and the check returns the restatement with its hash", async () => {
  expect(isQuestionRoute("/api/questions/recommendations")).toBe(true)
  expect(isQuestionRoute("/api/questions/anything")).toBe(false)
  const get = await questionsRoute(new Request("https://studio.test/api/questions/recommendations"), "/api/questions/recommendations", async () => weeklyOnly)
  expect(get.status).toBe(200)
  expect(((await get.json()) as { measures: unknown[] }).measures.length).toBe(3)
  const admit = () => JSON.stringify({ admitted: true, plan: null, diagnostics: { diagnostics: [] } })
  const checked = await questionsRoute(new Request("https://studio.test/api/questions/check", { method: "POST", body: JSON.stringify({ question: cell }) }), "/api/questions/check", async () => admit)
  expect(await checked.json()).toMatchObject({ kind: "question", admitted: true, restatement: { text: restate(cell), sha256: (await restatement(cell)).sha256 } })
})

// The real checker, from the built package when this checkout's checker exports it.
const pkg = process.env.NORN_CHECKER_WASM_PKG
const exported = pkg !== undefined && readFileSync(join(pkg, "norn_checker_wasm.d.ts"), "utf8").includes("admitQuestion")
test.skipIf(!exported)("the real checker admits weekly grouping of the cell's question, and that is the grain offered", async () => {
  const checker = await import(join(pkg ?? "", "norn_checker_wasm.js")) as { initSync: (m: { module: Uint8Array }) => void; admitQuestion: (a: string, m: string, q: string) => string }
  checker.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
  const tree = questionRecommendations(checker.admitQuestion)
  expect(tree.grains.occurred_at?.map(({ id }) => id)).toContain("week")
  expect(tree.grains.occurred_at?.find(({ recommended }) => recommended)?.id).toBe("week")
})
