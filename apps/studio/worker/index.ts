import { forwardToHost, originAllowed, resolveActor } from "./actor.ts"
import { cellHostOf } from "./http-host.ts"
import { activeMembership, createAuth, verifyMagicLinkOrigin, type Auth, type MagicLinkMail } from "./auth.ts"
import { passwordScrypt } from "./password.ts"
import { clientIpHeader } from "./client-ip.ts"
import { connectors } from "./connectors.ts"
import { MCP_PATH, serveMcp } from "./mcp.ts"
import { cellFor } from "./cell.ts"
import { questions } from "./questions.ts"
import { billingDocument, entitlementOf, limitRaised, subscribed, billingOf, type BillingDb, type BillingSettings } from "./billing.ts"
import { polarConfig, settingsOf } from "./polar-config.ts"
import { acceptWorkspaceInvitation, createWorkspace, listWorkspaces } from "./workspaces.ts"
import { cellScope } from "./scope.ts"
import { instrumented } from "./telemetry.ts"

/** The Studio Worker: Better Auth under /api/auth, authenticated /api/*, static assets for everything else. */
export interface Env {
  readonly ASSETS: Fetcher
  /** D1 on Cloudflare; any database Better Auth's Kysely adapter accepts (bun:sqlite in tests). */
  readonly AUTH_DB: D1Database
  readonly BETTER_AUTH_SECRET: string
  readonly BETTER_AUTH_URL: string
  /** Optional default data cell for newly created workspaces. */
  readonly STUDIO_DEFAULT_ENVIRONMENT_ID?: string
  /** JSON array of the exact origins configured for this stage. */
  readonly BETTER_AUTH_TRUSTED_ORIGINS?: string
  /** Cloudflare Email Service binding and sender; both absent until a sender is enabled. */
  readonly EMAIL?: { send(message: { to: string; from: string; subject: string; text: string }): Promise<unknown> }
  readonly EMAIL_FROM?: string
  /** "js" selects the pure-JS scrypt (set by the celld config); Cloudflare leaves it unset. */
  readonly AUTH_SCRYPT?: string
  /** The Norn cell host (service binding); absent until a host is connected to this stage. */
  readonly NORN_HOST?: Fetcher
  /** Or the cell host's https origin (a self-hosted host, http-host.ts); never both. */
  readonly NORN_HOST_URL?: string
  /** The organization members' sessions start in (preview: the seeded `org_demo`). */
  readonly STUDIO_DEFAULT_ORG?: string
  /** The header the edge sets to the client's address (client-ip.ts); required, the Worker refuses auth without it. */
  readonly STUDIO_CLIENT_IP_HEADER?: string
  /** The tool gateway and Studio's OAuth client there (#772); all three or none. */
  readonly TOOL_GATEWAY_URL?: string
  readonly TOOL_GATEWAY_CLIENT_ID?: string
  readonly TOOL_GATEWAY_CLIENT_SECRET?: string
  /** "on" serves /mcp to assistants people connect, with Studio as their OAuth server (migration 0007). */
  readonly STUDIO_MCP?: string
  /** Invite-only sign-up: the comma-separated addresses that may create an account. */
  readonly STUDIO_SIGNUP_EMAILS?: string
  /** The dashboard assistant's model (#923): the AI Gateway URL (it names the account) and its run-only token; both or neither. */
  readonly AI_GATEWAY_URL?: string
  readonly AI_GATEWAY_TOKEN?: string
  /** Claude through the gateway (default claude-sonnet-5-5), and the Workers AI fallback. */
  readonly AGENT_MODEL?: string
  readonly AGENT_FALLBACK_MODEL?: string
  /** What the assistant may spend in all, in dollars (default 20). */
  readonly AGENT_SPEND_CAP_USD?: string
  /** OpenTelemetry export (telemetry.ts); off unless the endpoint is set. Headers carry the collector's credentials. */
  readonly OTEL_EXPORTER_OTLP_ENDPOINT?: string
  readonly OTEL_EXPORTER_OTLP_HEADERS?: string
  readonly OTEL_RESOURCE_ATTRIBUTES?: string
  readonly OTEL_TRACES_SAMPLER_ARG?: string
  /**
   * Hosted billing through Polar (worker/polar-config.ts); all four or none. None: billing is off and
   * every limit is unlimited (self-hosting). POLAR_SERVER is "sandbox" or "production"; POLAR_PLANS
   * is the checkout plans as JSON (polar-config.ts PLACEHOLDER_PLANS shows its shape); limits come from Polar.
   */
  readonly POLAR_ACCESS_TOKEN?: string
  readonly POLAR_WEBHOOK_SECRET?: string
  readonly POLAR_SERVER?: string
  readonly POLAR_PLANS?: string
}

