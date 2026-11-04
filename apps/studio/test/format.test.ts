import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { readDocument } from "../src/lib/exact.ts"
import { bucketLabel, headline, period, scope } from "../src/lib/format.ts"
import type { Answer, Question } from "../src/lib/transport.ts"

// Engine documents retained byte for byte (crates/norn-cell/tests/answer_documents.rs).
const recorded = (name: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo/answers", name), "utf8")
const question = readDocument<Question>(recorded("scrap-yield.question.json"), "recorded").document
const answer = readDocument<Answer>(recorded("scrap-yield.k1.answer.json"), "recorded")
const WEEK = `/rows/${'"2026-10-19"'}`

test("the showcase week reads as the engine wrote it: 294 / 300 = 0.98", () => {
  expect(answer.numbers.get(`${WEEK}/value`)).toBe("0.98")
  expect(answer.numbers.get(`${WEEK}/totals/0`)).toBe("294")
  expect(answer.numbers.get(`${WEEK}/totals/1`)).toBe("300")
  expect(answer.numbers.get(`${WEEK}/members/SYN-MELT-15-1/amounts/0`)).toBe("73.5")
})

test("decimal text keeps its scale, never passing through a binary float", () => {
  const source = recorded("scrap-yield.k1.answer.json").replace('"value": 0.98,', '"value": 0.980,')
  expect(readDocument<Answer>(source, "recorded").numbers.get(`${WEEK}/value`)).toBe("0.980")
})

test("a malformed document refuses to read", () => {
  expect(() => readDocument("{\"rows\": {\"a\" 1}}", "recorded")).toThrow()
})

test("the question is stated in plain words", () => {
  expect(headline(question)).toBe("Yield by week")
  expect(scope(question)).toBe("Conversion records · Metal is gold")
  expect(period(question.window)).toBe("6 Jul 2026 – 25 Oct 2026")
  expect(bucketLabel("2026-10-19", question)).toBe("Week of 19 Oct 2026")
})
