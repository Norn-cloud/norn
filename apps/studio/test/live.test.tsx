import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { readDocument } from "../src/lib/exact.ts"
import { amountTexts, hostRead, liveAnswer, liveTrace, type Trace } from "../src/lib/live.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { QuestionPage } = await import("../src/questions.tsx")

const answers = join(import.meta.dir, "../../../fixtures/demo/answers")
const text = (name: string) => readFileSync(join(answers, name), "utf8")
const respond = (status: number, body: string) => async () => new Response(body, { status, headers: { "content-type": "application/json" } })

// Stands in for the host: the engine's own answer document (the same NornQueryResult the host returns),
// and a trace in the host's NornNumericTrace shape over that answer's first bucket.
const answerText = text("scrap-yield.k1.answer.json")
const answer = JSON.parse(answerText) as { lineageRef: string; rows: Record<string, { members: Record<string, { version: number; amounts: number[] }> }> }
const [bucket = "", row] = Object.entries(answer.rows)[0] ?? []
const traceText = `{"result":${answerText},"bucket":${JSON.stringify(bucket)},"contributions":[${Object.entries(row?.members ?? {}).map(([key, { version }], index) =>
  `{"fact":{"key":${JSON.stringify(key)},"objectType":"demo.Conversion","version":${version}},"amounts":[237.501,242.426],"receiptHash":"r${index}","factDigest":null,"source":{"status":"restricted"},"commitSeq":${index + 1},"recordedAtMs":1,"restricted":true,"values":null}`).join(",")}]}`

test("a 200 is the server's document, read live with its exact number texts", async () => {
  const read = await hostRead<Trace>("/v1/x", respond(200, traceText))
  if (read.kind !== "read") throw new Error(read.kind)
  expect(read.read.origin).toBe("live")
  expect(amountTexts(read.read, 0)).toEqual(["237.501", "242.426"])
  expect(read.read.document.contributions.every(({ restricted, values }) => restricted && values === null)).toBe(true)
})

test("the host's 403 is shown as its refusal; the Worker's own errors and a missing host are not live reads", async () => {
  const denied = await hostRead("/v1/x", respond(403, '{"code":"authz.denied","category":"authorization","message":"The policy does not permit Values for this principal.","explanation":"rule v-1"}'))
  expect(denied).toEqual({ kind: "refused", refusal: { code: "authz.denied", category: "authorization", message: "The policy does not permit Values for this principal.", explanation: "rule v-1" } })
  expect(await hostRead("/v1/x", respond(403, '{"error":"Cross-origin request refused."}'))).toEqual({ kind: "unavailable", status: 403 })
  expect(await hostRead("/v1/x", respond(503, '{"error":"No Norn host is connected to this deployment."}'))).toEqual({ kind: "unavailable", status: 503 })
})

test("the trace is asked for by the answer's lineage and the row key exactly as the server wrote it", async () => {
  let asked = ""
  await liveTrace("demo.yield_week", answer.lineageRef, bucket, async (url: string) => { asked = url; return new Response(traceText) })
  const url = new URL(asked, "https://studio")
  expect(url.pathname).toBe("/api/host/v1/questions/demo.yield_week/trace")
  expect(Object.fromEntries(url.searchParams)).toEqual({ orgId: "org_demo", environmentId: "analytics", lineageRef: answer.lineageRef, bucket })
})

