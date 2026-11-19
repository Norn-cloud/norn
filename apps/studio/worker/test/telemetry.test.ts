import { expect, test } from "bun:test"
import { telemetry } from "../../../norn-cell-do/otlp.mjs"
import { otelSettings } from "../../deploy-target.ts"
import { errorClass, instrumented, studioRoute } from "../telemetry.ts"

// Studio's request telemetry: OTEL-OFF-1 (no endpoint, no change), bounded routes and
// error classes, and OTEL-NO-TENANT-1 (no address, id or email leaves in an attribute).

test("routes are bounded templates; identifiers and emails never become labels", () => {
  expect(studioRoute("/")).toBe("static")
  expect(studioRoute("/assets/index-3f2a.js")).toBe("static")
  expect(studioRoute("/mcp")).toBe("/mcp")
  expect(studioRoute("/.well-known/oauth-authorization-server")).toBe("/.well-known")
  expect(studioRoute("/api/auth/sign-in/email")).toBe("/api/auth/sign-in/email")
  expect(studioRoute("/api/host/v1/objects/shop.Sale/SKU-1")).toBe("/api/host/v1/objects/{}/{}")
  expect(studioRoute("/api/agent/dashboards/prop_8c1f2")).toBe("/api/agent/dashboards/{}")
  expect(studioRoute("/api/connectors/pat@example.com/x/y/z")).toBe("/api/connectors/{}/x/y")
})

test("auth, billing, rate-limit and server errors are told apart", () => {
  expect(errorClass("/api/me", 401)).toBe("auth")
  expect(errorClass("/api/host/v1/{}", 403)).toBe("auth")
  expect(errorClass("/api/agent/dashboards", 429)).toBe("billing")
  expect(errorClass("/api/billing", 402)).toBe("billing")
  expect(errorClass("/api/sidekick/grant", 429)).toBe("rate_limit")
  expect(errorClass("/api/me", 503)).toBe("server")
  expect(errorClass("/api/me", 404)).toBeNull()
})

test("OTEL-OFF-1: without an endpoint the handler's own response passes through", async () => {
  const reply = new Response("ok")
  const handler = instrumented({ fetch: async () => reply })
  const waited: Promise<unknown>[] = []
  const ctx = { waitUntil: (p: Promise<unknown>) => waited.push(p), passThroughOnException() {} } as unknown as ExecutionContext
  expect(await handler.fetch(new Request("https://studio.test/api/me"), {}, ctx)).toBe(reply)
  expect(waited).toEqual([])
})

test("with an endpoint, latency and errors are exported per route with no identifiers", async () => {
  const env = { OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x" }
  const handler = instrumented<typeof env>({ fetch: async (request) => new Response(null, { status: new URL(request.url).pathname === "/api/me" ? 401 : 200 }) })
  await handler.fetch(new Request("https://studio.test/api/me"), env)
  await handler.fetch(new Request("https://studio.test/api/host/v1/objects/acme.Order/ORD-77?orgId=acme"), env)
  const sent: string[] = []
  await telemetry(env, "norn-studio")!.flush((async (_url: string, init: RequestInit) => {
    sent.push(String(init.body))
    return new Response(null)
  }) as unknown as typeof fetch)
  const all = sent.join("\n")
  expect(all).toContain("norn_studio_requests_total")
  expect(all).toContain("norn_studio_errors_total")
  expect(all).toContain("/api/host/v1/objects/{}/{}")
  for (const leaked of ["acme", "ORD-77"]) expect(all).not.toContain(leaked)
})

test("a deploy passes OpenTelemetry settings only when the endpoint is a credential-free URL", () => {
  expect(otelSettings({})).toBeUndefined()
  expect(otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", OTEL_EXPORTER_OTLP_HEADERS: "Authorization=Basic%20x" })).toEqual({ endpoint: "https://o2.test/api/norn", headers: "Authorization=Basic%20x" })
  expect(() => otelSettings({ OTEL_EXPORTER_OTLP_ENDPOINT: "https://u:p@o2.test" })).toThrow()
  expect(() => otelSettings({ OTEL_EXPORTER_OTLP_HEADERS: "Authorization=x" })).toThrow()
})