export const refuse = (status: number, reason: string, headers: HeadersInit = {}) =>
  Response.json({ error: reason }, { status, headers: { "cache-control": "no-store", ...headers } })

/** Identity reaches Norn only from the verified session; a client-supplied actor header is refused on every stage. */
/** Better Auth's third-party sign-in endpoints: Studio offers none of them. */
export const SIGN_IN_ELSEWHERE = new Set(["/api/auth/sign-in/social", "/api/auth/sign-in/oauth2"])

export const ACTOR_HEADERS = ["x-norn-actor", "x-norn-principal", "x-norn-roles"] as const

/** A missing list keeps existing single-origin deployments; malformed configuration fails closed. */
function trustedOrigins(env: Env): string[] {
  if (env.BETTER_AUTH_TRUSTED_ORIGINS === undefined) return [env.BETTER_AUTH_URL]
  const origins: unknown = JSON.parse(env.BETTER_AUTH_TRUSTED_ORIGINS)
  if (!Array.isArray(origins) || origins.length === 0 || !origins.every((origin) => {
    if (typeof origin !== "string") return false
    try { const url = new URL(origin); return url.protocol === "https:" && url.origin === origin && !url.username && !url.password } catch { return false }
  }) || !origins.includes(env.BETTER_AUTH_URL)) throw new Error("Invalid Studio trusted origins")
  return origins
}

// One Better Auth instance per database binding (per isolate in production).
const instances = new WeakMap<object, Auth>()

/** Whether this deployment has a mail sender configured for magic-link sign-in. */
export const magicLinkEnabled = (env: Pick<Env, "EMAIL" | "EMAIL_FROM">): boolean =>
  env.EMAIL !== undefined && typeof env.EMAIL_FROM === "string" && env.EMAIL_FROM.trim() !== ""

function auth(env: Env): Auth {
  const known = instances.get(env.AUTH_DB)
  if (known !== undefined) return known
  const { EMAIL: mail, EMAIL_FROM: from } = env
  const sendMagicLink = !magicLinkEnabled(env) || mail === undefined || from === undefined ? undefined : async ({ to, url }: MagicLinkMail) => {
    await mail.send({ to, from, subject: "Sign in to Norn Studio", text: `Sign in to Norn Studio: ${url}\n\nThe link expires in 10 minutes. If you did not ask for it, ignore this email.` })
  }
  const { TOOL_GATEWAY_URL: url, TOOL_GATEWAY_CLIENT_ID: clientId, TOOL_GATEWAY_CLIENT_SECRET: clientSecret } = env
  const gateway = url === undefined || clientId === undefined || clientSecret === undefined ? undefined : { url, clientId, clientSecret }
  const billing = polarConfig(env)
  const defaultEnvironmentId = env.STUDIO_DEFAULT_ENVIRONMENT_ID?.trim() || undefined
  const instance = createAuth({ ...billing === undefined ? {} : { billing: { config: billing, db: env.AUTH_DB as unknown as BillingDb } }, mcp: mcpOn(env), database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: env.BETTER_AUTH_URL, trustedOrigins: trustedOrigins(env), defaultEnvironmentId, scrypt: passwordScrypt(env.AUTH_SCRYPT), clientIpHeader: clientIpHeader(env.STUDIO_CLIENT_IP_HEADER), ...signUpEmails(env) === undefined ? {} : { signUpEmails: signUpEmails(env) }, ...sendMagicLink === undefined ? {} : { sendMagicLink }, ...env.STUDIO_DEFAULT_ORG === undefined ? {} : { defaultOrganizationId: env.STUDIO_DEFAULT_ORG }, ...gateway === undefined ? {} : { gateway } })
  const handle = instance.handler
  instance.handler = (request: Request) => guarded(handle, request)
  instances.set(env.AUTH_DB, instance)
  return instance
}

