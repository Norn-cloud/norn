import { routeOf, statusClass, telemetry, type Telemetry } from "../../norn-cell-do/otlp.mjs"

/**
 * Studio's request telemetry over OTLP (apps/norn-cell-do/otlp.mjs): latency and
 * status per route, and auth, billing and server errors. Off unless
 * OTEL_EXPORTER_OTLP_ENDPOINT is set (OTEL-OFF-1); when off the handler is
 * called directly. Attributes are route templates, methods and status classes
 * only (OTEL-NO-TENANT-1): no user, organization, email or path identifier.
 */
export const SERVICE = "norn-studio"

/** A path segment kept in a route: a plain lowercase word. Anything with a digit, dot or underscore is an identifier. */
const WORD = /^[a-z][a-z-]{0,31}$/

/** The bounded route a Studio path is counted under. */
export function studioRoute(pathname: string): string {
  if (pathname === "/mcp") return "/mcp"
  if (pathname.startsWith("/.well-known/")) return "/.well-known"
  if (!pathname.startsWith("/api/")) return "static"
  // The cell's own routes, as the cell host names them.
  if (pathname.startsWith("/api/host/")) return `/api/host${routeOf(pathname.slice("/api/host".length))}`
  const parts = pathname.split("/").slice(2, 6)
  return `/api/${parts.map((part) => (WORD.test(part) ? part : "{}")).join("/")}`
}

/** Which error class a response is: auth (401/403), billing (402, or the assistant's spend cap), rate limit, server; null otherwise. */
export function errorClass(route: string, status: number): string | null {
  if (status === 401 || status === 403) return "auth"
  if (status === 402 || (status === 429 && route.startsWith("/api/agent/"))) return "billing"
  if (status === 429) return "rate_limit"
  if (status >= 500) return "server"
  return null
}

export function record(t: Telemetry, request: Request, status: number, startedMs: number, endedMs: number): void {
  const route = studioRoute(new URL(request.url).pathname)
  const attrs = { route, method: request.method, status_class: statusClass(status) }
  t.count("norn_studio_requests_total", attrs)
  t.observe("norn_studio_request_duration_ms", attrs, endedMs - startedMs)
  const error = errorClass(route, status)
  if (error !== null) t.count("norn_studio_errors_total", { route, class: error, status })
  if (route !== "static") t.span(`studio ${request.method} ${route}`, { "http.route": route, "http.request.method": request.method, "http.response.status_code": status, "norn.error.class": error ?? "" }, startedMs, endedMs, status >= 500)
}

/** A Worker handler; method syntax so a handler typed for Cloudflare's narrower Request fits. */
interface Handler<E> {
  fetch(request: Request, env: E, ctx?: ExecutionContext): Promise<Response>
}

/** The handler, timed per request when export is configured. */
export function instrumented<E>(handler: Handler<E>): Handler<E> {
  return {
    async fetch(request, env, ctx) {
      const t = telemetry(env, SERVICE)
      if (t === null) return handler.fetch(request, env, ctx)
      const started = Date.now()
      let status = 500
      try {
        const response = await handler.fetch(request, env, ctx)
        status = response.status
        return response
      } finally {
        try { record(t, request, status, started, Date.now()) } catch { t.log("studio telemetry record failed") }
        ctx?.waitUntil?.(t.flushIfDue())
      }
    },
  }
}