/** The question page with the recorded board and a given live read already answered. */
async function page(live: unknown) {
  const client = new QueryClient()
  const recorded = (name: string) => readDocument(text(name), "recorded")
  client.setQueryData(["question-board"], [{ question: recorded("scrap-yield.question.json"), answer: recorded("scrap-yield.k1.answer.json"), metrics: recorded("scrap-yield.metrics.json"), objectType: recorded("scrap-yield.object-type.json") }])
  client.setQueryData(["live-answer", "demo.yield_week"], live)
  const root = createRootRoute()
  const router = createRouter({ routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/questions/$name", component: QuestionPage })]), history: createMemoryHistory({ initialEntries: ["/questions/demo.yield_week"] }) })
  await router.load()
  return renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
}

test("the page says where its answer came from: live from the cell, or the recorded snapshot", async () => {
  expect(await page({ kind: "read", read: readDocument(answerText, "live") })).toContain(">Live<")
  const recorded = await page({ kind: "unavailable", status: 503 })
  expect(recorded).toContain(">Recorded snapshot<")
  expect(recorded).not.toContain(">Live<")
})

test("a refused live read shows the server's reason over the recorded snapshot", async () => {
  const html = await page({ kind: "refused", refusal: { code: "authz.denied", message: "The policy does not permit Population for this principal." } })
  expect(html).toContain("The policy does not permit Population for this principal.")
  expect(html).toContain(">Refused:<")
  expect(html).toContain("authz.denied")
  expect(html).toContain(">Recorded snapshot<")
})

test("a network failure or a 200 that is not JSON is no live read, so the page keeps its recording", async () => {
  expect(await hostRead("/v1/x", async () => { throw new TypeError("network") })).toEqual({ kind: "unavailable", status: 0 })
  expect(await hostRead("/v1/x", respond(200, "<html>gateway</html>"))).toEqual({ kind: "unavailable", status: 200 })
})

test("a 200 that is JSON but not an answer document is no live read, so the page keeps its recording", async () => {
  expect((await liveAnswer("demo.yield_week", respond(200, answerText))).kind).toBe("read")
  expect(await liveAnswer("demo.yield_week", respond(200, "null"))).toEqual({ kind: "unavailable", status: 200 })
  expect(await liveAnswer("demo.yield_week", respond(200, '{"error":"unavailable"}'))).toEqual({ kind: "unavailable", status: 200 })
})

test("a live answer with a malformed row, watermark or freshness is no live read, so the page keeps its recording", async () => {
  const broken = (change: (doc: Record<string, unknown>) => void) => { const doc = JSON.parse(answerText) as Record<string, unknown>; change(doc); return JSON.stringify(doc) }
  const first = (doc: Record<string, unknown>) => Object.keys(doc.rows as object)[0] ?? ""
  for (const body of [
    broken((doc) => { (doc.rows as Record<string, unknown>)[first(doc)] = null }),
    broken((doc) => { delete ((doc.rows as Record<string, Record<string, unknown>>)[first(doc)] ?? {}).totals }),
    broken((doc) => { delete ((doc.rows as Record<string, Record<string, unknown>>)[first(doc)] ?? {}).value }),
    broken((doc) => { ((doc.rows as Record<string, Record<string, unknown>>)[first(doc)] ?? {}).value = "0.98" }),
    broken((doc) => { ((doc.rows as Record<string, Record<string, unknown>>)[first(doc)] ?? {}).members = { x: { version: 1, amounts: "1" } } }),
    broken((doc) => { doc.watermarks = { measurements: { observed: null } } }),
    broken((doc) => { doc.freshness = "unheard-of" }),
  ]) expect(await liveAnswer("demo.yield_week", respond(200, body))).toEqual({ kind: "unavailable", status: 200 })
})

test("a live trace that is not the drawer's document is no live read, so the drawer says it could not read the records", async () => {
  expect((await liveTrace("demo.yield_week", answer.lineageRef, bucket, respond(200, traceText))).kind).toBe("read")
  for (const body of ["null", '{"error":"unavailable"}', `{"bucket":${JSON.stringify(bucket)}}`, `{"bucket":${JSON.stringify(bucket)},"contributions":[null]}`, `{"bucket":${JSON.stringify(bucket)},"contributions":[{"fact":{"key":"k"},"amounts":[1]}]}`])
    expect(await liveTrace("demo.yield_week", answer.lineageRef, bucket, respond(200, body))).toEqual({ kind: "unavailable", status: 200 })
})

test("a live answer row whose value is null still reads live", async () => {
  const doc = JSON.parse(answerText) as { rows: Record<string, { value: unknown }> }
  for (const row of Object.values(doc.rows)) row.value = null
  expect((await liveAnswer("demo.yield_week", respond(200, JSON.stringify(doc)))).kind).toBe("read")
})