const INVITATION_ANSWERED = "This invitation was already answered: accepted, rejected or withdrawn."
/** Migration 0012's refusal to change an invitation that is no longer pending. */
export const invitationAnswered = (error: unknown) => (error instanceof Error ? error.message : String(error)).includes("invitation.not_pending")

/**
 * Better Auth's handler, with the errors it throws (auth.ts onAPIError) answered: a plan-limit
 * trigger's refusal (migration 0011) as PLAN_LIMIT, a write to an invitation already answered
 * (migration 0012) as INVITATION_NOT_FOUND, anything else as a bare 500. Nothing in the request
 * is read to explain it; the routes that can meet a limit with billing on are Studio's own
 * (workspaces.ts), which explain it from what the server authorized.
 */
async function guarded(handle: (request: Request) => Promise<Response>, request: Request): Promise<Response> {
  try {
    return await handle(request)
  } catch (error) {
    // Another answer to the invitation committed first (migration 0012): it stays as that answer.
    if (invitationAnswered(error)) return Response.json({ code: "INVITATION_NOT_FOUND", message: INVITATION_ANSWERED }, { status: 400, headers: { "cache-control": "no-store" } })
    const limit = limitRaised(error)
    if (limit === null) {
      console.error(error)
      return new Response(null, { status: 500, statusText: "Internal Server Error" })
    }
    const message = limit === "cells" ? "Your plan's workspace limit is reached." : "This workspace's member limit is reached."
    return Response.json({ code: "PLAN_LIMIT", message }, { status: 403, headers: { "cache-control": "no-store" } })
  }
}

/**
 * Better Auth's routes that write a workspace and its owner, or an accepted invitation and its member,
 * in separate statements. With billing on they are refused: Studio's own (workspaces.ts) do each in one
 * transaction, so a plan-limit refusal or an interruption never leaves half of one.
 */
export const SPLIT_WRITE_PATHS: ReadonlyMap<string, string> = new Map([
  ["/api/auth/organization/create", "/api/workspaces"],
  ["/api/auth/organization/accept-invitation", "/api/workspaces/invitations/accept"],
])
/** Matched as Better Auth's router could: any case, with or without a trailing slash. */
const routePath = (pathname: string) => pathname.toLowerCase().replace(/\/+$/, "")

/** Studio's workspace routes: read-only GET /api/workspaces for the switcher, and POST writes from a trusted origin. */
async function workspaceRoute(request: Request, env: Env, pathname: string): Promise<Response> {
  const clean = routePath(pathname)
  if (clean === "/api/workspaces" && request.method === "GET") {
    try {
      return await listWorkspaces(request, { auth: auth(env), db: env.AUTH_DB })
    } catch (error) {
      console.error(error)
      return new Response(null, { status: 500, statusText: "Internal Server Error" })
    }
  }
  if (request.method !== "POST") {
    return clean === "/api/workspaces"
      ? refuse(405, "GET or POST only.", { allow: "GET, POST" })
      : refuse(405, "POST only.", { allow: "POST" })
  }
  const origin = request.headers.get("origin")
  if (origin === null || !trustedOrigins(env).includes(origin)) return refuse(403, "Cross-origin request refused.")
  const context = { auth: auth(env), db: env.AUTH_DB, billing: await billingSettings(env), baseURL: env.BETTER_AUTH_URL }
  const handle = pathname === "/api/workspaces" ? createWorkspace : pathname === "/api/workspaces/invitations/accept" ? acceptWorkspaceInvitation : undefined
  if (handle === undefined) return refuse(404, "No such endpoint.")
  try {
    return await handle(request, context)
  } catch (error) {
    // Rolled back whole (workspaces.ts); a bare 500, as Better Auth answers a database failure.
    console.error(error)
    return new Response(null, { status: 500, statusText: "Internal Server Error" })
  }
}

