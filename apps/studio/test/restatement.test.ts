import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { buckets, type Declarations } from "../src/lib/answer.ts"
import { readDocument } from "../src/lib/exact.ts"
import { restated, sameAnswer } from "../src/lib/restatement.ts"
import type { Answer, HistoricalAnswer, Metric, ObjectType, Question } from "../src/lib/transport.ts"

// Engine documents retained byte for byte (crates/norn-cell/tests/answer_documents.rs).
const recorded = <T,>(name: string) =>
  readDocument<T>(readFileSync(join(import.meta.dir, "../../../fixtures/demo/answers", name), "utf8"), "recorded")
const declared: Declarations = {
  question: recorded<Question>("scrap-yield.question.json").document,
  metrics: recorded<readonly Metric[]>("scrap-yield.metrics.json").document,
  objectType: recorded<ObjectType>("scrap-yield.object-type.json").document,
}
const read = (name: string) => {
  const { document, numbers } = recorded<Answer>(name)
  return buckets(declared, { answer: document, numbers, at: "" })
}
const asOf = recorded<HistoricalAnswer>("scrap-yield.k1-as-of.answer.json")
const published = read("scrap-yield.k1.answer.json")
const corrected = read("scrap-yield.k2.answer.json")
const then = buckets(declared, { answer: asOf.document.result, numbers: asOf.numbers, at: "/result" })

test("only the corrected week is restated: 294 g / 300 g = 0.98 becomes 292.5 g / 300 g = 0.975", () => {
  const changes = restated(published, corrected, then)
  expect(changes.map(({ corrected: { label } }) => label)).toEqual(["Week of 19 Oct 2026"])
  const [change] = changes
  expect([change?.published.value, change?.published.totals]).toEqual(["0.98", ["294 g", "300 g"]])
  expect([change?.corrected.value, change?.corrected.totals]).toEqual(["0.975", ["292.5 g", "300 g"]])
})

test("the corrected record is named with both versions it was counted at", () => {
  const [change] = restated(published, corrected, then)
  expect(change?.records).toHaveLength(1)
  const [record] = change?.records ?? []
  expect(record?.id).toBe("SYN-MELT-15-1")
  expect([record?.published?.version, record?.published?.amounts]).toEqual([1, ["73.5 g", "75 g"]])
  expect([record?.corrected?.version, record?.corrected?.amounts]).toEqual([2, ["72 g", "75 g"]])
})

test("read at the earlier cutoff, the published answer is still exactly 0.98", () => {
  const [change] = restated(published, corrected, then)
  expect(change?.asOf && sameAnswer(change.asOf, change.published)).toBe(true)
  expect(change?.asOf?.value).toBe("0.98")
})

test("identical reads restate nothing", () => {
  expect(restated(published, published, then)).toEqual([])
})
