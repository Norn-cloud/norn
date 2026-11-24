import { activeMembership, isProvisioningMetadata, type Auth } from "./auth.ts"
import { cellScope } from "./scope.ts"

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
  /** The membership row id (generation) for user actors; changes on revoke+rejoin. */
  readonly memberId?: string
  /** The membership role in that organization (owner/admin/member/viewer). */
  readonly role: string
  /** `oauth`: an assistant the person connected, acting on their grant (MCP). */
  readonly authMethod: "password" | "magic_link" | "api_key" | "oauth"
}

/** Roles an organization API key may act with; approving roles stay with people. */
export const SERVICE_ROLES = ["viewer", "member"] as const
/** Host tokens live five minutes, the most the host admits. */
export const HOST_TOKEN_SECONDS = 300
/** Client-asserted displayed scope headers for stale-tab write protection (never grants access). */
export const EXPECTED_ORG_HEADER = "x-norn-expected-org"
export const EXPECTED_ENV_HEADER = "x-norn-expected-env"
export const WORKSPACE_CHANGED_MESSAGE = "Your active workspace changed. Reload the page before submitting."
export const SCOPE_REQUIRED_MESSAGE = "Workspace scope headers are required for mutations."
export const NO_DATA_CELL_MESSAGE = "This workspace has no data cell yet."

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
    const { adapter } = await auth.$context
    const org = await adapter.findOne<{ metadata?: unknown }>({ model: "organization", where: [{ field: "id", value: verified.key.referenceId }] })
    if (org === null || isProvisioningMetadata(org.metadata)) return null
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
  return { actorKind: "user", subjectId: session.user.id, orgId: member.organizationId, memberId: member.memberId, role: member.role, authMethod: method as NornActor["authMethod"] }
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

export interface ValidatedWriteScope {
  readonly orgId: string
  readonly environmentId: string | null
}

export type WriteScopeCheck =
  | { readonly ok: true; readonly scope?: ValidatedWriteScope }
  | { readonly ok: false; readonly response: Response }

const nonEmpty = (value: string | null): string | null =>
  value !== null && value.trim() !== "" ? value : null

/**
 * Verify that a mutation targets the exact workspace scope the caller displayed, and resolve
 * the canonical server-validated scope to forward.
 *
 * Cookie-authenticated (`user`) mutations MUST supply `x-norn-expected-org` (and `x-norn-expected-env`
 * when `requireEnv` is true, e.g. `/api/host/*` and cell-backed routes). Missing assertions are
 * refused 400 `scope.required`; any disagreement between expected headers, forwarded query params,
 * and the server-resolved active workspace/environment is refused 409 `workspace.changed` with zero
 * calls to the host or database.
 */
