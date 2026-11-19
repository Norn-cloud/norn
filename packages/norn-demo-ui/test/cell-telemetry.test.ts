// The cell host's telemetry (apps/norn-cell-do/telemetry.mjs over otlp.mjs), against a fake
// cell object: OTEL-OFF-1 (nothing changes and nothing is sent without an endpoint),
// OTEL-NO-TENANT-1 (attributes and span names come from fixed tables, never from the path,
// scope or a body), OTEL-NEVER-FAILS-1 (telemetry never delays, replaces or fails a handler's
// result), OTEL-TLS-1 (credentials only over HTTPS or loopback) and OTEL-BOUNDED-1.

import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { CELL_ROUTES, Telemetry, cellRoute, exportConfig, methodOf, telemetry } from "../../../apps/norn-cell-do/otlp.mjs"
// @ts-expect-error: the host module is plain JavaScript without declarations.
import { instrument, kindOf, noteRefusal, outcomeOf, refusalCode, refusalCodeOf, syncFinished } from "../../../apps/norn-cell-do/telemetry.mjs"
// @ts-expect-error: the host module is plain JavaScript without declarations.
import * as live from "../../../apps/norn-cell-do/subscribe.mjs"

const ON = { OTEL_EXPORTER_OTLP_ENDPOINT: "https://collector.test/api/norn/", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20abc,stream-name=norn", NORN_OTEL_INTERVAL_MS: "1000" }

type Sent = { url: string; headers: Record<string, string>; body: any }

function capture(): { sent: Sent[]; send: typeof fetch } {
  const sent: Sent[] = []
  const send = (async (url: string, init: RequestInit) => {
    sent.push({ url, headers: init.headers as Record<string, string>, body: JSON.parse(String(init.body)) })
    return new Response(null, { status: 200 })
  }) as unknown as typeof fetch
  return { sent, send }
}

/** A cell object whose fetch answers what the test names, with the hooks instrument() wraps. Refusals are noted as worker.mjs notes them. */
function fakeCell(answer: (request: Request) => Response | Promise<Response>) {
  class Cell {
    env: Record<string, unknown>
    ctx: { waitUntil(p: Promise<unknown>): void; getWebSockets(): unknown[] }
    waited: Promise<unknown>[] = []
    sockets: unknown[] = []
    armed: number | null = null
    published = 0
    snapshots = 0
    constructor(env: Record<string, unknown>) {
      this.env = env
      this.ctx = { waitUntil: (p) => this.waited.push(p), getWebSockets: () => this.sockets }
    }
    async fetch(request: Request) {
      const response = await answer(request)
      return response.status >= 400 ? noteRefusal(this.env, response, await response.clone().text()) : response
    }
    publish() {
      this.published += 1
      return Promise.resolve(this.snapshots)
    }
    async alarm(_info?: unknown) {}
  }
  instrument(Cell)
  return Cell
}

/** Every background task the cell handed to waitUntil, settled. */
const settled = async (cell: { waited: Promise<unknown>[] }) => {
  for (let i = 0; i < cell.waited.length; i++) await cell.waited[i]
}

const TENANT = ["acme-corp", "acme", "globex_prod", "tenant-slug"]
const scoped = (path: string, method = "GET", body?: string) =>
  new Request(`https://cell.test${path}?orgId=acme-corp&environmentId=prod`, { method, body: body ?? null, headers: { "x-norn-cell": JSON.stringify(["acme-corp", "prod"]) } })

const metricList = (body: any): any[] => body.resourceMetrics[0].scopeMetrics[0].metrics
const names = (body: any): string[] => metricList(body).map((m: any) => m.name)
/** Every data point of `name` in a body, across however many metrics carry it. */
const points = (body: any, name: string): any[] =>
  metricList(body).filter((m: any) => m.name === name).flatMap((m: any) => (m.sum ?? m.histogram).dataPoints)
const attr = (point: any, key: string) => {
  const value = point.attributes.find((a: any) => a.key === key)?.value
  return value?.stringValue ?? value?.boolValue ?? value?.intValue
}
/** Every batch's metrics as one body: the background flush and a test's own flush may each have sent one. */
const metricsOf = (sent: Sent[]) => ({ resourceMetrics: [{ scopeMetrics: [{ metrics: sent.filter((s) => s.url.endsWith("/v1/metrics")).flatMap((s) => metricList(s.body)) }] }] })
const spansOf = (sent: Sent[]) => sent.filter((s) => s.url.endsWith("/v1/traces")).flatMap((s) => s.body.resourceSpans[0].scopeSpans[0].spans)

/** The env's telemetry sending to a capture from the start, so the background flush is seen too. */
function collected(env: Record<string, unknown>) {
  const t = telemetry(env, "norn-cell")!
  const { sent, send } = capture()
  t.send = send
  return { t, sent, all: async () => { await t.flush(); return sent } }
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

test("routes come from the fixed table: every template the host serves, filled with tenant names, maps back to itself", () => {
  for (const template of CELL_ROUTES) {
    for (const slug of TENANT) {
      const path = template.replace(/\{[a-z]+\}/g, slug)
      expect(cellRoute(path)).toBe(template)
      expect(cellRoute(path)).not.toContain(slug)
    }
  }
  // Unknown routes, lowercase words included, are one fallback: no segment survives.
  for (const path of ["/v1/acme-corp", "/v1/acme", "/v1/objects/a/b/acme", "/v1/sources/acme:acme", "/v1/definitions/acme:acme/x", "/v1/actions/", "/admin", "/v1/acme:sync", "", "v1/x"]) {
    expect(cellRoute(path)).toBe("other")
  }
  expect(methodOf("GET")).toBe("GET")
  expect(methodOf("ACME")).toBe("other")
  expect(kindOf("POST", "/v1/actions/{action}")).toBe("action")
  expect(kindOf("POST", "/v1/questions:evaluate")).toBe("question")
  expect(kindOf("POST", "/v1/sources/{source}:sync")).toBe("sync")
  expect(kindOf("GET", "/v1/objects/{type}")).toBe("read")
  expect(kindOf("POST", "other")).toBe("other")
  expect(outcomeOf(202)).toBe("pending")
})

test("the route table covers every route the cell server declares (server.rs routes!)", () => {
  const server = readFileSync(new URL("../../../crates/norn-cell-do/src/server.rs", import.meta.url), "utf8")
  const declared = [...server.matchAll(/=> \("(?:GET|POST)", "(\/v1\/[^"]+)"\)/g)].map((m) => m[1]!.replace(/\{[A-Za-z]+\}/g, "{}"))
  expect(declared.length).toBeGreaterThan(40)
  const ours = new Set(CELL_ROUTES.map((template) => template.replace(/\{[a-z]+\}/g, "{}")))
  for (const path of declared) expect(ours.has(path)).toBe(true)
})

test("refusal codes are allowlisted: a known code is kept, anything else is other", () => {
  expect(refusalCode('{"code":"authz.denied"}')).toBe("authz.denied")
  expect(refusalCode('{"code":"action.write_mode_failed"}')).toBe("action.write_mode_failed")
  expect(refusalCode('{"code":"acme.customer_42"}')).toBe("other")
  expect(refusalCode('{"code":"acme customer 42"}')).toBe("other")
  expect(refusalCode("<html>")).toBe("other")
  expect(refusalCode(`{"code":"authz.denied","pad":"${"x".repeat(9000)}"}`)).toBe("other")
})

test("the refusal allowlist holds every refusal-family code the host and kernel write", () => {
  const root = new URL("../../../", import.meta.url)
  const families = /^(action|analytics|approval|artifact_set|authn|authz|bulk|cell|commit|connector|dataset|decision|definition|entitlement|guard|ingress|lifecycle|live|object|observations|query|question|request|reveal|route|run|scope|source|subscription|transform|upload)\./
  const sources = ["crates/norn-kernel/src", "crates/norn-cell-do/src", "crates/norn-cell/src", "crates/norn-api/src", "crates/norn-authz/src", "crates/norn-decisions/src"]
    .flatMap((dir) => readdirSync(new URL(dir, root), { recursive: true, encoding: "utf8" }).filter((file) => file.endsWith(".rs") && !file.includes("tests")).map((file) => new URL(`${dir}/${file}`, root)))
  sources.push(...["worker.mjs", "subscribe.mjs"].map((file) => new URL(`apps/norn-cell-do/${file}`, root)))
  const written = new Set(sources.flatMap((file) => [...readFileSync(file, "utf8").matchAll(/"([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+)"/g)].map((m) => m[1]!)).filter((code) => families.test(code)))
  expect(written.size).toBeGreaterThan(250)
  expect([...written].filter((code) => refusalCodeOf(code) === "other")).toEqual([])
})

test("OTEL-NO-TENANT-1: lowercase slugs, ids and colon suffixes never reach an attribute or span name, refused requests included", async () => {
  const Cell = fakeCell((request) => {
    const path = new URL(request.url).pathname
    if (path.startsWith("/v1/actions/")) return Response.json({ outcome: "pending", decision: { id: "acme-corp" } }, { status: 202 })
    if (path.startsWith("/v1/decisions/")) return Response.json({ format: "norn/refusal@2", code: "decision.not_found", message: "acme-corp" }, { status: 404 })
    if (path.startsWith("/v1/objects/")) return Response.json({ objects: [{ id: "acme", owner: "pat@example.com" }] })
    return Response.json({ format: "norn/refusal@2", code: "globex_prod.secret", message: "tenant-slug" }, { status: 404 })
  })
  const env = { ...ON }
  const out = collected(env)
  const cell = new Cell(env)
  await cell.fetch(scoped("/v1/objects/acme/acme-corp"))
  await cell.fetch(scoped("/v1/actions/acme", "POST", "{}"))
  const refused = await cell.fetch(scoped("/v1/decisions/acme-corp:decide", "POST", "{}"))
  // The caller still reads the refusal the cell wrote.
  expect((await refused.json()).code).toBe("decision.not_found")
  await cell.fetch(scoped("/v1/acme-corp/globex_prod:tenant-slug", "POST", "{}"))
  await cell.fetch(scoped("/v1/acme-corp"))
  await settled(cell)
  const sent = await out.all()
  expect([...new Set(sent.map((s) => s.url))].sort()).toEqual(["https://collector.test/api/norn/v1/metrics", "https://collector.test/api/norn/v1/traces"])
  expect(sent[0]!.headers["Authorization"]).toBe("Basic abc")
  const metrics = metricsOf(sent)
  expect(names(metrics)).toEqual(expect.arrayContaining(["norn_cell_requests_total", "norn_cell_request_duration_ms", "norn_cell_refusals_total", "norn_cell_actions_total", "norn_cell_decisions_total"]))
  expect(names(metrics)).not.toContain("norn_cell_commit_duration_ms")
  expect(attr(points(metrics, "norn_cell_actions_total")[0], "outcome")).toBe("pending")
  const refusals = points(metrics, "norn_cell_refusals_total").map((p) => [attr(p, "code"), attr(p, "route"), attr(p, "environment")]).sort()
  expect(refusals).toEqual([["decision.not_found", "/v1/decisions/{decision}:decide", "prod"], ["other", "other", "prod"], ["other", "other", "prod"]])
  expect(points(metrics, "norn_cell_requests_total").map((p) => attr(p, "method")).sort()).toEqual(["GET", "GET", "POST", "POST", "POST"])
  const spanNames = spansOf(sent).map((s: any) => s.name).sort()
  expect(spanNames).toEqual(["cell action /v1/actions/{action}", "cell decision /v1/decisions/{decision}:decide", "cell other other", "cell read /v1/objects/{type}/{id}", "cell read other"])
  const everything = JSON.stringify(sent)
  for (const secret of [...TENANT, "pat@example.com", "secret"]) expect(everything).not.toContain(secret)
})

test("an environment outside the configured stages is other", async () => {
  const env = { ...ON }
  const out = collected(env)
  const Cell = fakeCell(() => new Response("{}"))
  const cell = new Cell(env)
  await cell.fetch(new Request("https://cell.test/v1/definitions", { headers: { "x-norn-cell": JSON.stringify(["acme", "acme-staging"]) } }))
  await settled(cell)
  const sent = await out.all()
  expect(points(metricsOf(sent), "norn_cell_requests_total").map((p) => attr(p, "environment"))).toEqual(["other"])
  expect(JSON.stringify(sent)).not.toContain("acme")
})

test("OTEL-NEVER-FAILS-1: a slow or failed collector never delays or changes the handler's result", async () => {
  const env = { ...ON, NORN_OTEL_INTERVAL_MS: "1000" }
  const reply = new Response("{}")
  const Cell = fakeCell(() => reply)
  const cell = new Cell(env)
  const t = telemetry(env, "norn-cell")!
  // A collector that never answers.
  t.send = (() => new Promise(() => {})) as unknown as typeof fetch
  const started = Date.now()
  expect(await cell.fetch(scoped("/v1/definitions"))).toBe(reply)
  expect(await cell.fetch(scoped("/v1/definitions"))).toBe(reply)
  expect(Date.now() - started).toBeLessThan(200)
  expect(cell.waited.length).toBe(2)
  // A collector that fails: the background task still settles, quietly.
  const env2 = { ...ON }
  const cell2 = new (fakeCell(() => reply))(env2)
  telemetry(env2, "norn-cell")!.send = (async () => { throw new TypeError("Bearer s3cret") }) as unknown as typeof fetch
  const errors: string[] = []
  const original = console.error
  console.error = (message: string) => errors.push(message)
  try {
    expect(await cell2.fetch(scoped("/v1/definitions"))).toBe(reply)
    await settled(cell2)
  } finally {
    console.error = original
  }
  expect(errors.join("\n")).toContain("failed: error")
  expect(errors.join("\n")).not.toContain("s3cret")
})

test("OTEL-NEVER-FAILS-1: a hook that throws never reaches the handler, the alarm or the sync", async () => {
  const env = { ...ON }
  const reply = new Response("{}")
  const Cell = fakeCell(() => reply)
  const cell = new Cell(env)
  const t = telemetry(env, "norn-cell")!
  t.count = () => { throw new Error("count broke") }
  t.observe = () => { throw new Error("observe broke") }
  t.span = () => { throw new Error("span broke") }
  t.pump = () => { throw new Error("pump broke") }
  t.log = () => {}
  expect(await cell.fetch(scoped("/v1/definitions"))).toBe(reply)
  await expect(cell.alarm({ retryCount: 0 })).resolves.toBeUndefined()
  expect(() => syncFinished(env, { outcome: "ok", wallMs: 3 }, cell.ctx)).not.toThrow()
  cell.sockets = [{}]
  cell.snapshots = 1
  await expect(cell.publish()).resolves.toBe(1)
  await Promise.allSettled(cell.waited)
  // A handler that throws still throws its own error.
  const Broken = fakeCell(() => { throw new Error("cell open refused") })
  await expect(new Broken(env).fetch(scoped("/v1/definitions"))).rejects.toThrow("cell open refused")
})

test("OTEL-NEVER-FAILS-1: bad configuration turns export off instead of throwing", async () => {
  const bad: Record<string, unknown>[] = [
    { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api", OTEL_SERVICE_NAME: 42, NORN_OTEL_INTERVAL_MS: "1000" },
    { OTEL_EXPORTER_OTLP_ENDPOINT: 42 },
    { OTEL_EXPORTER_OTLP_ENDPOINT: "not a url" },
    { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Bearer a%0Ab" },
    { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=%E0%A4%A" },
    { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api", OTEL_EXPORTER_OTLP_HEADERS: "Bad Name=x" },
    { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api", OTEL_EXPORTER_OTLP_HEADERS: 7 },
  ]
  // A numeric service name and a non-text sampler are ignored, not thrown on; every other bad setting turns export off.
  expect(exportConfig(bad[0], "norn-cell")!.service).toBe("norn-cell")
  expect(exportConfig({ OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api", OTEL_TRACES_SAMPLER_ARG: Symbol("x") }, "norn-cell")!.sampleRatio).toBe(1)
  for (const env of bad.slice(1)) expect(telemetry(env, "norn-cell")).toBeNull()
  const reply = new Response("{}")
  const Cell = fakeCell(() => reply)
  telemetry(bad[0], "norn-cell")!.send = capture().send
  for (const env of bad) {
    const cell = new Cell(env)
    expect(await cell.fetch(scoped("/v1/definitions"))).toBe(reply)
    await Promise.allSettled(cell.waited)
  }
})

test("OTEL-TLS-1: credentials travel only over HTTPS or to loopback", () => {
  const credentialed = (endpoint: string) => exportConfig({ OTEL_EXPORTER_OTLP_ENDPOINT: endpoint, OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x" }, "s")
  expect(credentialed("http://o2.example.com/api/norn")).toBeNull()
  expect(credentialed("http://10.0.0.5:5080/api/norn")).toBeNull()
  expect(credentialed("https://user:pw@o2.example.com/api")).toBeNull()
  expect(credentialed("https://o2.example.com/api/norn")).not.toBeNull()
  expect(credentialed("http://127.0.0.1:5080/api/norn")).not.toBeNull()
  expect(credentialed("http://localhost:5080/api/norn")).not.toBeNull()
  expect(credentialed("http://[::1]:5080/api/norn")).not.toBeNull()
  // Without credentials, plain HTTP is allowed.
  expect(exportConfig({ OTEL_EXPORTER_OTLP_ENDPOINT: "http://o2.example.com/api" }, "s")).not.toBeNull()
})

test("a quiet isolate exports its first observation after one bounded wait", async () => {
  const env = { ...ON, NORN_OTEL_INTERVAL_MS: "1000" }
  const Cell = fakeCell(() => new Response("{}"))
  const cell = new Cell(env)
  const { sent, send } = capture()
  telemetry(env, "norn-cell")!.send = send
  const started = Date.now()
  await cell.fetch(scoped("/v1/definitions"))
  // One event, then nothing: the event's own background task flushes it.
  await settled(cell)
  expect(Date.now() - started).toBeLessThan(3000)
  expect(points(metricsOf(sent), "norn_cell_requests_total").length).toBe(1)
})

test("OTEL-BOUNDED-1: series and spans stop growing at their bound and the drop is counted", async () => {
  const t = new Telemetry(exportConfig(ON, "norn-cell")!)
  for (let i = 0; i < 2100; i++) t.count("y_total", { route: `/r${i}` })
  for (let i = 0; i < 1100; i++) t.span("s", {}, 0, 1)
  const { sent, send } = capture()
  await t.flush(send)
  const metrics = metricsOf(sent)
  expect(points(metrics, "y_total").length).toBe(2000)
  expect(points(metrics, "norn_otel_dropped_total")[0].asDouble).toBe(200)
  expect(spansOf(sent).length).toBe(1000)
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

test("sync, alarm lag, and live publishes with the snapshots they sent are recorded from the hooks", async () => {
  const env = { ...ON }
  const out = collected(env)
  const Cell = fakeCell(() => new Response("{}"))
  const cell = new Cell(env)
  cell.sockets = [{}, {}, {}]
  cell.snapshots = 2
  expect(await cell.publish()).toBe(2)
  expect(cell.published).toBe(1)
  cell.armed = Date.now() - 1500
  await cell.alarm({ retryCount: 0 })
  syncFinished(env, { outcome: "partial", rowsRead: 10, rowsLanded: 8, rowsQuarantined: 2, wallMs: 1200 }, cell.ctx)
  syncFinished(env, { outcome: "failed", error: ["source.unreachable", "the source did not answer"], wallMs: 5 }, cell.ctx)
  syncFinished(env, { outcome: "failed", error: ["source.http_status", "acme said no"], wallMs: 5 }, cell.ctx)
  // The publish's record runs once its promise settles; wait for its background task.
  await new Promise((resolve) => setTimeout(resolve, 5))
  await settled(cell)
  const metrics = metricsOf(await out.all())
  expect(points(metrics, "norn_cell_live_fanout")[0].sum).toBe(3)
  expect(points(metrics, "norn_cell_live_snapshots_total")[0].asDouble).toBe(2)
  expect(points(metrics, "norn_cell_alarm_lag_ms")[0].sum).toBeGreaterThanOrEqual(1500)
  expect(points(metrics, "norn_cell_sync_rows_quarantined_total").map((p: any) => p.asDouble)).toContain(2)
  expect(points(metrics, "norn_cell_sync_runs_total").map((p: any) => attr(p, "code")).sort()).toEqual(["", "other", "source.http_status"])
  expect(JSON.stringify(metrics)).not.toContain("did not answer")
  expect(JSON.stringify(metrics)).not.toContain("acme")
})

test("B2: a finished sync records nothing on the caller's path, only in the background task it hands to waitUntil", async () => {
  const env = { ...ON }
  const out = collected(env)
  const Cell = fakeCell(() => new Response("{}"))
  const cell = new Cell(env)
  const record = { outcome: "ok", rowsRead: 4, rowsLanded: 4, rowsQuarantined: 0, wallMs: 30 }
  syncFinished(env, record, cell.ctx)
  // Returned without recording: nothing is buffered yet, and one task waits on the event.
  expect(out.t.buffered()).toBe(false)
  expect(cell.waited.length).toBe(1)
  // The caller's record changing afterwards does not change what is recorded.
  record.rowsLanded = 999
  await settled(cell)
  const metrics = metricsOf(await out.all())
  expect(points(metrics, "norn_cell_sync_rows_landed_total")[0].asDouble).toBe(4)
})

test("S2: the snapshot counter counts only the sends a socket took", async () => {
  // Two subscribed sockets; one throws on send (closing under us).
  const socket = (fails: boolean) => {
    let attachment: any = { scope: ["o", "e"], authorization: "Bearer x", subs: { a: { pathname: "/v1/decisions", query: "", digest: "" } } }
    return {
      serializeAttachment: (v: unknown) => { attachment = structuredClone(v) },
      deserializeAttachment: () => structuredClone(attachment),
      send: () => { if (fails) throw new Error("closed") },
      close() {},
    }
  }
  const ask = () => ({ status: 200, body: "{}" })
  expect(await live.publish({ getWebSockets: () => [socket(false), socket(true)] }, ask)).toBe(1)
  expect(await live.publish({ getWebSockets: () => [socket(true)] }, ask)).toBe(0)
})

test("B1: the resource carries only allowlisted keys with allowlisted values, and a service name from the fixed set", async () => {
  const env = { ...ON, OTEL_SERVICE_NAME: "acme-corp", OTEL_RESOURCE_ATTRIBUTES: "deployment.environment=prod,tenant=acme-corp,host.name=acme.cells.example,deployment.environment.name=acme-corp,service.namespace=globex" }
  const config = exportConfig(env, "norn-cell")!
  expect(config.service).toBe("norn-cell")
  expect(config.resource).toEqual([["deployment.environment", "prod"]])
  // A service name outside the set falls back to the host's; a listed one is kept.
  expect(exportConfig({ ...ON, OTEL_SERVICE_NAME: "norn-studio" }, "norn-cell")!.service).toBe("norn-studio")
  expect(exportConfig(ON, "acme-service")!.service).toBe("norn")
  // An environment value outside the stages is dropped, not exported.
  expect(exportConfig({ ...ON, OTEL_RESOURCE_ATTRIBUTES: "deployment.environment=acme-pilot" }, "norn-cell")!.resource).toEqual([])
  const t = new Telemetry(config)
  t.count("x_total", {})
  t.span("s", {}, 0, 1)
  const { sent, send } = capture()
  await t.flush(send)
  const text = JSON.stringify(sent.map((s) => s.body))
  for (const leaked of ["acme", "globex", "cells.example", "tenant", "host.name"]) expect(text).not.toContain(leaked)
  const keys = sent[0]!.body.resourceMetrics[0].resource.attributes.map((a: any) => a.key).sort()
  expect(keys).toEqual(["deployment.environment", "service.instance.id", "service.name", "telemetry.sdk.language", "telemetry.sdk.name"])
})