/**
 * The public answer to a successful sign-up: the same keys in the same order for a new and a
 * registered address, a placeholder id, no server timestamps and no cookie. Better Auth's own
 * body differs between the two (a real row vs a synthetic one, createdAt taken at different points).
 * Explicit refusals (e.g. 403 INVITE_ONLY or 429 rate-limit) pass through with no-store.
 */
export async function publicSignUp(response: Response): Promise<Response> {
  if (!response.ok) {
    const headers = new Headers(response.headers)
    if (!headers.has("cache-control")) headers.set("cache-control", "no-store")
    return new Response(response.body, { status: response.status, statusText: response.statusText, headers })
  }
  const { user } = (await response.json()) as { user: { email: string; name: string } }
  const body = { token: null, user: { id: crypto.randomUUID(), email: user.email, name: user.name, emailVerified: false } }
  return Response.json(body, { status: 200, headers: { "cache-control": "no-store" } })
}

/**
 * Accepting an invitation that is no longer pending: it expired or an admin withdrew it; Better Auth
 * says only "Invitation not found", so say what the person can do about it while keeping every
 * upstream response header (such as a refreshed Set-Cookie from session middleware).
 */
export async function acceptInvitation(response: Response): Promise<Response> {
  if (response.status !== 400) return response
  const body = (await response.clone().json().catch(() => null)) as { code?: unknown } | null
  if (body?.code !== "INVITATION_NOT_FOUND") return response
  await response.body?.cancel().catch(() => undefined)
  const headers = new Headers(response.headers)
  headers.delete("content-length")
  headers.delete("content-encoding")
  headers.delete("etag")
  headers.set("content-type", "application/json; charset=utf-8")
  headers.set("cache-control", "no-store")
  return new Response(
    JSON.stringify({ code: "INVITATION_NOT_FOUND", message: "This invitation is no longer open: it expired or was withdrawn. Ask the workspace's admins for a new one." }),
    { status: response.status, statusText: response.statusText, headers },
  )
}

/** Public auth capabilities so LoginPage hides unsupported methods (no 503 Email link tab) and states invite-only clearly. */
function authConfig(request: Request, env: Env): Response {
  if (request.method !== "GET") return refuse(405, "GET only.", { allow: "GET" })
  return Response.json(
    { magicLink: magicLinkEnabled(env), inviteOnly: signUpEmails(env) !== undefined },
    { headers: { "cache-control": "no-store" } },
  )
}

/** The signed-in user and active organization, for the shell. Display only; Norn decides permissions. */
async function me(request: Request, env: Env): Promise<Response> {
  const session = await auth(env).api.getSession({ headers: request.headers })
  if (session === null) return refuse(401, "Sign in first.")
  const member = await activeMembership(auth(env), session.user.id, session.session.activeOrganizationId)
  return Response.json(
    { user: { id: session.user.id, name: session.user.name, email: session.user.email }, organization: member === null ? null : { id: member.organizationId, name: member.organizationName, role: member.role },
      // The caller's active workspace's data environment, from its server-provisioned metadata; null without one.
      environmentId: member?.environmentId ?? null },
    { headers: { "cache-control": "no-store" } },
  )
}

const mcpOn = (env: Env) => env.STUDIO_MCP === "on"

/**
 * Billing as entitlement and admission see it (provider-neutral), or undefined when off: on or off as
 * the database's stored mode says (billing.ts billingOf), the same mode its limit triggers read, never
 * this isolate's own settings. The Polar settings themselves stay with the adapter (auth.ts, polar.ts).
 */
const billingSettings = (env: Env): Promise<BillingSettings | undefined> => billingOf(env.AUTH_DB as unknown as BillingDb, settingsOf(polarConfig(env)))

/**
 * The Polar plugin's checkout and portal endpoints. The plugin forwards whatever the caller sends
 * (trial length, products, discounts, metadata, success and return URLs) to Polar, so a caller could
 * shape their own checkout, e.g. a years-long free trial that reads as an active plan. They are refused
 * from outside; /api/billing/* is the only way in and calls them with a fixed body.
 */
/**
 * The plugin's endpoints refused from outside: checkout and portal (Studio calls them itself, from
 * /api/billing, with a fixed body) and the customer reads, which Studio never calls. The reads go too:
 * with a `reference_id`, `/customer/subscriptions/list` lists a whole workspace's subscriptions by
 * metadata under the organization's token. Studio's billing document (/api/billing) is the only read.
 */