export async function verifyWriteScope(
  request: Request,
  auth: Auth,
  actor: NornActor,
  options: { readonly checkHostQuery?: boolean; readonly requireEnv?: boolean } = {},
): Promise<WriteScopeCheck> {
  const upgrade = request.headers.get("upgrade")?.toLowerCase() === "websocket"
  const safeRead = SAFE_METHODS.has(request.method) && !upgrade
  const mismatch = () => ({
    ok: false as const,
    response: Response.json(
      { code: "workspace.changed", message: WORKSPACE_CHANGED_MESSAGE, error: WORKSPACE_CHANGED_MESSAGE },
      { status: 409, headers: { "cache-control": "no-store" } },
    ),
  })
  const required = () => ({
    ok: false as const,
    response: Response.json(
      { code: "scope.required", message: SCOPE_REQUIRED_MESSAGE, error: SCOPE_REQUIRED_MESSAGE },
      { status: 400, headers: { "cache-control": "no-store" } },
    ),
  })
  const noCell = () => ({
    ok: false as const,
    response: Response.json(
      { code: "workspace.no_cell", message: NO_DATA_CELL_MESSAGE, error: NO_DATA_CELL_MESSAGE },
      { status: 409, headers: { "cache-control": "no-store" } },
    ),
  })

  const headerOrg = nonEmpty(request.headers.get(EXPECTED_ORG_HEADER))
  const headerEnv = nonEmpty(request.headers.get(EXPECTED_ENV_HEADER))

  // On safe reads outside `/api/host/*`, still validate any explicit expected-scope headers sent
  // by a tab (e.g. connector GETs) so a stale tab never caches another workspace's data.
  if (safeRead && !options.checkHostQuery) {
    if (headerOrg === null && headerEnv === null) return { ok: true }
    if (headerOrg !== null && headerOrg !== actor.orgId) return mismatch()
    if (headerEnv !== null) {
      const scope = await cellScope(auth, actor.orgId)
      if (scope.environmentId !== headerEnv) return mismatch()
      return { ok: true, scope: { orgId: actor.orgId, environmentId: scope.environmentId } }
    }
    return { ok: true }
  }

  const url = options.checkHostQuery || upgrade ? new URL(request.url) : null

  // For `/api/host/*` (reads, writes, WebSocket upgrades, users and service keys), ALWAYS resolve
  // and validate the canonical organization + environment before forwarding to the host (S6).
  if (safeRead && options.checkHostQuery) {
    const scope = await cellScope(auth, actor.orgId)
    if (options.requireEnv && scope.environmentId === null) return noCell()
    if (headerOrg !== null && headerOrg !== actor.orgId) return mismatch()
    if (headerEnv !== null && scope.environmentId !== headerEnv) return mismatch()
    if (url !== null) {
      const queryOrgs = url.searchParams.getAll("orgId")
      if (queryOrgs.some((org) => org !== actor.orgId)) return mismatch()
      const queryEnvs = url.searchParams.getAll("environmentId")
      if (queryEnvs.some((env) => env !== scope.environmentId)) return mismatch()
    }
    return { ok: true, scope: { orgId: actor.orgId, environmentId: scope.environmentId } }
  }

  if (upgrade) {
    // Browser WebSocket upgrades cannot send custom headers, so query scope is validated directly
    // against the canonical server session workspace/environment and rewritten before forwarding.
    const scope = await cellScope(auth, actor.orgId)
    if (options.requireEnv && scope.environmentId === null) return noCell()
    if (url !== null) {
      const queryOrgs = url.searchParams.getAll("orgId")
      if (actor.actorKind === "user") {
        if (queryOrgs.length === 0) return required()
        if (queryOrgs.some((org) => org !== actor.orgId)) return mismatch()
      } else if (queryOrgs.some((org) => org !== actor.orgId)) {
        return mismatch()
      }
      const queryEnvs = url.searchParams.getAll("environmentId")
      if (actor.actorKind === "user" && options.requireEnv) {
        if (queryEnvs.length === 0) return required()
        if (queryEnvs.some((env) => env !== scope.environmentId)) return mismatch()
      } else if (queryEnvs.some((env) => env !== scope.environmentId)) {
        return mismatch()
      }
    }
    return { ok: true, scope: { orgId: actor.orgId, environmentId: scope.environmentId } }
  }
  if (actor.actorKind !== "user") {
    // Service API keys are bound to a single organization and its canonical cell environment:
    // never forward a caller-chosen environmentId when the org has no cell or a different cell.
    const scope = options.requireEnv || options.checkHostQuery || headerEnv !== null
      ? await cellScope(auth, actor.orgId)
      : { orgId: actor.orgId, environmentId: null }
    if (options.requireEnv && scope.environmentId === null) return noCell()
    if (headerOrg !== null && headerOrg !== actor.orgId) return mismatch()
    if (headerEnv !== null && scope.environmentId !== headerEnv) return mismatch()
    if (url !== null) {
      const queryOrgs = url.searchParams.getAll("orgId")
      if (queryOrgs.some((org) => org !== actor.orgId)) return mismatch()
      const queryEnvs = url.searchParams.getAll("environmentId")
      if (queryEnvs.some((env) => env !== scope.environmentId)) return mismatch()
    }
    return { ok: true, scope: { orgId: actor.orgId, environmentId: scope.environmentId } }
  }

  if (headerOrg === null) return required()
  if (headerOrg !== actor.orgId) return mismatch()

  let environmentId: string | null = null
  if (options.requireEnv || headerEnv !== null || options.checkHostQuery) {
    const scope = await cellScope(auth, actor.orgId)
    environmentId = scope.environmentId
    if (options.requireEnv && environmentId === null) return noCell()
    if (options.requireEnv && headerEnv === null) return required()
    if (headerEnv !== null && environmentId !== headerEnv) return mismatch()
  }

  if (url !== null) {
    const queryOrgs = url.searchParams.getAll("orgId")
    if (queryOrgs.some((org) => org !== actor.orgId)) return mismatch()
    const queryEnvs = url.searchParams.getAll("environmentId")
    if (queryEnvs.some((env) => env !== environmentId)) return mismatch()
  }

  return { ok: true, scope: { orgId: actor.orgId, environmentId } }
}

