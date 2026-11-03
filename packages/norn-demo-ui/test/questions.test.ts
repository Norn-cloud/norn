import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderQuestionBoard, type ExactRead, type QuestionCard } from "../src/questions.ts"

// Documents retained byte-for-byte from the cell evaluator (crates/norn-cell/tests/answer_documents.rs).
const directory = join(import.meta.dir, "../../../fixtures/demo/answers")
const read = <T>(name: string): ExactRead<T> => {
  const source = readFileSync(join(directory, name), "utf8")
  return { document: JSON.parse(source) as T, source }
}
const question = read<QuestionCard["question"]["document"]>("scrap-yield.question.json")
const k1 = read<QuestionCard["answer"]["document"]>("scrap-yield.k1.answer.json")
const k2 = read<QuestionCard["answer"]["document"]>("scrap-yield.k2.answer.json")
const quarantined = read<QuestionCard["answer"]["document"]>("scrap-yield.k1-quarantined.answer.json")
const asOf = read<NonNullable<QuestionCard["asOf"]>["document"]>("scrap-yield.k1-as-of.answer.json")
const SHOWCASE = '"2026-10-19"'

test("every bucket shows the evaluator's value, numerator and denominator", () => {
  const html = renderQuestionBoard({ questions: [{ question, answer: k1 }] })
  const rows = Object.values(k1.document.rows)
  expect(rows.length).toBe(16)
  expect(html.match(/<td class="num value">/g)?.length).toBe(rows.length)
  expect(html).toContain('aria-label="Where this number comes from"><span class="exact">0.98</span></a></td><td class="num"><span class="exact">294</span></td><td class="num"><span class="exact">300</span></td>')
})

test("the question's declaration is shown as the server states it", () => {
  const html = renderQuestionBoard({ questions: [{ question, answer: k1 }] })
  for (const text of ["demo.yield_week", "<code>demo.yield</code> over <code>demo.Conversion</code>", "<code>metal</code> = <code>gold</code>",
    "week of <code>occurred_at</code>", "<code>2026-07-06T00:00:00Z</code> to <code>2026-10-26T00:00:00Z</code>", "live, at most 5000 ms old"]) {
    expect(html).toContain(text)
  }
})

test("freshness, knowledge revision and frontier come from the read", () => {
  const html = renderQuestionBoard({ questions: [{ question, answer: k1 }] })
  expect(html).toContain('class="chip freshness exact"')
  expect(html).toContain("<b>commit 1</b>")
  expect(html).toContain("<b>2026-10-26T00:00:00Z</b>")
  expect(html).toContain("<td><code>erp</code></td><td><code>measurements</code></td><td><code>k1/80</code></td>")
  expect(html).not.toContain("quarantined input")
})

test("a quarantined input makes the answer incomplete and names its finding", () => {
  const html = renderQuestionBoard({ questions: [{ question, answer: quarantined }] })
  expect(html).toContain('class="chip freshness incomplete"')
  expect(html).toContain("1 quarantined input excluded")
  expect(html).toContain("<code>SYN-MELT-15-1</code> <span class=\"sub\">finding <code>core07-finding</code>")
  const showcase = quarantined.document.rows[SHOWCASE]
  expect(Object.keys(showcase?.members ?? {})).not.toContain("SYN-MELT-15-1")
})

test("each value drills through to exactly the object versions it sums", () => {
  const html = renderQuestionBoard({ questions: [{ question, answer: k1 }] })
  const index = Object.keys(k1.document.rows).indexOf(SHOWCASE)
  const drill = html.slice(html.indexOf(`<section class="drill" id="question-0-${index}"`))
  const section = drill.slice(0, drill.indexOf("</section>"))
  expect(html).toContain(`<a href="#question-0-${index}"`)
  expect(section.match(/<tr><td><code>SYN-MELT-15-\d<\/code><\/td>/g)?.length).toBe(4)
  expect(section).toContain('<td><code>SYN-MELT-15-1</code></td><td class="num">1</td><td class="num"><span class="exact">73.5</span></td><td class="num"><span class="exact">75</span></td>')
  expect(html.match(/<section class="drill"/g)?.length).toBe(16)
  expect(html).toContain("Source rows and documents behind each object version are not exposed by the server.")
})

test("a restatement lists only the buckets whose answer changed", () => {
  const html = renderQuestionBoard({ questions: [{ question, answer: k2, asOf }] })
  expect(html).toContain("Known as of commit 1 at <code>2026-10-26T00:00:00.000Z</code>, compared with commit 2 now. 15 buckets are unchanged.")
  expect(html).toContain('<td class="num"><span class="exact">0.98</span></td><td class="num"><span class="exact">0.975</span></td>')
})

test("decimal text is rendered as sent, never through a binary float", () => {
  const scaled = k1.source.replace('"value": 0.98,', '"value": 0.980,')
  expect(scaled).not.toBe(k1.source)
  const html = renderQuestionBoard({ questions: [{ question, answer: { document: JSON.parse(scaled), source: scaled } }] })
  expect(html).toContain('<span class="exact">0.980</span>')
})

test("a source that is not the same read refuses to render", () => {
  expect(() => renderQuestionBoard({ questions: [{ question, answer: { document: k1.document, source: k2.source } }] })).toThrow()
  expect(() => renderQuestionBoard({ questions: [{ question, answer: k2, asOf: { document: asOf.document, source: k1.source } }] })).toThrow()
})

test("server text never becomes markup", () => {
  const hostile = '</h3><script>alert("x")</script>'
  const named = question.source.replace('"demo.yield_week"', JSON.stringify(hostile))
  const html = renderQuestionBoard({ questions: [{ question: { document: JSON.parse(named), source: named }, answer: k1 }] })
  expect(html).not.toContain("<script>")
  expect(html).toContain("&lt;script&gt;")
})
