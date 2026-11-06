import { forwardToHost, originAllowed, resolveActor } from "./actor.ts"
import { activeMembership, createAuth, type Auth, type MagicLinkMail } from "./auth.ts"
import type { AgentRequest } from "./agent.ts"
import type { AgentBudget } from "./budget.ts"
import type { Scope } from "./limits.ts"

export { AgentBudget } from "./budget.ts"

/** The Studio Worker: Better Auth under /api/auth, authenticated /api/* (the assistant included), static assets for everything else. */
export interface Env {
  readonly ASSETS: Fetcher
  /** D1 on Cloudflare; any database Better Auth's Kysely adapter accepts (bun:sqlite in tests). */
  readonly AUTH_DB: D1Database
  readonly BETTER_AUTH_SECRET: string
  readonly BETTER_AUTH_URL: string
  /** Cloudflare Email Service binding and sender; both absent until a sender is enabled. */
  readonly EMAIL?: { send(message: { to: string; from: string; subject: string; text: string }): Promise<unknown> }
  readonly EMAIL_FROM?: string
  /** The Norn cell host (service binding); absent until a host is connected to this stage. */
  readonly NORN_HOST?: Fetcher
  readonly AGENT_BUDGET?: DurableObjectNamespace<AgentBudget>
  /** Deploy-time secrets; absent when the stage runs without the assistant. */
  readonly AI_GATEWAY_URL?: string
  readonly AI_GATEWAY_TOKEN?: string
  readonly AGENT_MODEL?: string
  /** The organization members' sessions start in (preview: the seeded `org_demo`). */
  readonly STUDIO_DEFAULT_ORG?: string
}

export const refuse = (status: number, reason: string, headers: HeadersInit = {}) =>
  Response.json({ error: reason }, { status, headers: { "cache-control": "no-store", ...headers } })

/** Identity reaches Norn only from the verified session; a client-supplied actor header is refused on every stage. */
export const ACTOR_HEADERS = ["x-norn-actor", "x-norn-principal", "x-norn-roles"] as const

// One Better Auth instance per database binding (per isolate in production).
const instances = new WeakMap<object, Auth>()

function auth(env: Env): Auth {
  const known = instances.get(env.AUTH_DB)
  if (known !== undefined) return known
  const { EMAIL: mail, EMAIL_FROM: from } = env
  const sendMagicLink = mail === undefined || from === undefined ? undefined : async ({ to, url }: MagicLinkMail) => {
    await mail.send({ to, from, subject: "Sign in to Norn Studio", text: `Sign in to Norn Studio: ${url}\n\nThe link expires in 10 minutes. If you did not ask for it, ignore this email.` })
  }
  const instance = createAuth({ database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: env.BETTER_AUTH_URL, ...sendMagicLink === undefined ? {} : { sendMagicLink }, ...env.STUDIO_DEFAULT_ORG === undefined ? {} : { defaultOrganizationId: env.STUDIO_DEFAULT_ORG } })
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
    { user: { id: session.user.id, name: session.user.name, email: session.user.email }, organization: member === null ? null : { id: member.organizationId, name: member.organizationName, role: member.role } },
    { headers: { "cache-control": "no-store" } },
  )
}

const MAX_BODY = 64 * 1024
const SESSION = /^[0-9a-f-]{36}$/

const digest = async (text: string) =>
  [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)))].map((byte) => byte.toString(16).padStart(2, "0")).join("")

/** The assistant, for a signed-in actor only; its budget is charged per chat session, per visitor and overall. */
async function agent(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  if (request.method !== "POST") return refuse(405, "Use POST.", { allow: "POST" })
  const { AGENT_BUDGET: budget, AI_GATEWAY_URL: url, AI_GATEWAY_TOKEN: token } = env
  if (budget === undefined || url === undefined || token === undefined) return refuse(503, "The assistant is not configured on this deployment.")
  const session = request.headers.get("x-norn-session") ?? ""
  if (!SESSION.test(session)) return refuse(400, "Missing session.")
  const length = Number(request.headers.get("content-length") ?? "0")
  if (length > MAX_BODY) return refuse(413, "The conversation is too long.")
  const text = await request.text()
  if (text.length > MAX_BODY) return refuse(413, "The conversation is too long.")
  let body: AgentRequest
  try {
    body = JSON.parse(text) as AgentRequest
    if (!Array.isArray(body.messages)) throw new Error("messages")
  } catch {
    return refuse(400, "Expected {messages: [...]}.")
  }
  const visitor = await digest(request.headers.get("cf-connecting-ip") ?? "unknown")
  const scopes: [Scope, string][] = [["session", `session:${session}`], ["visitor", `visitor:${visitor}`], ["global", "global"]]
  const ledgers = scopes.map(([scope, name]) => [scope, budget.get(budget.idFromName(name))] as const)
  for (const [scope, ledger] of ledgers) {
    const admission = await ledger.admit(scope)
    if (!admission.ok) return refuse(429, admission.reason, { "retry-after": String(admission.retryAfterSeconds) })
  }
  // Loaded on first use: the assistant brings the model SDK and the checker's WebAssembly, which sign-in never needs.
  const { gatewayModel, respond } = await import("./agent.ts")
  const model = gatewayModel(url, token, env.AGENT_MODEL ?? "claude-opus-5-5")
  return respond(model, body, (tokens) => ctx.waitUntil(Promise.all(ledgers.map(([, ledger]) => ledger.spend(tokens)))), request.signal)
}

export default {
  async fetch(request, env, ctx) {
    const { pathname } = new URL(request.url)
    if (!pathname.startsWith("/api/")) return env.ASSETS.fetch(request)
    if (ACTOR_HEADERS.some((name) => request.headers.has(name))) return refuse(400, "Identity comes from the signed-in session, not a request header.")
    if (pathname === "/api/auth/sign-up/email") return publicSignUp(await auth(env).handler(request))
    if (pathname.startsWith("/api/auth/")) return auth(env).handler(request)
    if (pathname === "/api/me") return me(request, env)
    // Every Norn call goes to the host as the resolved actor; the host and its PDP decide.
    if (pathname.startsWith("/api/host/")) {
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      if (!originAllowed(request, actor, env.BETTER_AUTH_URL)) return refuse(403, "Cross-origin request refused.")
      if (env.NORN_HOST === undefined) return refuse(503, "No Norn host is connected to this deployment.")
      return forwardToHost(request, env.NORN_HOST, auth(env), actor, pathname.slice("/api/host".length))
    }
    if (pathname === "/api/agent") {
      const actor = await resolveActor(request, auth(env))
      if (actor === null) return refuse(401, "Sign in first.")
      // The assistant is for people; organization API keys may not spend its budget.
      if (actor.actorKind !== "user") return refuse(403, "The assistant is for signed-in users, not API keys.")
      if (!originAllowed(request, actor, env.BETTER_AUTH_URL)) return refuse(403, "Cross-origin request refused.")
      return agent(request, env, ctx)
    }
    const session = await auth(env).api.getSession({ headers: request.headers })
    if (session === null) return refuse(401, "Sign in first.")
    return refuse(404, "No such endpoint.")
  },
} satisfies ExportedHandler<Env>