export const PLUGIN_BILLING_PATHS = new Set([
  "/api/auth/checkout",
  "/api/auth/customer/portal",
  "/api/auth/customer/state",
  "/api/auth/customer/orders/list",
  "/api/auth/customer/benefits/list",
  "/api/auth/customer/subscriptions/list",
])
/** Matched as Better Auth's router could: any case, with or without a trailing slash. */
const pluginBillingPath = (pathname: string) => PLUGIN_BILLING_PATHS.has(pathname.toLowerCase().replace(/\/+$/, ""))

/** A Polar page (checkout, portal) the plugin minted for this person; refused unless it is https. */
async function toPolar(response: Response): Promise<Response> {
  if (!response.ok) return refuse(response.status === 401 ? 401 : 502, response.status === 401 ? "Sign in first." : "Billing is unavailable right now; try again shortly.")
  const { url } = (await response.json()) as { url?: unknown }
  if (typeof url !== "string" || !URL.canParse(url) || new URL(url).protocol !== "https:") return refuse(502, "Billing is unavailable right now; try again shortly.")
  return new Response(null, { status: 303, headers: { location: url, "cache-control": "no-store" } })
}

/**
 * The billing routes: the server's billing document for the shell, and links that send the signed-in
 * person to Polar. Checkout and portal go through the plugin's own endpoints (session, origin and rate
 * checks included); a person who already pays is sent to the portal, never a second checkout.
 */
async function billingRoute(request: Request, env: Env, billing: BillingSettings | undefined, pathname: string): Promise<Response> {
  if (request.method !== "GET") return refuse(405, "GET only.", { allow: "GET" })
  // Checkout and portal are plain links in Studio's own pages; another site must not start one with the person's cookie.
  const site = request.headers.get("sec-fetch-site")
  if (pathname !== "/api/billing" && site !== null && site !== "same-origin" && site !== "none") return refuse(403, "Cross-site billing request refused.")
  const session = await auth(env).api.getSession({ headers: request.headers })
  if (session === null) return refuse(401, "Sign in first.")
  const db = env.AUTH_DB as unknown as BillingDb
  if (pathname === "/api/billing") return Response.json(await billingDocument(db, billing, env.BETTER_AUTH_URL, session.user.id), { headers: { "cache-control": "no-store" } })
  if (billing === undefined) return refuse(404, "Billing is not enabled on this deployment.")
  const internal = (path: string, body: unknown) => auth(env).handler(new Request(new URL(`/api/auth${path}`, env.BETTER_AUTH_URL), {
    method: "POST",
    // Only the cookie: getSession above resolves a person from the session cookie alone (no bearer plugin;
    // API keys belong to workspaces, not people), so the plugin acts for exactly the person this route resolved.
    headers: { "content-type": "application/json", cookie: request.headers.get("cookie") ?? "", origin: new URL(env.BETTER_AUTH_URL).origin, [clientIpHeader(env.STUDIO_CLIENT_IP_HEADER)]: request.headers.get(clientIpHeader(env.STUDIO_CLIENT_IP_HEADER)) ?? "" },
    body: JSON.stringify(body),
  }))
  if (pathname === "/api/billing/portal") return toPolar(await internal("/customer/portal", { redirect: false }))
  if (pathname === "/api/billing/checkout") {
    const plan = new URL(request.url).searchParams.get("plan")
    if (plan === null || !billing.plans.some((candidate) => candidate.id === plan)) return refuse(404, "No such plan.")
    if (subscribed(await entitlementOf(db, billing, session.user.id))) return toPolar(await internal("/customer/portal", { redirect: false }))
    return toPolar(await internal("/checkout", { slug: plan, redirect: false }))
  }
  return refuse(404, "No such endpoint.")
}
/** The invited addresses, lower-case; undefined (open sign-up) when the setting is absent. */
export const signUpEmails = (env: Pick<Env, "STUDIO_SIGNUP_EMAILS">): readonly string[] | undefined =>
  env.STUDIO_SIGNUP_EMAILS === undefined ? undefined : env.STUDIO_SIGNUP_EMAILS.split(",").map((email) => email.trim().toLowerCase()).filter(Boolean)

