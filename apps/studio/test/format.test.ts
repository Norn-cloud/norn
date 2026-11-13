import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { readDocument } from "../src/lib/exact.ts"
import { bucketLabel, derivedMetricWords, headline, pathWords, percent, period, scope, words } from "../src/lib/format.ts"
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

test("a count is a number of records, never an amount in its property's unit", () => {
  const metrics = readDocument<Metric[]>(recorded("scrap-yield.metrics.json"), "recorded").document
  const objectType = readDocument<ObjectType>(recorded("scrap-yield.object-type.json"), "recorded").document
  const counted = metrics.map((metric) => metric.type_params.metric_aggregation_params
    ? { ...metric, type_params: { ...metric.type_params, metric_aggregation_params: { ...metric.type_params.metric_aggregation_params, agg: "count" as const } } }
    : metric)
  expect(operandUnits(question, counted, objectType)).toEqual([undefined, undefined])
})

test("a ratio reads as a percentage rounded on its decimal text, never a float", () => {
  expect(percent("0.979685")).toBe("97.97%")
  expect(percent("0.98")).toBe("98.00%")
  expect(percent("0.975")).toBe("97.50%")
  expect(percent("0.979995")).toBe("98.00%")
  expect(percent("1")).toBe("100.00%")
  expect(percent("-0.000049")).toBe("0.00%")
  expect(percent("unavailable")).toBe("unavailable")
})

// The metric compiler's path encoding (crates/norn-checker/src/metric.rs `field_name`), restated so
// the decoder is checked against it over many names, underscores and lengths included.
const encodePath = (parts: readonly string[]) => `norn_path_${parts.map((part) => `${part.length}_${part.replaceAll("_", "_u")}`).join("_")}_value`

test("a compiled link path reads as the links' possessives and the property, for any parts", () => {
  let seed = 20261113
  const next = () => (seed = (Math.imul(seed, 1103515245) + 12345) >>> 0) / 2 ** 32
  const letters = "abcdefghijklmnopqrstuvwxyz_0123456789"
  const part = () => { let text = String.fromCharCode(97 + Math.floor(next() * 26)); for (let n = Math.floor(next() * 14); n > 0; n--) text += letters[Math.floor(next() * letters.length)]; return text }
  for (let round = 0; round < 500; round++) {
    const parts = Array.from({ length: 1 + Math.floor(next() * 4) }, part)
    const spoken = parts.map((one) => one.replaceAll("_", " "))
    expect(pathWords(encodePath(parts))).toBe([...spoken.slice(0, -1).map((link) => `${link}'s`), spoken.at(-1)].join(" "))
  }
  expect(pathWords("occurred_at")).toBeUndefined()
  expect(pathWords("norn_path_9_event_value")).toBeUndefined()
})

test("a derived metric's id reads as what it measures; the raw id never reaches the words", () => {
  const time = encodePath(["event", "occurred_at"])
  expect(derivedMetricWords(`analysis.object_63616e6f6e2e506f7374696e67.amount.by_${time}`)).toBe("Amount")
  expect(derivedMetricWords("analysis.object_63616e6f6e2e486f6c64696e67.quantity.as_of_observed_at")).toBe("Quantity, as of observed at")
  expect(words(`analysis.object_63616e6f6e2e506f7374696e67.amount.by_${time}`)).toBe("Amount")
  expect(words(time)).toBe("Event's occurred at")
  expect(words("demo.yield_week")).toBe("Yield week")
})
