// The cell host's telemetry (apps/norn-cell-do/telemetry.mjs over otlp.mjs), against a fake
// cell object: OTEL-OFF-1 (nothing changes and nothing is sent without an endpoint),
// OTEL-NO-TENANT-1 (attributes are route templates, outcomes and codes, never ids or scope),
// OTEL-NEVER-FAILS-1 (a failed export never reaches the request) and OTEL-BOUNDED-1.

import { expect, test } from "bun:test"
import { Telemetry, exportConfig, refusalCode, routeOf, telemetry } from "../../../apps/norn-cell-do/otlp.mjs"
// @ts-expect-error: the host module is plain JavaScript without declarations.
import { instrument, kindOf, outcomeOf, syncFinished } from "../../../apps/norn-cell-do/telemetry.mjs"

const ON = { OTEL_EXPORTER_OTLP_ENDPOINT: "http://collector.test/api/norn/", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20abc,stream-name=norn", NORN_OTEL_INTERVAL_MS: "1000" }

type Sent = { url: string; headers: Record<string, string>; body: any }

function capture(): { sent: Sent[]; send: typeof fetch } {
  const sent: Sent[] = []
  const send = (async (url: string, init: RequestInit) => {
    sent.push({ url, headers: init.headers as Record<string, string>, body: JSON.parse(String(init.body)) })
    return new Response(null, { status: 200 })
  }) as unknown as typeof fetch
  return { sent, send }
}

/** A cell object whose fetch answers what the test names, with the hooks instrument() wraps. */
function fakeCell(answer: (request: Request) => Response | Promise<Response>) {
  class Cell {
    env: Record<string, string>
    ctx: { waitUntil(p: Promise<unknown>): void; getWebSockets(): unknown[] }
    waited: Promise<unknown>[] = []
    sockets: unknown[] = []
    armed: number | null = null
    published = 0
    constructor(env: Record<string, string>) {
      this.env = env
      this.ctx = { waitUntil: (p) => this.waited.push(p), getWebSockets: () => this.sockets }
    }
    async fetch(request: Request) { return answer(request) }
    publish() { this.published += 1 }
    async alarm(_info?: unknown) {}
  }
  instrument(Cell)
  return Cell
}

const scoped = (path: string, method = "GET", body?: string) =>
  new Request(`https://cell.test${path}?orgId=acme&environmentId=prod`, { method, body: body ?? null, headers: { "x-norn-cell": JSON.stringify(["acme", "prod"]) } })

const names = (body: any): string[] => body.resourceMetrics[0].scopeMetrics[0].metrics.map((m: any) => m.name)
const points = (body: any, name: string): any[] => {
  const metric = body.resourceMetrics[0].scopeMetrics[0].metrics.find((m: any) => m.name === name)
  return (metric?.sum ?? metric?.histogram)?.dataPoints ?? []
}
const attr = (point: any, key: string) => {
  const value = point.attributes.find((a: any) => a.key === key)?.value
  return value?.stringValue ?? value?.boolValue ?? value?.intValue
}

test("OTEL-OFF-1: without an endpoint the wrapped cell answers the same response and records nothing", async () => {
  expect(exportConfig({}, "x")).toBeNull()
  expect(exportConfig({ OTEL_EXPORTER_OTLP_ENDPOINT: "ftp://x" }, "x")).toBeNull()
  const reply = new Response("{}", { status: 200 })
  const Cell = fakeCell(() => reply)
  const cell = new Cell({})
  expect(await cell.fetch(scoped("/v1/definitions"))).toBe(reply)
  expect(cell.waited).toEqual([])
  expect(telemetry({}, "norn-cell")).toBeNull()
})

test("routes are templates: identifiers never become labels", () => {
  expect(routeOf("/v1/objects/shop.Sale/SKU-1/history")).toBe("/v1/objects/{}/{}/history")
  expect(routeOf("/v1/actions/acme.orders.approve")).toBe("/v1/actions/{}")
  expect(routeOf("/v1/decisions/dec_42:decide")).toBe("/v1/decisions/{}:decide")
  expect(routeOf("/v1/sources/erpnext.rest:sync")).toBe("/v1/sources/{}:sync")
  expect(routeOf("/v1/questions:evaluate")).toBe("/v1/questions:evaluate")
  expect(routeOf("/v1/cell/definitions:apply")).toBe("/v1/cell/definitions:apply")
  expect(routeOf("/v1/Acme Corp")).toBe("/v1/{}")
  expect(routeOf("/admin")).toBe("other")
  expect(kindOf("POST", "/v1/actions/{}")).toBe("action")
  expect(kindOf("POST", "/v1/questions:evaluate")).toBe("question")
  expect(kindOf("GET", "/v1/objects/{}")).toBe("read")
  expect(outcomeOf(202)).toBe("pending")
  expect(refusalCode('{"code":"authz.denied"}')).toBe("authz.denied")
  expect(refusalCode('{"code":"acme customer 42"}')).toBe("other")
  expect(refusalCode("<html>")).toBe("other")
})

test("a request, a refusal, a parked action and a commit are counted by route, outcome and code only", async () => {
  const Cell = fakeCell((request) => {
    const path = new URL(request.url).pathname
    if (path.startsWith("/v1/actions/")) return Response.json({ outcome: "pending", decision: { id: "dec_1" } }, { status: 202 })
    if (path.startsWith("/v1/decisions/")) return Response.json({ format: "norn/refusal@2", code: "decision.not_found" }, { status: 404 })
    return Response.json({ objects: [{ id: "SKU-1", owner: "pat@example.com" }] })
  })
  const cell = new Cell(ON)
  await cell.fetch(scoped("/v1/objects/shop.Sale/SKU-1"))
  await cell.fetch(scoped("/v1/actions/acme.orders.approve", "POST", "{}"))
  const refused = await cell.fetch(scoped("/v1/decisions/dec_1:decide", "POST", "{}"))
  // The caller still reads the refusal the cell wrote.
  expect((await refused.json()).code).toBe("decision.not_found")
  const t = telemetry(ON, "norn-cell")!
  const { sent, send } = capture()
  await t.flush(send)
  expect(sent.map((s) => s.url).sort()).toEqual(["http://collector.test/api/norn/v1/metrics", "http://collector.test/api/norn/v1/traces"])
  expect(sent[0]!.headers["Authorization"]).toBe("Basic abc")
  const metrics = sent.find((s) => s.url.endsWith("/v1/metrics"))!.body
  expect(names(metrics)).toEqual(expect.arrayContaining(["norn_cell_requests_total", "norn_cell_request_duration_ms", "norn_cell_refusals_total", "norn_cell_commit_duration_ms", "norn_cell_actions_total", "norn_cell_decisions_total"]))
  expect(attr(points(metrics, "norn_cell_actions_total")[0], "outcome")).toBe("pending")
  const refusal = points(metrics, "norn_cell_refusals_total")[0]
  expect([attr(refusal, "code"), attr(refusal, "route"), attr(refusal, "environment")]).toEqual(["decision.not_found", "/v1/decisions/{}:decide", "prod"])
  // OTEL-NO-TENANT-1: neither the org, an object id, nor anything from a body is exported.
  const everything = JSON.stringify(sent)
  for (const secret of ["acme", "SKU-1", "pat@example.com", "dec_1", "shop.Sale"]) expect(everything).not.toContain(secret)
})

test("OTEL-NEVER-FAILS-1: an export that throws or is refused never reaches the request", async () => {
  // A clock the test moves: the failure log is rate-limited to once a minute.
  let clock = 1_000_000
  const t = new Telemetry(exportConfig(ON, "norn-cell")!, () => clock)
  t.count("x_total", {})
  const errors: string[] = []
  const original = console.error
  console.error = (message: string) => errors.push(message)
  try {
    await t.flush((async () => { throw new TypeError("network down") }) as unknown as typeof fetch)
    t.count("x_total", {})
    clock += 61_000
    await t.flush((async () => new Response("no", { status: 401 })) as unknown as typeof fetch)
  } finally {
    console.error = original
  }
  expect(errors.some((e) => e.includes("failed"))).toBe(true)
  expect(errors.some((e) => e.includes("401"))).toBe(true)
  // A handler that throws still throws, and is recorded as an error.
  const Cell = fakeCell(() => { throw new Error("cell open refused") })
  const env = { ...ON }
  await expect(new Cell(env).fetch(scoped("/v1/definitions"))).rejects.toThrow("cell open refused")
  const { sent, send } = capture()
  await telemetry(env, "norn-cell")!.flush(send)
  const point = points(sent.find((s) => s.url.endsWith("/v1/metrics"))!.body, "norn_cell_requests_total")[0]
  expect(attr(point, "outcome")).toBe("error")
})

test("OTEL-BOUNDED-1: series and spans stop growing at their bound and the drop is counted", async () => {
  const t = new Telemetry(exportConfig(ON, "norn-cell")!)
  for (let i = 0; i < 2100; i++) t.count("y_total", { route: `/r${i}` })
  for (let i = 0; i < 1100; i++) t.span("s", {}, 0, 1)
  const { sent, send } = capture()
  await t.flush(send)
  const metrics = sent.find((s) => s.url.endsWith("/v1/metrics"))!.body
  expect(points(metrics, "y_total").length).toBe(2000)
  expect(points(metrics, "norn_otel_dropped_total")[0].asDouble).toBe(200)
  expect(sent.find((s) => s.url.endsWith("/v1/traces"))!.body.resourceSpans[0].scopeSpans[0].spans.length).toBe(1000)
})

test("a histogram exports delta buckets with its bounds", async () => {
  const t = new Telemetry(exportConfig(ON, "norn-cell")!)
  for (const ms of [3, 40, 40, 9000]) t.observe("lat_ms", { route: "/v1/x" }, ms)
  const { sent, send } = capture()
  await t.flush(send)
  const point = points(sent[0]!.body, "lat_ms")[0]
  expect(point.count).toBe("4")
  expect(point.sum).toBe(9083)
  expect(point.bucketCounts.length).toBe(point.explicitBounds.length + 1)
  expect(point.bucketCounts.map(Number).reduce((a: number, b: number) => a + b, 0)).toBe(4)
  // The next window starts empty (delta temporality).
  const again = capture()
  await t.flush(again.send)
  expect(again.sent).toEqual([])
})

test("sync, alarm lag and live fan-out are recorded from the hooks", async () => {
  const env = { ...ON }
  const Cell = fakeCell(() => new Response("{}"))
  const cell = new Cell(env)
  cell.sockets = [{}, {}, {}]
  cell.publish()
  expect(cell.published).toBe(1)
  cell.armed = Date.now() - 1500
  await cell.alarm({ retryCount: 0 })
  syncFinished(env, { outcome: "partial", rowsRead: 10, rowsLanded: 8, rowsQuarantined: 2, wallMs: 1200 })
  syncFinished(env, { outcome: "failed", error: ["source.unreachable", "the source did not answer"], wallMs: 5 })
  const { sent, send } = capture()
  await telemetry(env, "norn-cell")!.flush(send)
  const metrics = sent.find((s) => s.url.endsWith("/v1/metrics"))!.body
  expect(points(metrics, "norn_cell_live_fanout")[0].sum).toBe(3)
  expect(points(metrics, "norn_cell_alarm_lag_ms")[0].sum).toBeGreaterThanOrEqual(1500)
  expect(points(metrics, "norn_cell_sync_rows_quarantined_total").map((p: any) => p.asDouble)).toContain(2)
  expect(points(metrics, "norn_cell_sync_runs_total").map((p: any) => attr(p, "code"))).toContain("source.unreachable")
  expect(JSON.stringify(metrics)).not.toContain("did not answer")
})
