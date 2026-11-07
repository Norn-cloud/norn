import { activeMembership, type Auth } from "./auth.ts"

/**
 * The data environment (cell) a workspace's data lives in, from Worker-held configuration only: a JSON map from
 * organization id to environment id (`STUDIO_WORKSPACE_ENVIRONMENTS`), else the preview default. Nothing the client
 * sends is read. A malformed map, an unmapped workspace or an empty value fails closed: no environment.
 */
const DEFAULT_WORKSPACE_ENVIRONMENTS: Readonly<Record<string, string>> = { org_demo: "analytics" }
export function workspaceEnvironment(orgId: string, configured?: string): string | null {
  let mapping: unknown = DEFAULT_WORKSPACE_ENVIRONMENTS
  if (configured !== undefined) {
    try { mapping = JSON.parse(configured) } catch { return null }
  }
  if (typeof mapping !== "object" || mapping === null || Array.isArray(mapping) || !Object.hasOwn(mapping, orgId)) return null
  const value = (mapping as Record<string, unknown>)[orgId]
  return typeof value === "string" && value.trim() !== "" ? value : null
}

/**
 * Who a request acts as, resolved on the server from the verified session or
 * API key, never from anything the client asserts. The Norn host turns this
 * into a principal through its declared role table (norn-api `principal`);
 * Norn's AccessIR/Cedar policy decides what it may do.
 */
export interface NornActor {
  readonly actorKind: "user" | "service"
  /** The Better Auth user id, or the API key id for a service. */
  readonly subjectId: string
  /** The organization the request is scoped to. */
  readonly orgId: string
  /** The membership role in that organization (owner/admin/member/viewer). */
  readonly role: string
  readonly authMethod: "password" | "magic_link" | "api_key"
}

/** Roles an organization API key may act with; approving roles stay with people. */
export const SERVICE_ROLES = ["viewer", "member"] as const
/** Host tokens live five minutes, the most the host admits. */
export const HOST_TOKEN_SECONDS = 300

const USER_METHODS = new Set(["password", "magic_link"])

/**
 * The request's actor, or null when it has no usable identity: no session,
 * a session without an organization or sign-in method, an invalid key, or a
 * key that asks for a role a service may not hold.
 */
export async function resolveActor(request: Request, auth: Auth): Promise<NornActor | null> {
  const key = request.headers.get("x-api-key")
  if (key !== null) {
    const verified = await auth.api.verifyApiKey({ body: { key } })
    if (!verified.valid || verified.key === null) return null
    const requested = (verified.key.metadata as { role?: unknown } | null)?.role ?? "viewer"
    if (!SERVICE_ROLES.some((role) => role === requested)) return null
    return { actorKind: "service", subjectId: verified.key.id, orgId: verified.key.referenceId, role: String(requested), authMethod: "api_key" }
  }
  const session = await auth.api.getSession({ headers: request.headers })
  if (session === null) return null
  const method = (session.session as { authMethod?: string | null }).authMethod
  if (method == null || !USER_METHODS.has(method)) return null
  const member = await activeMembership(auth, session.user.id, session.session.activeOrganizationId)
  if (member === null) return null
  return { actorKind: "user", subjectId: session.user.id, orgId: member.organizationId, role: member.role, authMethod: method as NornActor["authMethod"] }
}

/** A short-lived signed token for the Norn host carrying exactly this actor (audience `norn-host`). */
export async function hostToken(auth: Auth, actor: NornActor, now = Math.floor(Date.now() / 1000)): Promise<string> {
  const { token } = await auth.api.signJWT({
    body: {
      payload: {
        sub: actor.subjectId,
        iat: now,
        exp: now + HOST_TOKEN_SECONDS,
        norn: { kind: actor.actorKind, org: actor.orgId, role: actor.role, amr: actor.authMethod },
      },
    },
  })
  return token
}

const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"])

/**
 * Whether a request may reach the host as its actor. A cookie-authenticated
 * (user) request that can change state must come from the Studio origin itself:
 * a sibling *.norn.cloud site is same-site, so the cookie alone proves nothing.
 * A missing or "null" Origin refuses. API-key (service) calls carry their own
 * credential and need no Origin.
 */
export function originAllowed(request: Request, actor: NornActor, studioOrigin: string): boolean {
  if (actor.actorKind === "service" || SAFE_METHODS.has(request.method)) return true
  return request.headers.get("origin") === studioOrigin
}

/**
 * Forward a request to the Norn host as its actor. Only the method, path,
 * query, body and content headers travel; cookies, the API key and any
 * client Authorization are replaced by the host token.
 */
export async function forwardToHost(request: Request, host: Fetcher, auth: Auth, actor: NornActor, path: string): Promise<Response> {
  const url = new URL(request.url)
  const headers = new Headers({ authorization: `Bearer ${await hostToken(auth, actor)}` })
  for (const name of ["content-type", "accept"]) {
    const value = request.headers.get(name)
    if (value !== null) headers.set(name, value)
  }
  // Streamed, never buffered in the Worker.
  const body = request.method === "GET" || request.method === "HEAD" ? null : request.body
  return host.fetch(new Request(`https://norn-host${path}${url.search}`, { method: request.method, headers, body }))
}
