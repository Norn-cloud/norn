import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { readDocument } from "../src/lib/exact.ts"
import { bucketLabel, headline, period, scope } from "../src/lib/format.ts"
import { measured, operandUnits } from "../src/lib/units.ts"
import type { Answer, Metric, ObjectType, Question } from "../src/lib/transport.ts"

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

test("each operand carries the unit its stored property declares: 294 g / 300 g", () => {
  const metrics = readDocument<readonly Metric[]>(recorded("scrap-yield.metrics.json"), "recorded").document
  const objectType = readDocument<ObjectType>(recorded("scrap-yield.object-type.json"), "recorded").document
  const units = operandUnits(question, metrics, objectType)
  expect(units).toEqual(["g", "g"])
  expect(`${measured("294", units[0])} / ${measured("300", units[1])}`).toBe("294 g / 300 g")
})

test("an operand without a declared unit shows its bare amount", () => {
  const metrics = readDocument<readonly Metric[]>(recorded("scrap-yield.metrics.json"), "recorded").document
  const unitless = { name: "demo.Conversion", properties: [] } as ObjectType
  expect(operandUnits(question, metrics, unitless)).toEqual([undefined, undefined])
  expect(measured("294", undefined)).toBe("294")
})
