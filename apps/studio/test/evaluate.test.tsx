import { expect, mock, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { evaluateQuestion, traceQuestion } from "../src/lib/evaluate.ts"
import { readDocument } from "../src/lib/exact.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { AnswerView } = await import("../src/questions.tsx")

const answers = join(import.meta.dir, "../../../fixtures/demo/answers")
const text = (name: string) => readFileSync(join(answers, name), "utf8")
const answerText = text("scrap-yield.k1.answer.json")
const question = JSON.parse(text("scrap-yield.question.json"))
const calls: { path: string; body: unknown }[] = []
const answer = (status: number, body: string) => async (path: string, init?: RequestInit) => {
  calls.push({ path, body: JSON.parse(String(init?.body)) })
  return new Response(body, { status })
}

test("an admitted question is answered with the server's document, its numbers read exactly as written", async () => {
  const result = await evaluateQuestion(question, answer(200, answerText))
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/questions:evaluate?orgId=org_demo&environmentId=analytics", body: { question } })
  if (result.kind !== "answer") throw new Error(result.kind)
  expect(result.read.origin).toBe("live")
  expect(result.read.document.lineageRef).toBe(JSON.parse(answerText).lineageRef)
  const [key] = Object.keys(result.read.document.rows)
  expect(result.read.numbers.get(`/rows/${key?.replaceAll("~", "~0").replaceAll("/", "~1")}/value`)).toBeDefined()
})

test("the cell's checker refusal gives each finding's code, message and field; the read gates' refusal is the host's own; anything else is unavailable", async () => {
  expect(await evaluateQuestion(question, answer(422, JSON.stringify({ format: "norn/refusal@2", code: "question.refused", diagnostics: [{ code: "question.window_order", severity: "error", message: "The time window must be an ordered half-open UTC interval.", span: { pointer: "/question/window" } }] }))))
    .toEqual({ kind: "not-a-question", findings: [{ code: "question.window_order", severity: "error", message: "The time window must be an ordered half-open UTC interval.", pointer: "/question/window" }] })
  expect(await evaluateQuestion(question, answer(403, '{"code":"authz.denied","message":"the read request is not authorized"}')))
    .toEqual({ kind: "refused", refusal: { code: "authz.denied", message: "the read request is not authorized" } })
  expect(await evaluateQuestion(question, answer(403, '{"error":"Cross-origin request refused."}'))).toEqual({ kind: "unavailable", status: 403 })
  expect(await evaluateQuestion(question, answer(404, "{}"))).toEqual({ kind: "unavailable", status: 404 })
  // Bounds: a typed "too large", never a partial answer; an unknown body member is the request's own refusal.
  expect(await evaluateQuestion(question, answer(413, '{"code":"core.compute.bound_exceeded","message":"the question reads more than 100000 rows"}'))).toEqual({ kind: "too-large", code: "core.compute.bound_exceeded", message: "the question reads more than 100000 rows" })
  expect(await evaluateQuestion(question, answer(413, '{"code":"question.bucket_bound_exceeded"}'))).toEqual({ kind: "too-large", code: "question.bucket_bound_exceeded", message: "" })
  expect(await evaluateQuestion(question, answer(400, '{"error":"unknown field `extra`"}'))).toEqual({ kind: "bad-request", message: "unknown field `extra`" })
  expect(await evaluateQuestion(question, answer(200, "<html>gateway</html>"))).toEqual({ kind: "unavailable", status: 200 })
  expect(await evaluateQuestion(question, async () => { throw new TypeError("network") })).toEqual({ kind: "unavailable", status: 0 })
})

test("a built question's records are traced with its lineage handle passed back exactly as received", async () => {
  const handle = "lin:v2/AbC+/=~opaque"
  const trace = `{"result":${answerText},"bucket":"2026-W28","contributions":[]}`
  const read = await traceQuestion(question, handle, "2026-W28", answer(200, trace))
  expect(calls.at(-1)).toEqual({ path: "/api/host/v1/questions:trace?orgId=org_demo&environmentId=analytics", body: { question, lineageRef: handle, bucket: "2026-W28" } })
  expect(read.kind).toBe("read")
})

test("a run answer renders like a published one, without a link to a published page", () => {
  const recorded = (name: string) => readDocument(text(name), "recorded")
  const read = { question: readDocument(JSON.stringify({ ...question, name: "studio.gold_yield" }), "live"), answer: readDocument(answerText, "live"), metrics: recorded("scrap-yield.metrics.json"), objectType: recorded("scrap-yield.object-type.json") } as Parameters<typeof AnswerView>[0]["read"]
  const html = renderToStaticMarkup(<AnswerView read={read} heading="h3" />)
  expect(html).toContain("<h3")
  expect(html).toContain(">Live<")
  expect(html).not.toContain('href="/questions/')
  expect(html).toContain("Every week")
})
