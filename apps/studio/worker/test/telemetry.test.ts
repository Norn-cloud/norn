import { expect, test } from "bun:test"
import { telemetry } from "../../../norn-cell-do/otlp.mjs"
import { otelSettings } from "../../deploy-target.ts"
import { STUDIO_ROUTES, errorClass, instrumented, studioRoute } from "../telemetry.ts"

// Studio's request telemetry: OTEL-OFF-1 (no endpoint, no change), routes from a fixed table,
// error classes, OTEL-NO-TENANT-1 (no path segment, id, slug or email leaves in an attribute or a
// span name) and OTEL-NEVER-FAILS-1 (telemetry never delays or changes the handler's result).

const TENANT = ["acme-corp", "acme", "globex_prod", "pat@example.com"]

type Ctx = ExecutionContext & { waited: Promise<unknown>[] }
const context = (): Ctx => {
  const waited: Promise<unknown>[] = []
  return { waited, waitUntil: (p: Promise<unknown>) => waited.push(p), passThroughOnException() {} } as unknown as Ctx
}
const settled = async (ctx: Ctx) => {
  for (let i = 0; i < ctx.waited.length; i++) await ctx.waited[i]
}
const capture = () => {
  const sent: string[] = []
  const send = (async (_url: string, init: RequestInit) => {
    sent.push(String(init.body))
    return new Response(null)
  }) as unknown as typeof fetch
  return { sent, send }
}

test("routes come from the fixed table: every template, filled with tenant names, maps back to itself", () => {
  for (const template of STUDIO_ROUTES) {
    if (template.endsWith("*")) continue
    for (const slug of TENANT) expect(studioRoute(template.replace(/\{[a-z]+\}/g, encodeURIComponent(slug)))).toBe(template)
  }
  expect(studioRoute("/")).toBe("static")
  expect(studioRoute("/assets/index-3f2a.js")).toBe("static")
  expect(studioRoute("/mcp")).toBe("/mcp")
  expect(studioRoute("/.well-known/oauth-authorization-server")).toBe("/.well-known/*")
  expect(studioRoute("/api/auth/sign-in/email")).toBe("/api/auth/sign-in/email")
  expect(studioRoute("/api/auth/acme-corp/callback")).toBe("/api/auth/*")
  expect(studioRoute("/api/host/v1/objects/shop.Sale/SKU-1")).toBe("/api/host/v1/objects/{type}/{id}")
  expect(studioRoute("/api/agent/dashboards/prop_8c1f2/accept")).toBe("/api/agent/dashboards/{proposal}/accept")
  // Unknown API paths, lowercase words included, are one fallback.
  for (const path of ["/api/acme-corp", "/api/connectors/acme/x/y/z", "/api/host/v1/acme-corp", "/api/host/acme", "/api/agent/acme", "/api/questions/acme"]) expect(studioRoute(path)).toBe("other")
})

test("auth, billing, spend-cap, quota, rate-limit and server errors are told apart", () => {
  expect(errorClass(401)).toBe("auth")
  expect(errorClass(403)).toBe("auth")
  expect(errorClass(402)).toBe("billing")
  // The assistant's spend cap is its own class, never billing.
  expect(errorClass(429, "spend_cap")).toBe("spend_cap")
  expect(errorClass(429, "proposals_hourly")).toBe("quota")
  expect(errorClass(429)).toBe("rate_limit")
  expect(errorClass(429, "acme-corp")).toBe("rate_limit")
  expect(errorClass(429, "constructor")).toBe("rate_limit")
  expect(errorClass(503)).toBe("server")
  expect(errorClass(404)).toBeNull()
})

test("OTEL-OFF-1: without an endpoint the handler's own response passes through", async () => {
  const reply = new Response("ok")
  const handler = instrumented({ fetch: async () => reply })
  const ctx = context()
  expect(await handler.fetch(new Request("https://studio.test/api/me"), {}, ctx)).toBe(reply)
  expect(ctx.waited).toEqual([])
})

