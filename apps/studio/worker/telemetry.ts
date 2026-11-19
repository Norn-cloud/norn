import { cellRoute, methodOf, routeTable, statusClass, telemetry, type Telemetry } from "../../norn-cell-do/otlp.mjs"

/**
 * Studio's request telemetry over OTLP (apps/norn-cell-do/otlp.mjs): latency and
 * status per route, and auth, billing, spend-cap, quota, rate-limit and server errors. Off
 * unless OTEL_EXPORTER_OTLP_ENDPOINT is set (OTEL-OFF-1); when off the handler
 * is called directly. Attributes and span names come from fixed tables only
 * (OTEL-NO-TENANT-1): a route template, an allowlisted method, a status class
 * and an error class; no user, organization, email or path segment.
 * OTEL-NEVER-FAILS-1: the wrapper reads only the response's status and its
 * `x-norn-limit` header, and records and exports in a background task handed
 * to waitUntil; a telemetry failure never changes the handler's result.
 */
export const SERVICE = "norn-studio"

/** Every Studio route, as worker/index.ts and its route modules serve them. Ordered: a specific template precedes a wider one. */
export const STUDIO_ROUTES = [
  "/mcp",
  "/.well-known/*",
  "/api/me",
  "/api/sidekick/grant",
  "/api/auth/sign-in/email",
  "/api/auth/sign-up/email",
  "/api/auth/sign-in/magic-link",
  "/api/auth/magic-link/verify",
  "/api/auth/sign-in/social",
  "/api/auth/sign-in/oauth",
  "/api/auth/sign-out",
  "/api/auth/get-session",
  "/api/auth/jwks",
  "/api/auth/callback/executor",
  "/api/auth/organization/create",
  "/api/auth/organization/update",
  "/api/auth/organization/set-active",
  "/api/auth/organization/invite-member",
  "/api/auth/organization/accept-invitation",
  "/api/auth/*",
  "/api/questions/check",
  "/api/questions/catalog",
  "/api/questions/recommendations",
  "/api/agent/dashboards",
  "/api/agent/dashboards/{proposal}",
  "/api/agent/dashboards/{proposal}/accept",
  "/api/agent/dashboards/{proposal}/decline",
  "/api/connectors/status",
  "/api/connectors/link",
  "/api/connectors/unlink",
  "/api/connectors/inventory",
  "/api/connectors/bindings",
  "/api/connectors/bindings/{binding}",
  "/api/connectors/bindings/{binding}/run",
  "/api/connectors/accounts/{account}",
  "/api/connectors/apps/{app}/tools",
  "/api/connectors/apps/{app}/accounts",
  "/api/connectors/apps/{app}/accounts/{account}/connect",
  "/api/connectors/apps/{app}/accounts/{account}/{connection}",
] as const

const studioTable = routeTable(STUDIO_ROUTES)

/** The bounded route a Studio path is counted under: a template from STUDIO_ROUTES or the cell's, "static", or "other". */
export function studioRoute(pathname: string): string {
  if (pathname.startsWith("/api/host/")) {
    // The cell's own routes, as the cell host names them.
    const route = cellRoute(pathname.slice("/api/host".length))
    return route === "other" ? "other" : `/api/host${route}`
  }
  const route = studioTable(pathname)
  if (route !== "other") return route
  return pathname === "/mcp" || pathname.startsWith("/api/") || pathname.startsWith("/.well-known/") ? "other" : "static"
}

/** The fixed reasons a 429 names in `x-norn-limit` (assistant.ts). */
const LIMITS: Readonly<Record<string, string>> = { spend_cap: "spend_cap", proposals_hourly: "quota" }

/** Which error class a response is: auth (401/403), billing (402), spend_cap (the assistant's spend cap), quota (the assistant's hourly proposals), rate limit, server; null otherwise. */
export function errorClass(status: number, limit: string | null = null): string | null {
  if (status === 401 || status === 403) return "auth"
  if (status === 402) return "billing"
  if (status === 429) return (limit !== null && Object.hasOwn(LIMITS, limit) ? LIMITS[limit] : undefined) ?? "rate_limit"
  if (status >= 500) return "server"
  return null
}

/** What the wrapper saw of one request: bounded metadata only. */
export interface Seen {
  readonly method: string
  readonly pathname: string
}

export function record(t: Telemetry, seen: Seen, status: number, limit: string | null, startedMs: number, endedMs: number): void {
  const route = studioRoute(seen.pathname)
  const method = methodOf(seen.method)
  const attrs = { route, method, status_class: statusClass(status) }
  t.count("norn_studio_requests_total", attrs)
  t.observe("norn_studio_request_duration_ms", attrs, endedMs - startedMs)
  const error = errorClass(status, limit)
  if (error !== null) t.count("norn_studio_errors_total", { route, class: error, status })
  if (route !== "static") t.span(`studio ${method} ${route}`, { "http.route": route, "http.request.method": method, "http.response.status_code": status, "norn.error.class": error ?? "" }, startedMs, endedMs, status >= 500)
}

/** A Worker handler; method syntax so a handler typed for Cloudflare's narrower Request fits. */
interface Handler<E> {
  fetch(request: Request, env: E, ctx?: ExecutionContext): Promise<Response>
}

const safeTelemetry = (env: unknown): Telemetry | null => {
  try {
    return telemetry(env, SERVICE)
  } catch {
    return null
  }
}

/** Record and export after one timer turn, as the event's background task; never throws. */
function background(t: Telemetry, ctx: ExecutionContext | undefined, work: () => void): void {
  try {
    const task = new Promise((resolve) => setTimeout(resolve, 0)).then(() => {
      try {
        work()
      } catch {
        t.log("studio telemetry record failed")
      }
      return t.pump()
    }).catch(() => {})
    ctx?.waitUntil?.(task)
  } catch {
    // Best effort.
  }
}

/** The handler, timed per request when export is configured. */
export function instrumented<E>(handler: Handler<E>): Handler<E> {
  return {
    async fetch(request, env, ctx) {
      const t = safeTelemetry(env)
      if (t === null) return handler.fetch(request, env, ctx)
      let seen: Seen | null = null
      let started = 0
      try {
        seen = { method: request.method, pathname: new URL(request.url).pathname }
        started = Date.now()
      } catch {
        // Unrecorded.
      }
      let status = 500
      let limit: string | null = null
      try {
        const response = await handler.fetch(request, env, ctx)
        try {
          status = response.status
          limit = response.headers.get("x-norn-limit")
        } catch {
          // Unrecorded.
        }
        return response
      } finally {
        if (seen !== null) {
          const done = seen
          let ended = started
          try { ended = Date.now() } catch { /* unrecorded duration */ }
          background(t, ctx, () => record(t, done, status, limit, started, ended))
        }
      }
    },
  }
}
