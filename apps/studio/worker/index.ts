import { forwardToHost, originAllowed, resolveActor } from "./actor.ts"
import { activeMembership, createAuth, verifyMagicLinkOrigin, type Auth, type MagicLinkMail } from "./auth.ts"
import { passwordScrypt } from "./password.ts"
import { connectors } from "./connectors.ts"

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
  /** The organization members' sessions start in (preview: the seeded `org_demo`). */
  readonly STUDIO_DEFAULT_ORG?: string
  /** The tool gateway and Studio's OAuth client there (#772); all three or none. */
  readonly TOOL_GATEWAY_URL?: string
  readonly TOOL_GATEWAY_CLIENT_ID?: string
  readonly TOOL_GATEWAY_CLIENT_SECRET?: string
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

function auth(env: Env): Auth {
  const known = instances.get(env.AUTH_DB)
  if (known !== undefined) return known
  const { EMAIL: mail, EMAIL_FROM: from } = env
  const sendMagicLink = mail === undefined || from === undefined ? undefined : async ({ to, url }: MagicLinkMail) => {
    await mail.send({ to, from, subject: "Sign in to Norn Studio", text: `Sign in to Norn Studio: ${url}\n\nThe link expires in 10 minutes. If you did not ask for it, ignore this email.` })
  }
  const { TOOL_GATEWAY_URL: url, TOOL_GATEWAY_CLIENT_ID: clientId, TOOL_GATEWAY_CLIENT_SECRET: clientSecret } = env
  const gateway = url === undefined || clientId === undefined || clientSecret === undefined ? undefined : { url, clientId, clientSecret }
  const instance = createAuth({ database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: env.BETTER_AUTH_URL, trustedOrigins: trustedOrigins(env), defaultEnvironmentId: env.STUDIO_DEFAULT_ENVIRONMENT_ID?.trim() || undefined, scrypt: passwordScrypt(env.AUTH_SCRYPT), ...sendMagicLink === undefined ? {} : { sendMagicLink }, ...env.STUDIO_DEFAULT_ORG === undefined ? {} : { defaultOrganizationId: env.STUDIO_DEFAULT_ORG }, ...gateway === undefined ? {} : { gateway } })
  instances.set(env.AUTH_DB, instance)
  return instance
}

/**
 * The public answer to a successful sign-up: the same keys in the same order for a new and a
 * registered address, a placeholder id, no server timestamps and no cookie. Better Auth's own
 * body differs between the two (a real row vs a synthetic one, createdAt taken at different points).
 */
export async function publicSignUp(response: Response): Promise<Response> {
  if (!response.ok) return response
  const { user } = (await response.json()) as { user: { email: string; name: string } }
  const body = { token: null, user: { id: crypto.randomUUID(), email: user.email, name: user.name, emailVerified: false } }
  return Response.json(body, { status: 200, headers: { "cache-control": "no-store" } })
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

export default {
  async fetch(request, env) {
    const { pathname } = new URL(request.url)
    if (!pathname.startsWith("/api/")) return env.ASSETS.fetch(request)
    if (ACTOR_HEADERS.some((name) => request.headers.has(name))) return refuse(400, "Identity comes from the signed-in session, not a request header.")
    if (pathname === "/api/auth/sign-in/magic-link" || pathname === "/api/auth/magic-link/verify") {
      const host = new URL(request.url).host
      if ([request.headers.get("host"), request.headers.get("x-forwarded-host")].some((value) => value !== null && value.toLowerCase() !== host.toLowerCase())) return refuse(403, "Magic link host does not match this origin.")
    }
    if (pathname === "/api/auth/magic-link/verify" && !(await verifyMagicLinkOrigin(request, env.BETTER_AUTH_SECRET, trustedOrigins(env)))) return refuse(403, "Magic link is not valid for this origin.")
    if (pathname === "/api/auth/sign-up/email") return publicSignUp(await auth(env).handler(request))
    // The gateway provider is link-only: refuse every sign-in that could start from it (social and
    // generic OAuth), so a linked gateway identity never yields a Studio session. Linking stays.
    if (SIGN_IN_ELSEWHERE.has(pathname)) return refuse(403, "Studio sign-in is by email and password or magic link.")
    if (pathname.startsWith("/api/auth/")) return auth(env).handler(request)
    if (pathname === "/api/me") return me(request, env)
    // Every Norn call goes to the host as the resolved actor; the host and its PDP decide.
    if (pathname.startsWith("/api/host/")) {
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      if (!originAllowed(request, actor, trustedOrigins(env))) return refuse(403, "Cross-origin request refused.")
      if (env.NORN_HOST === undefined) return refuse(503, "No Norn host is connected to this deployment.")
      return forwardToHost(request, env.NORN_HOST, auth(env), actor, pathname.slice("/api/host".length))
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
