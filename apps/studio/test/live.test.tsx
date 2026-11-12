import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { readDocument } from "../src/lib/exact.ts"
import { amountTexts, hostRead, isTrace, liveAccess, liveAnswer, liveTrace, type Trace } from "../src/lib/live.ts"
import * as answerLib from "../src/lib/answer.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { LiveRecords, QuestionPage } = await import("../src/questions.tsx")

const answers = join(import.meta.dir, "../../../fixtures/demo/answers")
const text = (name: string) => readFileSync(join(answers, name), "utf8")
const respond = (status: number, body: string) => async (url: string) => url === "/api/me"
  ? Response.json({ organization: { id: "org_other" }, environmentId: "warehouse" })
  : new Response(body, { status, headers: { "content-type": "application/json" } })

// Stands in for the host: the engine's own answer document (the same NornQueryResult the host returns),
// and a trace in the host's NornNumericTrace shape over that answer's first bucket.
const answerText = text("scrap-yield.k1.answer.json")
const answer = JSON.parse(answerText) as { lineageRef: string; rows: Record<string, { members: Record<string, { version: number; amounts: number[] }> }> }
const [bucket = "", row] = Object.entries(answer.rows)[0] ?? []
const traceText = `{"result":${answerText},"bucket":${JSON.stringify(bucket)},"contributions":[${Object.entries(row?.members ?? {}).map(([key, { version }], index) =>
  `{"fact":{"key":${JSON.stringify(key)},"objectType":"demo.Conversion","version":${version}},"amounts":[237.501,242.426],"receiptHash":null,"factDigest":null,"source":{"status":"restricted"},"commitSeq":${index + 1},"recordedAtMs":1,"restricted":true,"values":null}`).join(",")}]}`

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
  await liveTrace("demo.yield_week", answer.lineageRef, bucket, async (url: string) => { if (url === "/api/me") return Response.json({ organization: { id: "org_other" }, environmentId: "warehouse" }); asked = url; return new Response(traceText) })
  const url = new URL(asked, "https://studio")
  expect(url.pathname).toBe("/api/host/v1/questions/demo.yield_week/trace")
  expect(Object.fromEntries(url.searchParams)).toEqual({ orgId: "org_other", environmentId: "warehouse", lineageRef: answer.lineageRef, bucket })
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

// D52 / DATA-VALUES-1: a restricted row's amounts are row Values; the host sends amounts: null to a reader without Values.
const withheldText = (shape: string) => `{"result":${answerText},"bucket":${JSON.stringify(bucket)},"contributions":[
  {"fact":{"key":"SYN-MELT-00-0","objectType":"demo.Conversion","version":1},${shape}"receiptHash":null,"factDigest":null,"source":{"status":"restricted"},"commitSeq":7,"recordedAtMs":1,"restricted":true,"values":null},
  {"fact":{"key":"SYN-MELT-00-1","objectType":"demo.Conversion","version":1},"amounts":[55.917,57.071],"receiptHash":null,"factDigest":"f","source":{"status":"resolved","provenance":{}},"commitSeq":8,"recordedAtMs":1,"restricted":false,"values":{}}]}`

test("a contribution the server withholds (amounts: null) has no amount; none is computed", () => {
  for (const shape of ['"amounts":null,']) {
    const read = readDocument<Trace>(withheldText(shape), "live")
    expect({ shape, texts: amountTexts(read, 0) }).toEqual({ shape, texts: null })
    expect(amountTexts(read, 1)).toEqual(["55.917", "57.071"])
  }
})

/** A node rendered inside a router: each traced record links to its own page. */
async function routed(node: ReactNode) {
  const router = createRouter({ routeTree: createRootRoute({ component: () => node }), history: createMemoryHistory({ initialEntries: ["/questions"] }) })
  await router.load()
  return renderToStaticMarkup(<RouterProvider router={router} />)
}

test("a withheld receipt hash (null) is a trace; any other non-string is not", () => {
  const trace = JSON.parse(withheldText('"amounts":null,')) as { contributions: { receiptHash: unknown }[] }
  expect(isTrace(trace)).toBe(true)
  trace.contributions[0]!.receiptHash = 7
  expect(isTrace(trace)).toBe(false)
})