/**
 * Whether a request may reach the host as its actor. A cookie-authenticated
 * (user) request that can change state must come from the Studio origin itself:
 * a sibling *.norn.cloud site is same-site, so the cookie alone proves nothing.
 * A missing or "null" Origin refuses. API-key (service) calls carry their own
 * credential and need no Origin.
 */
export function originAllowed(request: Request, actor: NornActor, studioOrigins: readonly string[]): boolean {
  // A WebSocket upgrade is a GET the browser makes cross-site with the cookie and lets the page
  // read: a live socket (UI-LIVE-1) must come from the Studio origin like any state change.
  const upgrade = request.headers.get("upgrade")?.toLowerCase() === "websocket"
  if (actor.actorKind === "service" || (SAFE_METHODS.has(request.method) && !upgrade)) return true
  return studioOrigins.includes(request.headers.get("origin") ?? "")
}

/**
 * Forward a request to the Norn host as its actor. Only the method, path,
 * query, body and content headers travel; cookies, the API key and any
 * client Authorization are replaced by the host token. When `validatedScope`
 * is provided, `orgId` and `environmentId` on the forwarded URL are bound to
 * that server-validated scope.
 */
export async function forwardToHost(
  request: Request,
  host: Fetcher,
  auth: Auth,
  actor: NornActor,
  path: string,
  validatedScope?: ValidatedWriteScope,
): Promise<Response> {
  const url = new URL(request.url)
  const target = new URL(`https://norn-host${path}${url.search}`)
  if (validatedScope !== undefined) {
    target.searchParams.set("orgId", validatedScope.orgId)
    if (validatedScope.environmentId !== null) target.searchParams.set("environmentId", validatedScope.environmentId)
  }
  const headers = new Headers({ authorization: `Bearer ${await hostToken(auth, actor)}` })
  // A live socket (UI-LIVE-1) passes through with its handshake headers; the host answers it.
  if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
    for (const name of ["upgrade", "connection", "sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol"]) {
      const value = request.headers.get(name)
      if (value !== null) headers.set(name, value)
    }
    return host.fetch(new Request(target.href, { method: "GET", headers }))
  }
  for (const name of ["content-type", "accept"]) {
    const value = request.headers.get(name)
    if (value !== null) headers.set(name, value)
  }
  // Streamed, never buffered in the Worker.
  const body = request.method === "GET" || request.method === "HEAD" ? null : request.body
  const response = await host.fetch(new Request(target.href, { method: request.method, headers, body }))
  // The cell has already authorized this definition read. Add a display-only name
  // without changing its recorded signer or exposing users from other organizations.
  if (request.method !== "GET" || !/^\/v1\/definitions\/[^/:]+$/.test(path) || !response.ok) return response
  // Read once: a streamed host body cannot be cloned before it is consumed
  // (the cell runtime refuses), so every outcome below re-serves these bytes.
  const text = await response.text()
  const unchanged = () => new Response(text, { status: response.status, statusText: response.statusText, headers: response.headers })
  let value: unknown = null
  try { value = JSON.parse(text) } catch { return unchanged() }
  if (typeof value !== "object" || value === null || Array.isArray(value) || !("signoff" in value)) return unchanged()
  const signoff = value.signoff
  if (typeof signoff !== "object" || signoff === null || Array.isArray(signoff) || !("signer" in signoff) || typeof signoff.signer !== "string") return unchanged()
  const userId = signoff.signer.startsWith("user:") ? signoff.signer.slice(5) : ""
  let displayName = "a removed user"
  if (userId && actor.actorKind === "user" && actor.subjectId === userId) displayName = "you"
  else if (userId) {
    const { adapter } = await auth.$context
    const member = await adapter.findOne<{ userId: string }>({ model: "member", where: [{ field: "userId", value: userId }, { field: "organizationId", value: actor.orgId }] })
    if (member) {
      const user = await adapter.findOne<{ name: string }>({ model: "user", where: [{ field: "id", value: userId }] })
      if (typeof user?.name === "string" && user.name.trim()) displayName = user.name
    }
  }
  const resultHeaders = new Headers(response.headers)
  resultHeaders.delete("content-length"); resultHeaders.delete("content-encoding"); resultHeaders.delete("etag")
  return Response.json({ ...value, signoff: { ...signoff, displayName } }, { status: response.status, statusText: response.statusText, headers: resultHeaders })
}
