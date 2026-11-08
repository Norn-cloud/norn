import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { AnswerView } from "../src/answer-view.tsx"
import { fieldOf } from "../src/builder.tsx"
import { readDocument } from "../src/lib/exact.ts"
import { RunQuestion, answerRead, notRun } from "../src/run.tsx"
import { catalog, cell } from "./support/definitions.ts"

const answerText = readFileSync(join(import.meta.dir, "../../../fixtures/demo/answers/scrap-yield.k1.answer.json"), "utf8")

test("a run with no answer says why: the route not there yet, signed out, the cell's own refusal, never an empty chart", () => {
  expect(notRun({ kind: "unavailable", status: 404 })).toBe("Not run: the evaluate route isn't connected yet.")
  expect(notRun({ kind: "unavailable", status: 0 })).toBe("Not run: the evaluate route isn't connected yet.")
  expect(notRun({ kind: "unavailable", status: 401 })).toBe("Not run: sign in again.")
  expect(notRun({ kind: "unavailable", status: 502 })).toBe("Not run: the cell did not answer (502).")
  expect(notRun({ kind: "refused", refusal: { code: "authz.denied", message: "the read request is not authorized" } })).toBe("the read request is not authorized")
  expect(notRun({ kind: "too-large", code: "question.bucket_bound_exceeded", message: "" })).toBe("Too large to run: it would answer with more buckets than the cell allows.")
  expect(notRun({ kind: "too-large", code: "core.compute.bound_exceeded", message: "the question reads more than 100000 rows" })).toBe("Too large to run: the question reads more than 100000 rows.")
  expect(notRun({ kind: "bad-request", message: "unknown field `extra`" })).toBe("Not run: the cell refused the request (unknown field `extra`).")
})

test("a run answer reads with the catalogue's declarations, like a published answer", () => {
  const read = answerRead({ ...cell, name: "studio.gold_yield" }, catalog, { kind: "answer", read: readDocument(answerText, "live") })
  expect(read.metrics.document as unknown).toEqual(catalog.declarations.metrics)
  const html = renderToStaticMarkup(<AnswerView read={read} heading="h3" />)
  expect(html).toContain(">Live<")
  expect(html).toContain("Every week")
})

test("the cell's refusal of a run lands on the builder's fields, whether its pointer starts at the question or the checked document", () => {
  expect(fieldOf("/aggregate/0/metric")).toBe("measure.0")
  expect(fieldOf("/filter/1/value")).toBe("filter.1")
  expect(fieldOf("/window")).toBe("window")
  expect(fieldOf("/questions/0/aggregate/0/metric")).toBe("measure.0")
  expect(fieldOf("/question/aggregate/0/metric")).toBe("measure.0")
  expect(fieldOf("/question/window")).toBe("window")
})

test("Run is offered before anything runs, and nothing is shown in place of an answer", () => {
  const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><RunQuestion question={cell} catalog={catalog} /></QueryClientProvider>)
  expect(html).toContain(">Run<")
  expect(html).not.toContain("Every week")
})