test("the drawer shows Withheld for that row, its receipt hash withheld, and keeps its record and commit", async () => {
  const client = new QueryClient()
  const lineageRef = "lineage-1"
  client.setQueryData(["live-trace", "demo.yield_week", lineageRef, bucket], { kind: "read", read: readDocument<Trace>(withheldText('"amounts":null,'), "live") })
  const html = await routed(<QueryClientProvider client={client}><LiveRecords question="demo.yield_week" lineageRef={lineageRef} bucket={{ key: bucket, label: "Week", value: "0.98", totals: [], members: [] }} /></QueryClientProvider>)
  const rows = html.split("<tr").slice(2)
  expect(rows[0]).toContain("SYN-MELT-00-0")
  expect(rows[0]).toContain(">Withheld<")
  expect(rows[0]).toContain(">Receipt withheld<")
  expect(rows[0]).not.toContain("Copy ")
  expect(rows[0]).toContain("commit 7")
  expect(rows[0]).not.toMatch(/\d+\.\d+ \/ /)
  expect(rows[1]).toContain("55.917 / 57.071")
  // #1175: a restricted bucket withholds every member's receipt hash.
  expect(rows[1]).toContain(">Receipt withheld<")
  expect(rows[1]).not.toContain("Copy ")
})

test("a disclosed receipt hash can be copied", async () => {
  const client = new QueryClient()
  const lineageRef = "lineage-1"
  const trace = JSON.parse(withheldText('"amounts":null,')) as { contributions: { receiptHash: unknown }[] }
  trace.contributions = trace.contributions.slice(1)
  trace.contributions[0]!.receiptHash = "d5d7"
  client.setQueryData(["live-trace", "demo.yield_week", lineageRef, bucket], { kind: "read", read: readDocument<Trace>(JSON.stringify(trace), "live") })
  const html = await routed(<QueryClientProvider client={client}><LiveRecords question="demo.yield_week" lineageRef={lineageRef} bucket={{ key: bucket, label: "Week", value: "0.98", totals: [], members: [] }} /></QueryClientProvider>)
  const [row] = html.split("<tr").slice(2)
  expect(row).toContain("SYN-MELT-00-1")
  expect(row).toContain("Copy d5d7")
  expect(row).not.toContain(">Receipt withheld<")
})

test("an answer whose members' amounts are withheld still reads, its totals intact, each member Withheld", () => {
  const { buckets, contribution } = answerLib
  const withheld = JSON.parse(answerText) as { rows: Record<string, { members: Record<string, { amounts: unknown }> }> }
  for (const row of Object.values(withheld.rows)) for (const member of Object.values(row.members)) member.amounts = null
  const recorded = (name: string) => readDocument(text(name), "recorded").document
  const declared = { question: recorded("scrap-yield.question.json"), metrics: recorded("scrap-yield.metrics.json"), objectType: recorded("scrap-yield.object-type.json") } as Parameters<typeof buckets>[0]
  const plain = buckets(declared, { answer: readDocument(answerText, "live").document as never, numbers: readDocument(answerText, "live").numbers, at: "" })
  const source = JSON.stringify(withheld)
  const rows = buckets(declared, { answer: JSON.parse(source), numbers: readDocument(source, "live").numbers, at: "" })
  expect(rows.map(({ value, totals }) => ({ value, totals }))).toEqual(plain.map(({ value, totals }) => ({ value, totals })))
  expect(rows.every(({ members }) => members.every(({ amounts }) => amounts === null && contribution(amounts) === "Withheld"))).toBe(true)
})