/** OAuth discovery for assistants: the authorization server's and the MCP resource's metadata (RFC 8414, RFC 9728). */
const OAUTH_DISCOVERY = new Set([
  "/.well-known/oauth-authorization-server",
  "/.well-known/openid-configuration",
  "/.well-known/oauth-protected-resource",
  `/.well-known/oauth-protected-resource${MCP_PATH}`,
])

// One cell host per stage: the binding, or the https origin as a Fetcher (http-host.ts). Derived
// once per runtime env, so `auth(env)` keeps reusing its instance across requests.
const hosted = new WeakMap<object, Env>()
function withCellHost(given: Env): Env {
  const cached = hosted.get(given)
  if (cached !== undefined) return cached
  const host = cellHostOf(given)
  const env: Env = host === undefined || host === given.NORN_HOST ? given : { ...given, NORN_HOST: host }
  hosted.set(given, env)
  return env
}

const handler = {
  async fetch(request, given) {
    const env = withCellHost(given)
    const { pathname } = new URL(request.url)
    if (mcpOn(env) && pathname === MCP_PATH) {
      if (ACTOR_HEADERS.some((name) => request.headers.has(name))) return refuse(400, "Identity comes from the signed-in grant, not a request header.")
      return serveMcp(request, { auth: auth(env), baseURL: env.BETTER_AUTH_URL, trustedOrigins: trustedOrigins(env), host: env.NORN_HOST, assets: env.ASSETS })
    }
    if (mcpOn(env) && OAUTH_DISCOVERY.has(pathname)) return auth(env).handler(request)
    if (!pathname.startsWith("/api/")) return env.ASSETS.fetch(request)
    if (ACTOR_HEADERS.some((name) => request.headers.has(name))) return refuse(400, "Identity comes from the signed-in session, not a request header.")
    if (routePath(pathname) === "/api/auth/config") return authConfig(request, env)
    if (pathname === "/api/auth/sign-in/magic-link" || pathname === "/api/auth/magic-link/verify") {
      const host = new URL(request.url).host
      if ([request.headers.get("host"), request.headers.get("x-forwarded-host")].some((value) => value !== null && value.toLowerCase() !== host.toLowerCase())) return refuse(403, "Magic link host does not match this origin.")
    }
    if (pathname === "/api/auth/magic-link/verify" && !(await verifyMagicLinkOrigin(request, env.BETTER_AUTH_SECRET, trustedOrigins(env)))) return refuse(403, "Magic link is not valid for this origin.")
    if (routePath(pathname) === "/api/auth/sign-up/email") return publicSignUp(await auth(env).handler(request))
    // The gateway provider is link-only: refuse every sign-in that could start from it (social and
    // generic OAuth), so a linked gateway identity never yields a Studio session. Linking stays.
    if (SIGN_IN_ELSEWHERE.has(pathname)) return refuse(403, "Studio sign-in is by email and password or magic link.")
    if (pluginBillingPath(pathname)) return refuse(404, "No such endpoint; billing starts at /api/billing.")
    const moved = SPLIT_WRITE_PATHS.get(routePath(pathname))
    if (moved !== undefined && await billingSettings(env) !== undefined) return Response.json({ code: "USE_STUDIO_ROUTE", message: `With billing on, this goes through ${moved}.`, route: moved }, { status: 404, headers: { "cache-control": "no-store" } })
    if (routePath(pathname) === "/api/auth/organization/accept-invitation") return acceptInvitation(await auth(env).handler(request))
    if (pathname === "/api/workspaces" || pathname.startsWith("/api/workspaces/")) return workspaceRoute(request, env, pathname)
    if (pathname.startsWith("/api/auth/")) return auth(env).handler(request)
    if (pathname === "/api/me") return me(request, env)
    if (pathname === "/api/billing" || pathname.startsWith("/api/billing/")) return billingRoute(request, env, await billingSettings(env), pathname)
    // Every Norn call goes to the host as the resolved actor; the host and its PDP decide.
    if (pathname.startsWith("/api/host/")) {
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      if (!originAllowed(request, actor, trustedOrigins(env))) return refuse(403, "Cross-origin request refused.")
      if (env.NORN_HOST === undefined) return refuse(503, "No Norn host is connected to this deployment.")
      return forwardToHost(request, env.NORN_HOST, auth(env), actor, pathname.slice("/api/host".length))
    }
    // The question builders' catalog and checker: the cell's, read as the resolved actor.
    if (pathname.startsWith("/api/questions/")) {
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      if (!originAllowed(request, actor, trustedOrigins(env))) return refuse(403, "Cross-origin request refused.")
      if (env.NORN_HOST === undefined) return refuse(503, "No Norn host is connected to this deployment.")
      return questions(request, cellFor(env.NORN_HOST, auth(env), actor), pathname.slice("/api/questions".length))
    }
    // Sidekick's grant: the signed-in person's own /mcp access for the in-app agent (sidekick.ts).
    if (pathname === "/api/sidekick/grant") {
      if (request.method !== "POST") return refuse(405, "POST only.", { allow: "POST" })
      if (!mcpOn(env)) return refuse(404, "No such endpoint.")
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      if (actor.actorKind !== "user") return refuse(403, "Sidekick acts for a signed-in person only.")
      if (!originAllowed(request, actor, trustedOrigins(env))) return refuse(403, "Cross-origin request refused.")
      if ((await cellScope(auth(env), actor.orgId)).environmentId === null) return refuse(409, "This workspace has no data cell yet.")
      const { grantAllowed, sidekickGrant } = await import("./sidekick.ts")
      if (!grantAllowed(actor.subjectId)) return refuse(429, "Too many Sidekick grants; wait a few minutes.", { "retry-after": "60" })
      return Response.json(await sidekickGrant(auth(env), env.BETTER_AUTH_URL, actor), { headers: { "cache-control": "no-store" } })
    }
    // The dashboard assistant: it acts only as the signed-in person, through the cell's own admission.
    if (pathname === "/api/agent/dashboards" || pathname.startsWith("/api/agent/dashboards/")) {
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      if (actor.actorKind !== "user") return refuse(403, "The assistant acts for a signed-in person only.")
      if (!originAllowed(request, actor, trustedOrigins(env))) return refuse(403, "Cross-origin request refused.")
      if (env.NORN_HOST === undefined) return refuse(503, "No Norn host is connected to this deployment.")
      const scope = await cellScope(auth(env), actor.orgId)
      if (scope.environmentId === null) return refuse(409, "This workspace has no data cell yet.")
      // Loaded on first use, never with the Worker: sign-in must not pay for the assistant.
      const [{ assistant }, { configuredModel }] = await Promise.all([import("./assistant.ts"), import("./model.ts")])
      const cap = Number(env.AGENT_SPEND_CAP_USD ?? "20")
      return assistant(request, pathname.slice("/api/agent/dashboards".length), {
        db: env.AUTH_DB, cell: cellFor(env.NORN_HOST, auth(env), actor),
        scope: { orgId: actor.orgId, environmentId: scope.environmentId, userId: actor.subjectId },
        model: configuredModel(env), capMicroUsd: Math.round((Number.isFinite(cap) && cap >= 0 ? cap : 20) * 1_000_000), now: Date.now(),
      })
    }
    if (pathname.startsWith("/api/connectors/")) {
      // A gateway URL without its OAuth client is not a gateway this stage can use.
      const gatewayEnv = env.TOOL_GATEWAY_CLIENT_ID === undefined || env.TOOL_GATEWAY_CLIENT_SECRET === undefined ? { ...env, TOOL_GATEWAY_URL: undefined } : env
      return connectors(request, { ...gatewayEnv, trustedOrigins: trustedOrigins(env) }, auth(env), pathname.slice("/api/connectors".length), { db: env.AUTH_DB, ...env.NORN_HOST === undefined ? {} : { host: env.NORN_HOST } })
    }
    const session = await auth(env).api.getSession({ headers: request.headers })
    if (session === null) return refuse(401, "Sign in first.")
    return refuse(404, "No such endpoint.")
  },
} satisfies ExportedHandler<Env>

export default instrumented(handler) satisfies ExportedHandler<Env>