test("OTEL-NO-TENANT-1: latency and errors are exported per route template, refused and unknown requests included", async () => {
  const env = { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x", NORN_OTEL_INTERVAL_MS: "1000" }
  const t = telemetry(env, "norn-studio")!
  const { sent, send } = capture()
  t.send = send
  const handler = instrumented<typeof env>({
    fetch: async (request) => {
      const path = new URL(request.url).pathname
      if (path === "/api/me") return new Response(null, { status: 401 })
      if (path.startsWith("/api/agent/")) return Response.json({ error: "At most 20 proposals an hour" }, { status: 429, headers: { "x-norn-limit": "proposals_hourly" } })
      if (path.startsWith("/api/host/")) return new Response(null, { status: 200 })
      return new Response(null, { status: 404 })
    },
  })
  const ctx = context()
  await handler.fetch(new Request("https://studio.test/api/me"), env, ctx)
  await handler.fetch(new Request("https://studio.test/api/host/v1/objects/acme.Order/acme-corp?orgId=acme"), env, ctx)
  await handler.fetch(new Request("https://studio.test/api/agent/dashboards", { method: "POST", body: "{}" }), env, ctx)
  await handler.fetch(new Request("https://studio.test/api/acme-corp/globex_prod:pat@example.com"), env, ctx)
  await handler.fetch(new Request("https://studio.test/api/host/v1/acme-corp"), env, ctx)
  await settled(ctx)
  await t.flush()
  const all = sent.join("\n")
  expect(all).toContain("norn_studio_requests_total")
  expect(all).toContain("norn_studio_errors_total")
  expect(all).toContain("/api/host/v1/objects/{type}/{id}")
  expect(all).toContain('"quota"')
  expect(all).toContain("studio GET other")
  for (const leaked of [...TENANT, "acme.Order"]) expect(all).not.toContain(leaked)
})

test("OTEL-NEVER-FAILS-1: a hanging collector, a throwing hook or bad settings never delay or change the result", async () => {
  const reply = new Response("ok")
  const handler = instrumented<Record<string, unknown>>({ fetch: async () => reply })
  // A collector that never answers.
  const env = { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", NORN_OTEL_INTERVAL_MS: "1000" }
  telemetry(env, "norn-studio")!.send = (() => new Promise(() => {})) as unknown as typeof fetch
  const started = Date.now()
  expect(await handler.fetch(new Request("https://studio.test/api/me"), env, context())).toBe(reply)
  expect(Date.now() - started).toBeLessThan(200)
  // Hooks that throw.
  const broken = { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn" }
  const t = telemetry(broken, "norn-studio")!
  t.count = () => { throw new Error("count broke") }
  t.pump = () => { throw new Error("pump broke") }
  t.log = () => {}
  const ctx = context()
  expect(await handler.fetch(new Request("https://studio.test/api/me"), broken, ctx)).toBe(reply)
  await Promise.allSettled(ctx.waited)
  // Bad settings: telemetry off, the handler called directly.
  for (const bad of [{ OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=a%0Ab" }, { OTEL_EXPORTER_OTLP_ENDPOINT: "http://o2.test", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=x" }, { OTEL_EXPORTER_OTLP_ENDPOINT: 7 }]) {
    const quiet = context()
    expect(await handler.fetch(new Request("https://studio.test/api/me"), bad, quiet)).toBe(reply)
    expect(quiet.waited).toEqual([])
  }
  // A handler that throws still throws its own error.
  const failing = instrumented<typeof broken>({ fetch: async () => { throw new Error("handler broke") } })
  await expect(failing.fetch(new Request("https://studio.test/api/me"), { ...broken }, context())).rejects.toThrow("handler broke")
})

test("a single request is exported by its own background task", async () => {
  const env = { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", NORN_OTEL_INTERVAL_MS: "1000" }
  const { sent, send } = capture()
  telemetry(env, "norn-studio")!.send = send
  const ctx = context()
  await instrumented<typeof env>({ fetch: async () => new Response("ok") }).fetch(new Request("https://studio.test/api/me"), env, ctx)
  await settled(ctx)
  expect(sent.join("\n")).toContain("norn_studio_requests_total")
})

test("a deploy passes OpenTelemetry settings only when the endpoint is a credential-free URL", () => {
  expect(otelSettings({})).toBeUndefined()
  expect(otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x" })).toEqual({ endpoint: "https://o2.test/api/norn", headers: "Authorization=Basic%20x" })
  expect(() => otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "https://u:p@o2.test" })).toThrow()
  expect(() => otelSettings({ OTEL_EXPORTER_OTLP_HEADERS: "Authorization=x" })).toThrow()
  expect(() => otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "http://o2.test/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x" })).toThrow()
  expect(() => otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=a%0Ab" })).toThrow()
  expect(otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "http://127.0.0.1:5080/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x" })?.endpoint).toBe("http://127.0.0.1:5080/api/norn")
})