test("live answers and traces follow the current server scope and never call a missing cell", async () => {
  let environmentId: string | null = "first"
  let orgId = "org_one"
  const paths: string[] = []
  const fetcher = async (url: string) => {
    if (url === "/api/me") return Response.json({ organization: { id: orgId }, environmentId })
    paths.push(url)
    return new Response(traceText)
  }
  await liveAnswer("demo.yield_week", fetcher)
  orgId = "org_two"; environmentId = "second"
  await liveTrace("demo.yield_week", "lin:/+", "row & value", fetcher)
  await liveAccess(fetcher)
  expect(Object.fromEntries(new URL(paths[0] ?? "", "https://studio").searchParams)).toEqual({ orgId: "org_one", environmentId: "first" })
  expect(Object.fromEntries(new URL(paths[1] ?? "", "https://studio").searchParams)).toEqual({ lineageRef: "lin:/+", bucket: "row & value", orgId: "org_two", environmentId: "second" })
  expect(Object.fromEntries(new URL(paths[2] ?? "", "https://studio").searchParams)).toEqual({ orgId: "org_two", environmentId: "second" })
  environmentId = null
  expect(await liveAccess(fetcher)).toEqual({ kind: "unavailable", status: 0, reason: "no-cell" })
  expect(await liveAnswer("demo.yield_week", fetcher)).toEqual({ kind: "unavailable", status: 0, reason: "no-cell" })
  expect(await liveTrace("demo.yield_week", "lin", "row", fetcher)).toEqual({ kind: "unavailable", status: 0, reason: "no-cell" })
  expect(paths).toHaveLength(3)
  expect(await liveAnswer("demo.yield_week", async () => new Response("", { status: 401 }))).toEqual({ kind: "unavailable", status: 401 })
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

test("a 200 that is JSON but not an access document is no live read either", async () => {
  const access = { principal: { subjectId: "u1", actorKind: "user", roles: ["admin"], roleTable: "roles" }, policy: "p", grantTable: "g", capabilities: Object.fromEntries(["population", "values", "sources"].map((name) => [name, { decision: "permit", rule: null }])) }
  expect((await liveAccess(respond(200, JSON.stringify(access)))).kind).toBe("read")
  for (const text of ["null", '{"error":"unavailable"}', JSON.stringify({ ...access, capabilities: { population: access.capabilities.population } }), JSON.stringify({ ...access, principal: { subjectId: "u1" } })]) {
    expect(await liveAccess(respond(200, text))).toEqual({ kind: "unavailable", status: 200 })
  }
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

test("a live answer and trace whose amounts the server withheld (amounts: null) still read live", async () => {
  const withheld = JSON.parse(answerText) as { rows: Record<string, { members: Record<string, { amounts: unknown }> }> }
  for (const row of Object.values(withheld.rows)) for (const member of Object.values(row.members)) member.amounts = null
  expect((await liveAnswer("demo.yield_week", respond(200, JSON.stringify(withheld)))).kind).toBe("read")
  expect((await liveTrace("demo.yield_week", answer.lineageRef, bucket, respond(200, withheldText('"amounts":null,')))).kind).toBe("read")
})

test("a live answer row whose value is null still reads live", async () => {
  const doc = JSON.parse(answerText) as { rows: Record<string, { value: unknown }> }
  for (const row of Object.values(doc.rows)) row.value = null
  expect((await liveAnswer("demo.yield_week", respond(200, JSON.stringify(doc)))).kind).toBe("read")
})

test("a bucket the server suppressed (D53) reads 'Suppressed: fewer than 3 records' in the table, chart and totals, never a blank or a 0", async () => {
  const { SUPPRESSED, buckets, shown } = answerLib
  const { chartFrame } = await import("../src/lib/chart.ts")
  // The cell's D53 shape: under k=3 for a reader lacking Values on any member; membership stays.
  const suppressed = JSON.parse(answerText) as { rows: Record<string, { value: unknown; totals: unknown[]; suppressed?: boolean; members: Record<string, unknown> }> }
  const [first = ""] = Object.keys(suppressed.rows)
  const kept = Object.keys(suppressed.rows[first]?.members ?? {})
  Object.assign(suppressed.rows[first] ?? {}, { suppressed: true, value: null, totals: [] })
  const source = JSON.stringify(suppressed)
  const recorded = (name: string) => readDocument(text(name), "recorded").document
  const declared = { question: recorded("scrap-yield.question.json"), metrics: recorded("scrap-yield.metrics.json"), objectType: recorded("scrap-yield.object-type.json") } as Parameters<typeof buckets>[0]
  const rows = buckets(declared, { answer: JSON.parse(source), numbers: readDocument(source, "live").numbers, at: "" })
  expect(rows[0]).toMatchObject({ key: first, suppressed: true, value: SUPPRESSED, totals: [] })
  expect(rows[0]?.members.map(({ id }) => id)).toEqual(kept)
  expect(shown(rows[0] ?? { value: "", totals: [] })).toBe(SUPPRESSED)
  expect(rows.slice(1).every(({ suppressed: s, value }) => !s && value !== SUPPRESSED)).toBe(true)
  const frame = chartFrame(rows)
  expect(frame?.unit).toBe("%")
  expect(frame?.points[0] as unknown).toEqual({ label: rows[0]?.label, position: null, shown: SUPPRESSED, exact: SUPPRESSED })
  expect(frame?.points.slice(1).every(({ position }) => typeof position === "number")).toBe(true)
  const html = await page({ kind: "read", read: readDocument(source, "live") })
  expect(html).toContain(SUPPRESSED)
  expect(html).not.toContain(" = Suppressed")
})
