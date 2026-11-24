import { AsyncLocalStorage } from "node:async_hooks"
import { apiKey } from "@better-auth/api-key"
import { cimd } from "@better-auth/cimd"
import { mcp } from "@better-auth/mcp"
import { betterAuth, getCurrentAdapter, type BetterAuthOptions } from "better-auth"
import { APIError, createAuthMiddleware, getSessionFromCtx } from "better-auth/api"
import { setSessionCookie } from "better-auth/cookies"
import { generateRandomString } from "better-auth/crypto"
import { createAccessControl } from "better-auth/plugins/access"
import { genericOAuth } from "better-auth/plugins/generic-oauth"
import { jwt } from "better-auth/plugins/jwt"
import { magicLink } from "better-auth/plugins/magic-link"
import { organization } from "better-auth/plugins/organization"
import { adminAc, defaultStatements, memberAc, ownerAc } from "better-auth/plugins/organization/access"
import { z } from "zod"
import { hashPassword, verifyPassword, type PasswordScrypt } from "./password.ts"
import type { ClientIpHeader } from "./client-ip.ts"
import { admitMember, billingModeOn, billingOf, limitRaised, refusalMessage, type Admission, type BillingDb, type BillingSettings } from "./billing.ts"
import { forgetPolarCustomer, polarPlugin, type PolarBilling } from "./polar.ts"
import { settingsOf } from "./polar-config.ts"

/**
 * Better Auth for the hosted Studio: email + password, email magic link,
 * organizations (owner/admin/member/viewer) and organization-owned API keys
 * for service accounts. Identity only: what each role may do in Norn is
 * decided by Norn's AccessIR/Cedar policy on the server, never here or in the client.
 */

/** The sign-in path that created a session, as Norn's `actor.authMethod`. */
export const AUTH_METHODS = { "/sign-up/email": "password", "/sign-in/email": "password", "/magic-link/verify": "magic_link" } as const
export type AuthMethod = (typeof AUTH_METHODS)[keyof typeof AUTH_METHODS] | "api_key" | "oauth"

// Better Auth's own permissions cover organization administration only
// (members, invitations, API keys). Viewers and members administer nothing.
const ac = createAccessControl({ ...defaultStatements, apiKey: ["create", "read", "update", "delete"] })
const manageKeys = { apiKey: ["create", "read", "update", "delete"] } as const
export const ORG_ROLES = {
  owner: ac.newRole({ ...ownerAc.statements, ...manageKeys }),
  admin: ac.newRole({ ...adminAc.statements, ...manageKeys }),
  member: ac.newRole({ ...memberAc.statements }),
  viewer: ac.newRole({}),
}
export type OrgRole = keyof typeof ORG_ROLES

export interface MagicLinkMail {
  readonly to: string
  readonly url: string
}

export interface AuthConfig {
  /** A D1 database (Worker) or bun:sqlite (schema generation). */
  readonly database: BetterAuthOptions["database"]
  readonly secret: string
  /** The Studio origin, e.g. https://studio-preview.norn.cloud. */
  readonly baseURL: string
  /** Optional server-held default cell for newly provisioned workspaces. */
  readonly defaultEnvironmentId?: string
  readonly trustedOrigins?: readonly string[]
  /** Absent when this deployment has no mail sender: magic links then refuse. */
  readonly sendMagicLink?: (mail: MagicLinkMail) => Promise<void>
  /**
   * The organization a new session starts in when its user is a member, e.g. the
   * seeded `org_demo` on preview; otherwise the user's earliest membership.
   */
  readonly defaultOrganizationId?: string
  /** "js" where the runtime has no `node:crypto.scrypt` (celld); absent or "native" otherwise. */
  readonly scrypt?: PasswordScrypt
  /**
   * Invite-only sign-up: when set, only these addresses (lower-case) may create an account, by
   * any sign-up path; every other address is refused before the user exists. Absent: open.
   */
  readonly signUpEmails?: readonly string[]
  /** The tool gateway (#772) people link to their Norn account; absent on stages without one. */
  readonly gateway?: GatewayClient
  /**
   * Serve `<baseURL>/mcp` to assistants people connect (Claude, ChatGPT): Better Auth becomes
   * their OAuth 2.1 authorization server. Needs migration 0007_oauth_provider.
   */
  readonly mcp?: boolean
  /** The header the edge sets to the client's address (rate limits); never defaulted. */
  readonly clientIpHeader: ClientIpHeader
  /** Hosted billing through Polar (polar.ts); absent, nothing is billed or limited. */
  readonly billing?: PolarBilling
}

/** A plan limit refuses as a typed 403 the client renders as given: `code` PLAN_LIMIT and the `refusal`. */
function enforce(admission: Admission): void {
  if (admission.kind === "admitted") return
  throw new APIError("FORBIDDEN", { code: "PLAN_LIMIT", message: refusalMessage(admission), refusal: admission })
}

/** Where assistants reach Norn, and the resource their access tokens are bound to (RFC 8707). */
export const MCP_PATH = "/mcp"
export const mcpResource = (baseURL: string) => new URL(MCP_PATH, baseURL).href

/** The access-token claim carrying the organization the person consented for. */
export const ORG_CLAIM = "https://norn.cloud/org"
/** The access-token claim carrying the membership row id (generation) that authorized the grant. */
export const MEMBER_CLAIM = "https://norn.cloud/member"
/** Optional access-token claim pinning a Sidekick grant to a specific cell environment. */
export const ENV_CLAIM = "https://norn.cloud/env"
/** Header carrying the single-use consent approval token minted by POST /api/oauth/consent-context. */
export const CONSENT_APPROVAL_HEADER = "x-norn-consent-approval"
export const CONSENT_APPROVAL_TTL_MS = 5 * 60_000

/**
 * Callbacks an assistant may receive a person's authorization code on. Registration (dynamic or
 * by metadata document) cannot add others, so a look-alike client cannot collect codes elsewhere.
 * Loopback callbacks serve local tools (Claude Code, the MCP inspector).
 */
export const ASSISTANT_CALLBACKS: readonly string[] = [
  "https://claude.ai/api/mcp/auth_callback",
  "https://claude.com/api/mcp/auth_callback",
  "https://chatgpt.com/connector_platform_oauth_redirect",
]
export function assistantCallback(uri: string): boolean {
  if (ASSISTANT_CALLBACKS.includes(uri)) return true
  try {
    const url = new URL(uri)
    if (url.username || url.password || url.hash) return false
    if (url.origin === "https://chatgpt.com") return /^\/connector\/oauth\/[A-Za-z0-9_-]+$/.test(url.pathname) && url.search === ""
    return url.protocol === "http:" && (url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]")
  } catch {
    return false
  }
}

/** Origins whose client metadata documents (CIMD) are fetched; any other client_id URL is refused unfetched. */
export const CIMD_ORIGINS: ReadonlySet<string> = new Set(["https://claude.ai", "https://claude.com", "https://chatgpt.com"])
export function cimdAllowed(url: string): boolean {
  try {
    const parsed = new URL(url)
    return parsed.protocol === "https:" && CIMD_ORIGINS.has(parsed.origin) && !parsed.username && !parsed.password
  } catch {
    return false
  }
}

/** Fetch a client metadata document from an allowed origin only, never following a redirect. */
export async function fetchClientMetadata(input: RequestInfo | URL, init?: RequestInit, fetcher: typeof fetch = fetch): Promise<Response> {
  const url = input instanceof Request ? input.url : String(input)
  if (!cimdAllowed(url)) throw new Error("client metadata documents are fetched only from known assistants")
  return fetcher(url, { ...init, redirect: "manual", signal: init?.signal ?? AbortSignal.timeout(5000) })
}

/**
 * Per-request pinned OAuth consent reference: once `hooks.before` validates `x-norn-expected-org`
 * on `POST /oauth2/consent` (or `/oauth2/authorize` resolves one ready workspace), every subsequent
 * `postLogin.consentReferenceId` lookup within that same request stays bound to that exact workspace
 * (re-verifying that specific membership/readiness, never falling back to a different workspace).
 */
/** Encode an immutable OAuth grant reference binding organization + membership generation. */
export const encodeGrantRef = (organizationId: string, memberId: string): string => `${organizationId}|${memberId}`

/** Parse an immutable OAuth grant reference; returns null for legacy bare-org references without a generation. */
export function parseGrantRef(referenceId: unknown): { readonly organizationId: string; readonly memberId: string } | null {
  if (typeof referenceId !== "string") return null
  const parts = referenceId.split("|")
  if (parts.length !== 2) return null
  const [organizationId, memberId] = parts
  if (!organizationId || !memberId || organizationId.trim() === "" || memberId.trim() === "") return null
  return { organizationId, memberId }
}

interface ConsentSnapshot {
  readonly id: string
  readonly scopes: string
  readonly resources: string | null
  readonly requestedUserInfoClaims: string | null
  readonly updatedAt: string | number
}

interface OAuthConsentScope {
  referenceId?: string
  memberId?: string
  claimedApprovalId?: string
  consentUserId?: string
  consentClientId?: string
  isConsentAccept?: boolean
  previousConsent?: ConsentSnapshot | null
}
const oauthScopeStore = new AsyncLocalStorage<OAuthConsentScope>()

async function sha256Hex(value: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)))
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")
}

/**
 * Mint a short-lived, single-use consent approval token bound to the caller's live session,
 * user, client, active ready workspace, membership generation, and exact signed `oauth_query`.
 * Persisted conditionally so a revoked/expired session or revoked membership writes nothing.
 */
export async function issueConsentApproval(
  db: ProvisionDb,
  args: {
    readonly sessionId: string
    readonly sessionToken: string
    readonly userId: string
    readonly organizationId: string
    readonly memberId: string
    readonly oauthQuery: string
  },
): Promise<string | null> {
  const params = new URLSearchParams(args.oauthQuery)
  const clientId = nonEmptyString(params.get("client_id"))
  const redirectUri = nonEmptyString(params.get("redirect_uri"))
  if (clientId === null || redirectUri === null) return null
  const codeChallenge = nonEmptyString(params.get("code_challenge"))
  const state = nonEmptyString(params.get("state"))
  const token = generateRandomString(43, "a-z", "A-Z", "0-9")
  const [tokenHash, queryHash] = await Promise.all([sha256Hex(token), sha256Hex(args.oauthQuery)])
  const now = new Date()
  const nowIso = now.toISOString()
  const expiresIso = new Date(now.getTime() + CONSENT_APPROVAL_TTL_MS).toISOString()
  const id = crypto.randomUUID()
  const identifier = `oauth-approval:${tokenHash}`
  const grantRef = encodeGrantRef(args.organizationId, args.memberId)
  const payload = JSON.stringify({
    type: "consent_approval",
    sessionId: args.sessionId,
    sessionToken: args.sessionToken,
    userId: args.userId,
    clientId,
    redirectUri,
    codeChallenge,
    state,
    organizationId: args.organizationId,
    memberId: args.memberId,
    grantRef,
    queryHash,
  })
  const inserted = await dbFirst<{ id: string }>(
    db,
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt")
     select ?, ?, ?, ?, ?, ?
     where exists (
       select 1 from "session" s
       where s."id" = ? and s."token" = ? and s."userId" = ? and ${LIVE_SESSION_S_SQL}
     )
     and exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."id" = ? and m."organizationId" = ? and m."userId" = ? and ${READY_ORG_SQL}
     )
     returning "id"`,
    id, identifier, payload, expiresIso, nowIso, nowIso,
    args.sessionId, args.sessionToken, args.userId,
    args.memberId, args.organizationId, args.userId,
  )
  return inserted === null ? null : token
}

/**
 * The OAuth 2.1 authorization server for assistants (MCP authorization). Identity only, like the
 * rest of this file: the token names a person and the organization they consented for; Norn's
 * policy on the server decides every call. Sign-in is Studio's own, and nothing here signs anyone
 * up. A person sees who is asking and for which workspace before any grant.
 *
 * Consent and issued access tokens always bind to one validated ready workspace (carrying the
 * validated `x-norn-expected-org` reference through both consent recording and authorization-code
 * issuance); never to a raw session field, never unscoped, and never drifting on a concurrent
 * membership change.
 */
const assistantAuthorization = (baseURL: string, self: Self) => [
  mcp({
    resource: mcpResource(baseURL),
    loginPage: "/oauth/sign-in",
    consentPage: "/oauth/consent",
    // Public clients register themselves (RFC 7591) or by metadata document (CIMD); either way
    // only the callbacks above can receive a code.
    allowDynamicClientRegistration: true,
    allowUnauthenticatedClientRegistration: true,
    // A registration that names no application_type is typed by its callbacks: a loopback one
    // (Claude Code, the MCP inspector) registers as native, an https one stays web.
    clientRegistrationDefaultApplicationType: "infer",
    validateRedirectUri: (uri: string, _registered: readonly string[], matched: boolean) => matched && assistantCallback(uri),
    accessTokenExpiresIn: 15 * MINUTE,
    // The consent and authorization code bind to the single validated ready workspace.
    postLogin: {
      page: "/oauth/consent",
      consentReferenceId: async ({ user, session }) => {
        const adapter = (await self.auth?.$context)?.adapter as AdapterLike | undefined
        if (adapter === undefined) throw new APIError("INTERNAL_SERVER_ERROR", { message: "Auth context unavailable." })
        const store = oauthScopeStore.getStore()
        const pinned = store?.referenceId
        if (pinned !== undefined) {
          const parsed = parseGrantRef(pinned)
          if (parsed === null) {
            throw new APIError("CONFLICT", { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before authorizing an assistant." })
          }
          // Never re-run null-fallback: confirm this exact validated workspace & membership generation is still ready.
          const verified = await activeMembershipWithAdapter(adapter, user.id, parsed.organizationId)
          if (verified === null || verified.organizationId !== parsed.organizationId || verified.memberId !== parsed.memberId) {
            throw new APIError("CONFLICT", { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before authorizing an assistant." })
          }
          return pinned
        }
        const rawOrg = (session as { activeOrganizationId?: unknown }).activeOrganizationId
        const member = await activeMembershipWithAdapter(adapter, user.id, nonEmptyString(rawOrg))
        if (member === null) {
          throw new APIError("CONFLICT", { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before authorizing an assistant." })
        }
        const grantRef = encodeGrantRef(member.organizationId, member.memberId)
        if (store !== undefined) {
          store.referenceId = grantRef
          store.memberId = member.memberId
        }
        return grantRef
      },
      shouldRedirect: () => false,
    },
    customAccessTokenClaims: async ({ user, referenceId }) => {
      const parsed = parseGrantRef(referenceId)
      if (parsed === null || typeof user?.id !== "string" || user.id === "") {
        throw new APIError("FORBIDDEN", { code: "WORKSPACE_REQUIRED", message: "Assistant access requires a workspace-scoped grant." })
      }
      const adapter = (await self.auth?.$context)?.adapter as AdapterLike | undefined
      if (adapter === undefined) throw new APIError("INTERNAL_SERVER_ERROR", { message: "Auth context unavailable." })
      const member = await activeMembershipWithAdapter(adapter, user.id, parsed.organizationId)
      if (member === null || member.organizationId !== parsed.organizationId || member.memberId !== parsed.memberId) {
        throw new APIError("FORBIDDEN", { code: "WORKSPACE_REQUIRED", message: "Assistant access requires the original active ready workspace membership." })
      }
      return { [ORG_CLAIM]: parsed.organizationId, [MEMBER_CLAIM]: parsed.memberId }
    },
  }),
  cimd({ fetchClientMetadataResource: (input, init) => fetchClientMetadata(input, init), isMetadataDocumentUrlAllowed: cimdAllowed }),
]

/** Studio's OAuth client at the tool gateway (Executor v2). The secret comes from the operator's secret store by name. */
export interface GatewayClient {
  readonly url: string
  readonly clientId: string
  readonly clientSecret: string
}

const MINUTE = 60

/**
 * Link-only OAuth to the gateway: a signed-in person grants Studio their gateway access once,
 * Better Auth keeps the grant encrypted in `account` and refreshes it, and the Worker uses it
 * server-side. It never signs anyone in or creates a Norn user.
 */
/** The resource Studio's gateway tokens are for: the gateway's product API (`/api`), not its MCP endpoint. */
export const gatewayResource = (gateway: GatewayClient) => `${gateway.url}/api`

const gatewayLink = (gateway: GatewayClient) => genericOAuth({
  config: [{
    providerId: "executor",
    clientId: gateway.clientId,
    clientSecret: gateway.clientSecret,
    authorizationUrl: `${gateway.url}/api/auth/oauth2/authorize`,
    tokenUrl: `${gateway.url}/api/auth/oauth2/token`,
    // The gateway grants only these (its /.well-known/oauth-authorization-server); it has no OpenID userinfo.
    scopes: ["executor", "offline_access"],
    pkce: true,
    // Studio's client is registered at the gateway for client_secret_basic, which refuses the secret in the body.
    tokenEndpointAuth: { method: "client_secret_basic" },
    disableSignUp: true,
    disableImplicitSignUp: true,
    // RFC 8707: the gateway issues tokens only for a named resource, its product API.
    authorizationUrlParams: { resource: gatewayResource(gateway) },
    tokenUrlParams: { resource: gatewayResource(gateway) },
    refreshTokenParams: { resource: gatewayResource(gateway) },
    getUserInfo: (tokens) => gatewaySubject(gateway, tokens.accessToken),
  }],
})

/**
 * Who granted the gateway token, from the gateway's token introspection (RFC 7662) with Studio's
 * client credentials. The account is keyed by that subject; the address is a placeholder, since a
 * link-only provider never sets a Norn user's email.
 */
export async function gatewaySubject(gateway: GatewayClient, accessToken: string | undefined, fetcher: typeof fetch = fetch) {
  if (accessToken === undefined) return null
  const response = await fetcher(`${gateway.url}/api/auth/oauth2/introspect`, {
    method: "POST",
    headers: { authorization: `Basic ${btoa(`${gateway.clientId}:${gateway.clientSecret}`)}`, "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ token: accessToken, token_type_hint: "access_token" }),
  })
  const body = (await response.json().catch(() => null)) as { active?: boolean; sub?: string } | null
  if (!response.ok || body?.active !== true || typeof body.sub !== "string") return null
  return { id: body.sub, name: "Tool gateway", email: `${encodeURIComponent(body.sub)}@gateway.invalid`, emailVerified: false }
}
/** Bind the one-time Better Auth token to the origin that requested it. */
async function originProof(secret: string, token: string, origin: string): Promise<string> {
  const encoder = new TextEncoder()
  const key = await crypto.subtle.importKey("raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"])
  const signature = await crypto.subtle.sign("HMAC", key, encoder.encode(`studio-magic-link\n${origin}\n${token}`))
  return Array.from(new Uint8Array(signature), (byte) => byte.toString(16).padStart(2, "0")).join("")
}

/** Callback URLs must stay on the origin that will receive the session cookie. */
function callbackOnOrigin(value: string, origin: string): string | null {
  try { const callback = new URL(value, origin); return callback.origin === origin && !callback.username && !callback.password ? callback.href : null } catch { return null }
}

/** Refuse a verification URL moved to another host before Better Auth consumes its token. */
export async function verifyMagicLinkOrigin(request: Request, secret: string, trustedOrigins: readonly string[]): Promise<boolean> {
  const url = new URL(request.url)
  const token = url.searchParams.get("token")
  const proof = url.searchParams.get("studioOriginProof")
  if (!trustedOrigins.includes(url.origin) || !token || !proof || !/^[0-9a-f]{64}$/.test(proof)) return false
  if (!url.searchParams.has("callbackURL")) return false
  for (const name of ["callbackURL", "newUserCallbackURL", "errorCallbackURL"]) {
    const values = url.searchParams.getAll(name)
    // Issued links contain absolute callbacks. A modified relative callback would be
    // resolved by Better Auth against its primary baseURL, not the requesting host.
    if (values.length > 1 || (values[0] !== undefined && callbackOnOrigin(values[0], url.origin) !== values[0])) return false
  }
  const encoder = new TextEncoder()
  const key = await crypto.subtle.importKey("raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["verify"])
  const bytes = Uint8Array.from(proof.match(/../g) ?? [], (hex) => Number.parseInt(hex, 16))
  return crypto.subtle.verify("HMAC", key, bytes, encoder.encode(`studio-magic-link\n${url.origin}\n${token}`))
}


/** Minimal adapter surface used by hooks and membership resolution without creating a circular Auth type. */
interface AdapterLike {
  findOne<T>(args: { model: string; where: { field: string; value: string }[] }): Promise<T | null>
  findMany<T>(args: { model: string; where: { field: string; value: string }[]; sortBy?: { field: string; direction: "asc" | "desc" }; limit?: number }): Promise<T[]>
}

/** The running instance, for hooks that need its database outside an endpoint context. */
interface Self { auth?: { readonly $context: Promise<{ adapter: unknown }> } }

const nonEmptyString = (value: unknown): string | null =>
  typeof value === "string" && value.trim() !== "" ? value : null

const VALID_ORG_ROLES: ReadonlySet<string> = new Set(Object.keys(ORG_ROLES))

/** Exact case-sensitive role token check matching SQLite `GLOB` predicates (no whitespace stripping). */
const hasOrgRole = (role: string | null | undefined, allowed: readonly string[]): boolean =>
  typeof role === "string" && role.split(",").some((part) => allowed.includes(part))

/** Validate that every comma-separated role token is an exact declared organization role. */
function validateRoleList(role: string | null | undefined): string | null {
  if (role === null || role === undefined || role === "") return "member"
  const parts = role.split(",")
  if (parts.some((part) => !VALID_ORG_ROLES.has(part))) return null
  return parts.join(",")
}

type SqlParam = string | number | null
export type SqlStatement = { readonly sql: string; readonly params: readonly SqlParam[] }
type QueryResult = { first<T>(): Promise<T | null>; all<T>(): Promise<{ results: T[] }>; run(): Promise<unknown> }
export type ProvisionDb = {
  prepare(sql: string): { bind?: (...params: SqlParam[]) => QueryResult; get?: (...params: SqlParam[]) => unknown; all?: (...params: SqlParam[]) => unknown[]; run?: (...params: SqlParam[]) => unknown }
  batch?: (statements: unknown[]) => Promise<unknown>
  transaction?: (fn: () => void) => () => void
}

/** SQL predicate confirming an organization row is not marked `norn.provisioning`. */
export const READY_ORG_SQL = `(o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)`
/** SQL predicate confirming a timestamp column (`expiresAt`) has not passed. */
export const LIVE_EXPIRY_SQL = (column: string) =>
  `(case when typeof(${column}) in ('integer', 'real') then ${column} > (julianday('now') - 2440587.5) * 86400000.0 else julianday(${column}) > julianday('now') end)`
export const LIVE_SESSION_SQL = LIVE_EXPIRY_SQL(`"expiresAt"`)
export const LIVE_SESSION_S_SQL = LIVE_EXPIRY_SQL(`s."expiresAt"`)

/**
 * Unused Better Auth organization endpoints disabled on this deployment, including every
 * slug-accepting read route that resolves slugs before checking caller membership.
 */
export const DISABLED_ORG_PATHS: ReadonlySet<string> = new Set([
  "/organization/check-slug",
  "/organization/get-organization",
  "/organization/get-full-organization",
  "/organization/list",
  "/organization/list-members",
  "/organization/get-active-member",
  "/organization/get-active-member-role",
  "/organization/list-invitations",
  "/organization/get-invitation",
  "/organization/list-user-invitations",
  "/organization/has-permission",
  "/organization/create-team",
  "/organization/remove-team",
  "/organization/update-team",
  "/organization/list-teams",
  "/organization/set-active-team",
  "/organization/list-user-teams",
  "/organization/list-team-members",
  "/organization/add-team-member",
  "/organization/remove-team-member",
  "/organization/create-role",
  "/organization/delete-role",
  "/organization/list-roles",
  "/organization/get-role",
  "/organization/update-role",
])

/** Retained Better Auth organization mutations that can default to activeOrganizationId when organizationId is omitted. */
const IMPLICIT_ORG_MUTATION_PATHS: ReadonlySet<string> = new Set([
  "/organization/update",
  "/organization/invite-member",
  "/organization/remove-member",
  "/organization/update-member-role",
  "/organization/leave",
])

export const INVITATION_GONE = "This invitation is no longer open: it expired or was withdrawn. Ask the workspace's admins for a new one."
/** Better Auth's default membership limit (its accept route refuses at 100 members). */
export const MEMBERSHIP_LIMIT = 100
export const WORKSPACE_ACCESS_DENIED_MESSAGE = "You do not have access to this workspace."

export async function dbFirst<T>(db: ProvisionDb, sql: string, ...params: SqlParam[]): Promise<T | null> {
  const statement = db.prepare(sql)
  if (statement.bind) return statement.bind(...params).first<T>()
  return (statement.get?.(...params) as T | undefined) ?? null
}

export async function dbAll<T>(db: ProvisionDb, sql: string, ...params: SqlParam[]): Promise<T[]> {
  const statement = db.prepare(sql)
  if (statement.bind) return (await statement.bind(...params).all<T>()).results
  return (statement.all?.(...params) ?? []) as T[]
}

export async function dbRun(db: ProvisionDb, sql: string, ...params: SqlParam[]): Promise<void> {
  const statement = db.prepare(sql)
  if (statement.bind) await statement.bind(...params).run()
  else statement.run?.(...params)
}

export interface CommittedSessionRow {
  readonly id: string
  readonly token: string
  readonly userId: string
  readonly expiresAt: string | number
  readonly createdAt: string | number
  readonly updatedAt: string | number
  readonly ipAddress: string | null
  readonly userAgent: string | null
  readonly activeOrganizationId: string | null
  readonly authMethod: string | null
}

/**
 * Atomically activate `organizationId` on `session.token` ONLY if at commit time the user is a
 * member of that organization, it satisfies `READY_ORG_SQL`, and (when `expectedSlug` is given)
 * its slug still matches. Uses `UPDATE ... RETURNING` so even a no-op update against an already-set
 * active organization cannot pass after a concurrent revocation.
 */
export async function commitActivateSession(
  db: ProvisionDb,
  args: { readonly token: string; readonly userId: string; readonly organizationId: string; readonly expectedSlug?: string },
): Promise<CommittedSessionRow | null> {
  const at = new Date().toISOString()
  const slugClause = args.expectedSlug === undefined ? "" : ` and o."slug" = ?`
  const params: SqlParam[] = args.expectedSlug === undefined
    ? [args.organizationId, at, args.token, args.userId, args.organizationId, args.userId]
    : [args.organizationId, at, args.token, args.userId, args.organizationId, args.userId, args.expectedSlug]
  return dbFirst<CommittedSessionRow>(
    db,
    `update "session" set "activeOrganizationId" = ?, "updatedAt" = ?
     where "token" = ? and "userId" = ? and ${LIVE_SESSION_SQL}
       and exists (select 1 from "member" m join "organization" o on o."id" = m."organizationId"
                   where m."organizationId" = ? and m."userId" = ?${slugClause} and ${READY_ORG_SQL})
     returning "id", "token", "userId", "expiresAt", "createdAt", "updatedAt", "ipAddress", "userAgent", "activeOrganizationId", "authMethod"`,
    ...params,
  )
}

export async function commitClearActiveSession(
  db: ProvisionDb,
  args: { readonly token: string; readonly userId: string },
): Promise<CommittedSessionRow | null> {
  const at = new Date().toISOString()
  return dbFirst<CommittedSessionRow>(
    db,
    `update "session" set "activeOrganizationId" = null, "updatedAt" = ?
     where "token" = ? and "userId" = ? and ${LIVE_SESSION_SQL}
     returning "id", "token", "userId", "expiresAt", "createdAt", "updatedAt", "ipAddress", "userAgent", "activeOrganizationId", "authMethod"`,
    at,
    args.token,
    args.userId,
  )
}

export async function isSessionLive(db: ProvisionDb, token: string, userId: string): Promise<boolean> {
  return (await dbFirst<{ live: number }>(
    db,
    `select 1 as "live" from "session" where "token" = ? and "userId" = ? and ${LIVE_SESSION_SQL}`,
    token,
    userId,
  )) !== null
}

const expiryOf = (value: unknown) => typeof value === "number" ? value : typeof value === "string" ? Date.parse(value) : Number.NaN

export type AcceptInvitationOutcome =
  | {
    readonly kind: "accepted"
    readonly invitation: { readonly id: string; readonly organizationId: string; readonly email: string; readonly role: string | null; readonly status: "accepted"; readonly expiresAt: unknown }
    readonly member: { readonly id: string; readonly organizationId: string; readonly userId: string; readonly role: string; readonly createdAt: string }
    readonly updatedAt: string
  }
  | {
    readonly kind: "refused"
    readonly status: 400 | 401 | 403 | 409
    readonly code: string
    readonly message: string
    readonly refusal?: Admission
  }

/**
 * Atomically accept an invitation, insert the member row, and activate the organization on the
 * session in ONE serialized transaction with commit-time predicates for invitation status/expiry,
 * verified recipient email, workspace readiness, and membership limits.
 */
export async function commitAcceptInvitation(
  db: ProvisionDb,
  args: {
    readonly invitationId: string
    readonly userId: string
    readonly userEmail: string
    readonly emailVerified: boolean
    readonly sessionToken: string
    readonly billing: BillingSettings | undefined
    readonly baseURL: string
  },
): Promise<AcceptInvitationOutcome> {
  // Scope target lookup strictly to the caller's email so absent and foreign-recipient invitations
  // produce one uniform 400 INVITATION_NOT_FOUND refusal without leaking invitation existence.
  const invitation = await dbFirst<{ id: string; organizationId: string; email: string; role: string | null; status: string; expiresAt: unknown }>(
    db,
    `select "id", "organizationId", "email", "role", "status", "expiresAt" from "invitation" where "id" = ? and lower("email") = lower(?)`,
    args.invitationId,
    args.userEmail,
  )
  if (invitation === null || !(expiryOf(invitation.expiresAt) > Date.now()) || invitation.status !== "pending") {
    return { kind: "refused", status: 400, code: "INVITATION_NOT_FOUND", message: INVITATION_GONE }
  }
  if (!args.emailVerified) {
    return { kind: "refused", status: 403, code: "EMAIL_VERIFICATION_REQUIRED_BEFORE_ACCEPTING_OR_REJECTING_INVITATION", message: "Email verification required before accepting or rejecting invitation" }
  }
  const org = await dbFirst<{ metadata: string | null }>(db, `select "metadata" from "organization" where "id" = ?`, invitation.organizationId)
  if (org === null) {
    return { kind: "refused", status: 400, code: "ORGANIZATION_NOT_FOUND", message: "Organization not found" }
  }
  if (isProvisioningMetadata(org.metadata)) {
    return { kind: "refused", status: 409, code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." }
  }
  const count = await dbFirst<{ n: number }>(db, `select count(*) as "n" from "member" where "organizationId" = ?`, invitation.organizationId)
  if ((count?.n ?? 0) >= MEMBERSHIP_LIMIT) {
    return { kind: "refused", status: 403, code: "ORGANIZATION_MEMBERSHIP_LIMIT_REACHED", message: "Organization membership limit reached" }
  }
  const role = validateRoleList(invitation.role)
  if (role === null) {
    return { kind: "refused", status: 400, code: "INVALID_ROLE", message: "Invitation specifies an invalid workspace role." }
  }
  const at = new Date().toISOString()
  const member = { id: crypto.randomUUID(), organizationId: invitation.organizationId, userId: args.userId, role, createdAt: at }
  let batchResults: readonly (readonly Record<string, unknown>[])[]
  try {
    // All three writes commit in one serialized transaction. Session expiry is evaluated ONCE in
    // Statement 1; Statement 2 and Statement 3 chain via changes() = 1 and return their committed
    // rows directly so no second clock evaluation or post-batch read can diverge from the commit.
    batchResults = await atomicallyWithResults(db as unknown as ProvisionDb, [
      {
        sql: `update "invitation" set "status" = 'accepted' where "id" = ? and "status" = 'pending'
          and ${LIVE_EXPIRY_SQL(`"expiresAt"`)}
          and exists (select 1 from "session" s where s."token" = ? and s."userId" = ? and ${LIVE_SESSION_S_SQL})
          and exists (select 1 from "user" u where u."id" = ? and u."emailVerified" = 1 and lower(u."email") = lower("invitation"."email"))
          and exists (select 1 from "organization" o where o."id" = "invitation"."organizationId" and ${READY_ORG_SQL})
          and (select count(*) from "member" m where m."organizationId" = "invitation"."organizationId") < ${MEMBERSHIP_LIMIT}`,
        params: [args.invitationId, args.sessionToken, args.userId, args.userId],
      },
      {
        sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select ?, ?, ?, ?, ? where changes() = 1 returning "id"`,
        params: [member.id, member.organizationId, member.userId, member.role, member.createdAt],
      },
      {
        sql: `update "session" set "activeOrganizationId" = ?, "updatedAt" = ?
          where "token" = ? and "userId" = ? and changes() = 1
          returning "activeOrganizationId", "updatedAt"`,
        params: [member.organizationId, at, args.sessionToken, args.userId],
      },
    ])
  } catch (error) {
    if (limitRaised(error) === "members") {
      const admission = await admitMember(db as unknown as BillingDb, args.billing, args.baseURL, invitation.organizationId, args.userId)
      const refusal = admission.kind === "plan-limit" ? admission : undefined
      return {
        kind: "refused",
        status: 403,
        code: "PLAN_LIMIT",
        message: refusal === undefined ? "This workspace's member limit is reached." : refusalMessage(refusal),
        ...refusal === undefined ? {} : { refusal },
      }
    }
    if (/UNIQUE constraint failed: member\./.test(error instanceof Error ? error.message : "")) {
      return { kind: "refused", status: 400, code: "INVITATION_NOT_FOUND", message: INVITATION_GONE }
    }
    throw error
  }
  const insertedMember = batchResults[1]?.[0]
  const activatedSession = batchResults[2]?.[0]
  if (insertedMember === undefined || activatedSession === undefined) {
    if (!(await isSessionLive(db, args.sessionToken, args.userId))) {
      return { kind: "refused", status: 401, code: "UNAUTHORIZED", message: "Sign in first." }
    }
    const latestOrg = await dbFirst<{ metadata: string | null }>(db, `select "metadata" from "organization" where "id" = ?`, invitation.organizationId)
    if (latestOrg !== null && isProvisioningMetadata(latestOrg.metadata)) {
      return { kind: "refused", status: 409, code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." }
    }
    const now = await dbFirst<{ n: number }>(db, `select count(*) as "n" from "member" where "organizationId" = ?`, invitation.organizationId)
    if ((now?.n ?? 0) >= MEMBERSHIP_LIMIT) {
      return { kind: "refused", status: 403, code: "ORGANIZATION_MEMBERSHIP_LIMIT_REACHED", message: "Organization membership limit reached" }
    }
    return { kind: "refused", status: 400, code: "INVITATION_NOT_FOUND", message: INVITATION_GONE }
  }
  return { kind: "accepted", invitation: { ...invitation, status: "accepted" }, member, updatedAt: String(activatedSession.updatedAt ?? at) }
}

/** Hash an API key identically to `@better-auth/api-key`'s `defaultKeyHasher` (unpadded base64url SHA-256). */
async function hashApiKey(key: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(key)))
  let binary = ""
  for (const byte of digest) binary += String.fromCharCode(byte)
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

export interface CreateApiKeyInput {
  readonly userId: string
  readonly organizationId: string
  readonly sessionToken?: string | null
  readonly name?: string | null
  readonly prefix?: string | null
  readonly expiresIn?: number | null
  readonly metadata?: Record<string, unknown> | null
  readonly permissions?: Record<string, string[]> | null
  readonly remaining?: number | null
  readonly refillAmount?: number | null
  readonly refillInterval?: number | null
  readonly rateLimitEnabled?: boolean
  readonly rateLimitTimeWindow?: number
  readonly rateLimitMax?: number
}

export type CreateApiKeyOutcome =
  | {
    readonly kind: "created"
    readonly apiKey: Record<string, unknown>
  }
  | {
    readonly kind: "refused"
    readonly status: 401 | 403 | 409
    readonly code: string
    readonly message: string
  }

/** Strict request-body schema mirroring `@better-auth/api-key`'s `createApiKeyBodySchema`. */
const createApiKeyRequestSchema = z.object({
  configId: z.literal("default").optional(),
  name: z.string().min(1).max(32).optional(),
  expiresIn: z.number().finite().min(1).nullable().optional(),
  prefix: z.string().min(1).max(32).regex(/^[a-zA-Z0-9_-]+$/).optional(),
  remaining: z.number().finite().int().min(0).nullable().optional(),
  metadata: z.record(z.string(), z.unknown()).optional(),
  refillAmount: z.number().finite().int().min(1).optional(),
  refillInterval: z.number().finite().int().min(1).optional(),
  rateLimitTimeWindow: z.number().finite().int().min(1).optional(),
  rateLimitMax: z.number().finite().int().min(1).optional(),
  rateLimitEnabled: z.boolean().optional(),
  permissions: z.record(z.string(), z.array(z.string())).optional(),
  userId: z.string().min(1).optional(),
  organizationId: z.string().optional(),
  referenceId: z.string().optional(),
}).strict()

/**
 * Atomically insert an organization API key ONLY if at insertion time `userId` holds an exact
 * case-sensitive `owner` or `admin` membership in `organizationId`, the organization satisfies
 * `READY_ORG_SQL`, and (for session-backed calls) the session still exists and has not expired.
 */
export async function commitCreateApiKey(
  db: ProvisionDb,
  input: CreateApiKeyInput,
): Promise<CreateApiKeyOutcome> {
  const id = generateRandomString(32, "a-z", "A-Z", "0-9")
  const prefix = input.prefix ?? null
  const key = `${prefix ?? ""}${generateRandomString(64, "a-z", "A-Z")}`
  const hashed = await hashApiKey(key)
  const start = key.substring(0, 6)
  const now = new Date()
  const nowIso = now.toISOString()
  const expiresAt = typeof input.expiresIn === "number" ? new Date(now.getTime() + input.expiresIn * 1000) : null
  const expiresAtIso = expiresAt === null ? null : expiresAt.toISOString()
  const rateLimitEnabled = input.rateLimitEnabled ?? true
  const rateLimitTimeWindow = input.rateLimitTimeWindow ?? 60_000
  const rateLimitMax = input.rateLimitMax ?? 120
  const refillAmount = input.refillAmount ?? null
  const refillInterval = input.refillInterval ?? null
  const remaining = input.remaining === null ? null : input.remaining ?? refillAmount ?? null
  const permissionsText = input.permissions ? JSON.stringify(input.permissions) : null
  const metadataText = input.metadata ? JSON.stringify(input.metadata) : null
  const hasSession = typeof input.sessionToken === "string" && input.sessionToken !== ""
  const sessionGuard = hasSession
    ? ` and exists (select 1 from "session" s where s."token" = ? and s."userId" = ? and ${LIVE_SESSION_S_SQL})`
    : ""
  const guardParams: SqlParam[] = hasSession
    ? [input.organizationId, input.userId, input.sessionToken!, input.userId]
    : [input.organizationId, input.userId]

  const inserted = await dbFirst<{ id: string }>(
    db,
    `insert into "apikey" (
       "id", "configId", "name", "start", "referenceId", "prefix", "key",
       "refillInterval", "refillAmount", "lastRefillAt", "enabled",
       "rateLimitEnabled", "rateLimitTimeWindow", "rateLimitMax",
       "requestCount", "remaining", "lastRequest", "expiresAt",
       "createdAt", "updatedAt", "permissions", "metadata"
     )
     select ?, 'default', ?, ?, ?, ?, ?, ?, ?, null, 1, ?, ?, ?, 0, ?, null, ?, ?, ?, ?, ?
     where exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = ? and m."userId" = ?
         and ((',' || m."role" || ',') glob '*,owner,*'
           or (',' || m."role" || ',') glob '*,admin,*')
         and ${READY_ORG_SQL}
     )${sessionGuard}
     returning "id"`,
    id,
    input.name ?? null,
    start,
    input.organizationId,
    prefix,
    hashed,
    refillInterval,
    refillAmount,
    rateLimitEnabled ? 1 : 0,
    rateLimitTimeWindow,
    rateLimitMax,
    remaining,
    expiresAtIso,
    nowIso,
    nowIso,
    permissionsText,
    metadataText,
    ...guardParams,
  )

  if (inserted === null) {
    if (hasSession && !(await isSessionLive(db, input.sessionToken!, input.userId))) {
      return { kind: "refused", status: 401, code: "UNAUTHORIZED_SESSION", message: "Unauthorized or invalid session" }
    }
    const row = await dbFirst<{ role: string; metadata: string | null }>(
      db,
      `select m."role", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`,
      input.organizationId,
      input.userId,
    )
    if (row !== null && hasOrgRole(row.role, ["owner", "admin"]) && isProvisioningMetadata(row.metadata)) {
      return { kind: "refused", status: 409, code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." }
    }
    return { kind: "refused", status: 403, code: "FORBIDDEN", message: "You do not have permission to create an API key for this workspace." }
  }

  return {
    kind: "created",
    apiKey: {
      id,
      configId: "default",
      name: input.name ?? null,
      start,
      referenceId: input.organizationId,
      prefix,
      key,
      refillInterval,
      refillAmount,
      lastRefillAt: null,
      enabled: true,
      rateLimitEnabled,
      rateLimitTimeWindow,
      rateLimitMax,
      requestCount: 0,
      remaining,
      lastRequest: null,
      expiresAt,
      createdAt: now,
      updatedAt: now,
      permissions: input.permissions ?? null,
      metadata: input.metadata ?? null,
    },
  }
}

const API_ERROR_STATUS = { 400: "BAD_REQUEST", 401: "UNAUTHORIZED", 403: "FORBIDDEN", 404: "NOT_FOUND", 409: "CONFLICT" } as const
function responseHeadersOf(ctx: { responseHeaders?: Headers; context: { responseHeaders?: Headers } }): Headers | undefined {
  if (ctx.responseHeaders === undefined && ctx.context.responseHeaders === undefined) return undefined
  const merged = new Headers()
  for (const source of [ctx.context.responseHeaders, ctx.responseHeaders]) {
    if (source === undefined) continue
    source.forEach((value, key) => {
      if (key.toLowerCase() === "set-cookie") merged.append(key, value)
      else merged.set(key, value)
    })
  }
  return merged
}
function apiError(status: keyof typeof API_ERROR_STATUS, body: Record<string, unknown>, headers?: Headers): APIError {
  return new APIError(API_ERROR_STATUS[status], body, headers)
}

async function stateOf(db: ProvisionDb, userId: string): Promise<string | undefined> {
  const statement = db.prepare(`select "status" from "workspace_provisioning" where "userId" = ?`)
  const row = statement.bind ? await statement.bind(userId).first<{ status: string }>() : statement.get?.(userId) as { status: string } | undefined
  return row?.status
}

/** Execute statements in one database transaction (a D1 batch) and return each statement's rows. */
export async function atomicallyWithResults(db: ProvisionDb, statements: readonly SqlStatement[]): Promise<readonly (readonly Record<string, unknown>[])[]> {
  if (db.batch) {
    const prepared = statements.map(({ sql, params }) => {
      const statement = db.prepare(sql)
      if (!statement.bind) throw new Error("D1 statement binding unavailable")
      return statement.bind(...params)
    })
    const results = await db.batch(prepared) as { results?: Record<string, unknown>[] }[] | undefined
    return statements.map((_, index) => results?.[index]?.results ?? [])
  }
  if (db.transaction) {
    let out: Record<string, unknown>[][] = []
    db.transaction(() => {
      out = statements.map(({ sql, params }) => {
        const stmt = db.prepare(sql)
        if (/\breturning\b/i.test(sql) && stmt.all) {
          return (stmt.all(...params) ?? []) as Record<string, unknown>[]
        }
        stmt.run?.(...params)
        return []
      })
    })()
    return out
  }
  throw new Error("Atomic provisioning is unavailable")
}

/** Execute the statements in one database transaction (a D1 batch): all of them commit, or none. */
export async function atomically(db: ProvisionDb, statements: readonly SqlStatement[]): Promise<void> {
  await atomicallyWithResults(db, statements)
}

async function provisionWorkspace(db: ProvisionDb, secret: string, user: { id: string; name: string; email: string }, defaultEnvironmentId?: string, billed = false) {
  if (await stateOf(db, user.id) !== "pending") return
  const identity = await workspaceIdentity(secret, user.id)
  // With billing on, the workspace is billed to its owner (migration 0011 reads the mark).
  const metadata = JSON.stringify({ ...JSON.parse(identity.metadata), ...defaultEnvironmentId ? { environmentId: defaultEnvironmentId } : {}, ...billed ? { [BILLED]: true } : {} })
  const at = new Date().toISOString()
  const pending = `exists (select 1 from "workspace_provisioning" where "userId" = ? and "status" = 'pending')`
  await atomically(db, [
    { sql: `insert into "organization" ("id", "name", "slug", "metadata", "createdAt") select ?, ?, ?, ?, ? where ${pending} on conflict ("id") do nothing`, params: [identity.id, `${user.name || user.email}'s workspace`, `u-${identity.id.slice(10)}`, metadata, at, user.id] },
    { sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select ?, ?, ?, 'owner', ? where ${pending} and exists (select 1 from "organization" where "id" = ? and json_extract("metadata", '$."norn.provisioningProof"') = ?) on conflict ("organizationId", "userId") do nothing`, params: [crypto.randomUUID(), identity.id, user.id, at, user.id, identity.id, JSON.parse(identity.metadata)["norn.provisioningProof"]] },
    { sql: `update "workspace_provisioning" set "status" = 'complete' where "userId" = ? and "status" = 'pending' and exists (select 1 from "member" where "organizationId" = ? and "userId" = ? and "role" = 'owner')`, params: [user.id, identity.id, user.id] },
  ])
  if (await stateOf(db, user.id) === "pending") throw new Error("Workspace ownership could not be verified")
}

/** A server-authenticated workspace identity, independent of user-editable slugs. */
async function workspaceIdentity(secret: string, userId: string) {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"])
  const sign = async (value: string) => Array.from(new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(value))), (byte) => byte.toString(16).padStart(2, "0")).join("")
  const id = `workspace_${await sign(`id:${userId}`)}`
  return { id, metadata: JSON.stringify({ "norn.provisioningProof": await sign(`owner:${userId}:${id}`) }) }
}

/** Shown when a non-allowlisted address attempts sign-up on an invite-only deployment. */
export const INVITE_ONLY_MESSAGE = "This alpha is invite-only; ask your admin for an invitation."

/** Whether an email address may create an account under the deployment's allowlist (undefined = open). */
export const signUpAllowed = (allowlist: readonly string[] | undefined, email: string): boolean =>
  allowlist === undefined || allowlist.includes(email.trim().toLowerCase())

/** The workspace's payer, set at creation while billing is on (workspaces.ts) and read by migration 0011's triggers. */
export const BILLING_PAYER = "norn.billingPayer"
/** Set on workspaces created while billing is on: only those are limited (migration 0011). */
export const BILLED = "norn.billed"
/** Set while a newly created workspace's cell is still provisioning on NORN_HOST (excluded from opening until ready). */
export const PROVISIONING = "norn.provisioning"
/** Reserved metadata is assigned only by server-side provisioning. */
const reservedMetadata = (key: string) => key === "environmentId" || key.startsWith("norn.")
function metadataObject(value: unknown): Record<string, unknown> {
  if (typeof value === "string") { try { return metadataObject(JSON.parse(value)) } catch { return {} } }
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {}
}
export const publicMetadata = (value: unknown) => Object.fromEntries(Object.entries(metadataObject(value)).filter(([key]) => !reservedMetadata(key)))
export const isProvisioningMetadata = (value: unknown): boolean => metadataObject(value)[PROVISIONING] === true

export function authOptions(config: AuthConfig, self: Self = {}) {
  return {
    appName: "Norn Studio",
    database: config.database,
    secret: config.secret,
    baseURL: config.baseURL,
    basePath: "/api/auth",
    // An error that is not Better Auth's own (a database refusal) is thrown to the Worker, which turns a
    // plan-limit trigger's refusal into the typed PLAN_LIMIT answer (index.ts guarded) and anything
    // else into a bare 500, as Better Auth would.
    onAPIError: { throw: true },
    // Better Auth refuses state-changing requests whose Origin is not listed (its CSRF check).
    trustedOrigins: [...(config.trustedOrigins ?? [config.baseURL])],
    // No sign-in on sign-up: Better Auth then answers a registered address exactly as a new one
    // (no session either way), so sign-up does not reveal which emails have accounts.
    emailAndPassword: {
      enabled: true,
      autoSignIn: false,
      minPasswordLength: 12,
      maxPasswordLength: 128,
      // Better Auth's own hash, computed in pure JS: byte-identical, so accounts move between runtimes.
      ...config.scrypt === "js" ? { password: { hash: hashPassword, verify: verifyPassword } } : {},
      // Timing mitigation (live measurement pending, #817): a registered address skips the four
      // writes a new account makes (user, credential, organization, member), so it makes four too.
      // Best-effort: a failed write never changes the answer. A failed delete leaves one already
      // expired `signup-pad:` verification row behind; nothing reads it, and it holds no user data.
      onExistingUserSignUp: async () => {
        try {
          const adapter = await getCurrentAdapter((await self.auth?.$context)?.adapter as never)
          for (let pair = 0; pair < 2; pair++) {
            const now = new Date()
            const row = await adapter.create<{ id: string }>({ model: "verification", data: { identifier: `signup-pad:${crypto.randomUUID()}`, value: "", expiresAt: now, createdAt: now, updatedAt: now } })
            await adapter.delete({ model: "verification", where: [{ field: "id", value: row.id }] }).catch(() => undefined)
          }
        } catch {
          // The padding is a mitigation, not part of sign-up.
        }
      },
    },
    // Gateway grants are linked to an existing session only (never a sign-in), so the gateway's
    // email need not match; tokens are encrypted at rest.
    account: { encryptOAuthTokens: true, accountLinking: { enabled: true, trustedProviders: ["executor"], allowDifferentEmails: true } },
    session: {
      expiresIn: 7 * 24 * 60 * MINUTE,
      updateAge: 24 * 60 * MINUTE,
      additionalFields: { authMethod: { type: "string", required: false, input: false } },
    },
    rateLimit: {
      enabled: true,
      // Counters live in D1 so every isolate shares them.
      storage: "database",
      window: MINUTE,
      max: 60,
      customRules: {
        "/sign-in/email": { window: 5 * MINUTE, max: 10 },
        "/sign-up/email": { window: 60 * MINUTE, max: 5 },
        "/sign-in/magic-link": { window: 15 * MINUTE, max: 3 },
        "/magic-link/verify": { window: 5 * MINUTE, max: 10 },
        "/api-key/verify": { window: MINUTE, max: 60 },
        "/oauth2/register": { window: 60 * MINUTE, max: 20 },
        "/oauth2/token": { window: MINUTE, max: 60 },
        // Polar delivers from a few addresses; a burst of subscription events must not be throttled.
        "/polar/webhooks": { window: MINUTE, max: 600 },
        "/checkout": { window: MINUTE, max: 10 },
      },
    },
    advanced: {
      // Explicit: Better Auth otherwise turns its origin (CSRF) check off whenever NODE_ENV is "test".
      disableOriginCheck: false,
      useSecureCookies: true,
      // The same cookie prefix as Norn's other Better Auth apps; host-only, never shared across subdomains.
      cookiePrefix: "norn",
      defaultCookieAttributes: { httpOnly: true, secure: true, sameSite: "lax" },
      // Only the header this deployment's edge sets itself (client-ip.ts); X-Forwarded-For is client-controlled.
      ipAddress: { ipAddressHeaders: [config.clientIpHeader] },
    },
    plugins: [
      // Only a verified address may accept an invitation (GHSA-fmh4-wcc4-5jm3): until a mail
      // sender verifies addresses, nobody can join an organization by claiming an invited email.
      // Direct organization creation and deletion through Better Auth are disabled for normal users
      // so no org can be created without cell provisioning or deleted without full revocation.
      organization({ ac, roles: ORG_ROLES, creatorRole: "owner", allowUserToCreateOrganization: false, disableOrganizationDeletion: true, requireEmailVerificationOnInvitation: true,
        organizationHooks: {
          beforeCreateOrganization: async () => {
            throw new APIError("FORBIDDEN", { code: "ORGANIZATION_CREATE_DISABLED", message: "Direct workspace creation is disabled on this deployment." })
          },
          // Advisory: an admin is told before inviting someone the workspace could not take (members and
          // pending invitations). Invitations hold nothing; the member limit is enforced when one joins.
          beforeCreateInvitation: async ({ organization, inviter }) => {
            if (isProvisioningMetadata(organization.metadata)) {
              throw new APIError("CONFLICT", { code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." })
            }
            enforce(await admitMember(config.database as unknown as BillingDb, await billingOf(config.database as unknown as BillingDb, settingsOf(config.billing?.config)), config.baseURL, organization.id, inviter.id, true))
          },
          beforeAcceptInvitation: async ({ organization }) => {
            if (isProvisioningMetadata(organization.metadata)) {
              throw new APIError("CONFLICT", { code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." })
            }
          },
          beforeUpdateOrganization: async ({ organization, member }) => {
            const adapter = await getCurrentAdapter((await self.auth?.$context)?.adapter as never)
            const stored = await adapter.findOne<{ metadata?: string | null }>({ model: "organization", where: [{ field: "id", value: member.organizationId }] })
            if (stored === null) throw new Error("Organization no longer exists")
            if (isProvisioningMetadata(stored.metadata)) {
              throw new APIError("CONFLICT", { code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." })
            }
            if (organization.metadata === undefined) return
            const protectedFields = Object.fromEntries(Object.entries(metadataObject(stored.metadata)).filter(([key]) => reservedMetadata(key)))
            return { data: { ...organization, metadata: { ...publicMetadata(organization.metadata), ...protectedFields } } }
          },
          beforeDeleteOrganization: async () => {
            throw new APIError("FORBIDDEN", { code: "ORGANIZATION_DELETE_DISABLED", message: "Direct workspace deletion is disabled on this deployment." })
          },
        },
      }),
      apiKey({ references: "organization", enableMetadata: true, rateLimit: { enabled: true, timeWindow: 60_000, maxRequests: 120 } }),
      magicLink({
        expiresIn: 10 * MINUTE,
        disableSignUp: true,
        sendMagicLink: async ({ email, url, token }, ctx) => {
          if (config.sendMagicLink === undefined) throw new APIError("SERVICE_UNAVAILABLE", { message: "Email sign-in is not available on this deployment." })
          const origin = ctx?.headers?.get("origin") ?? ctx?.request?.headers.get("origin")
          if (!origin || !(config.trustedOrigins ?? [config.baseURL]).includes(origin)) throw new APIError("FORBIDDEN", { message: "Untrusted sign-in origin." })
          const link = new URL(url)
          const requested = new URL(origin)
          link.protocol = requested.protocol; link.host = requested.host
          for (const name of ["callbackURL", "newUserCallbackURL", "errorCallbackURL"]) {
            const value = link.searchParams.get(name)
            if (value === null) continue
            const callback = callbackOnOrigin(value, origin)
            if (callback === null) throw new APIError("FORBIDDEN", { message: "Untrusted magic-link callback." })
            link.searchParams.set(name, callback)
          }
          link.searchParams.set("studioOriginProof", await originProof(config.secret, token, origin))
          await config.sendMagicLink({ to: email, url: link.toString() })
        },
      }),
      // Short-lived tokens the Worker mints for the Norn host (#817); the host verifies them against /api/auth/jwks.
      jwt({ jwt: { issuer: config.baseURL, audience: "norn-host", expirationTime: "5m" } }),
      ...config.gateway === undefined ? [] : [gatewayLink(config.gateway)],
      ...config.mcp === true ? assistantAuthorization(config.baseURL, self) : [],
      ...config.billing === undefined ? [] : [polarPlugin(config.billing, config.baseURL)],
    ],
    hooks: {
      // Runs after Better Auth's origin check and rate limit, before endpoint execution:
      // - invite-only sign-up refusal (403 INVITE_ONLY),
      // - unconditional disable of direct organization create/delete and org-discovery endpoints,
      // - commit-time set-active with membership-scoped lookup and uniform non-existence refusal,
      // - commit-time invitation acceptance (preserving refreshed session cookies),
      // - commit-time API-key admission at insertion,
      // - required expected-org verification and single-reference pinning on MCP OAuth consent.
      before: createAuthMiddleware(async (ctx) => {
        const path = ctx.path.replace(/\/+$/, "").toLowerCase()
        if (path === "/sign-up/email" && config.signUpEmails !== undefined) {
          const body = ctx.body as { email?: unknown } | undefined
          if (typeof body?.email === "string" && body.email.trim() !== "" && !signUpAllowed(config.signUpEmails, body.email)) {
            throw new APIError("FORBIDDEN", { code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE })
          }
          return
        }
        if (path === "/organization/create") {
          throw new APIError("FORBIDDEN", { code: "ORGANIZATION_CREATE_DISABLED", message: "Direct workspace creation is disabled on this deployment." })
        }
        if (path === "/organization/delete") {
          throw new APIError("FORBIDDEN", { code: "ORGANIZATION_DELETE_DISABLED", message: "Direct workspace deletion is disabled on this deployment." })
        }
        if (DISABLED_ORG_PATHS.has(path)) {
          throw new APIError("NOT_FOUND", { code: "NOT_FOUND", message: "No such endpoint." })
        }
        if (IMPLICIT_ORG_MUTATION_PATHS.has(path)) {
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          if (session === null) return
          const body = (ctx.body ?? {}) as Record<string, unknown>
          const bodyOrg = nonEmptyString(body.organizationId)
          const headerOrg = nonEmptyString((ctx.headers ?? ctx.request?.headers)?.get("x-norn-expected-org"))
          let targetOrgId = bodyOrg
          if (targetOrgId === null) {
            if (headerOrg === null) {
              throw apiError(400, { code: "WORKSPACE_SCOPE_REQUIRED", message: "Workspace scope header is required for organization mutations." }, responseHeaders)
            }
            const adapter = await getCurrentAdapter(ctx.context.adapter) as unknown as AdapterLike
            const rawOrg = (session.session as { activeOrganizationId?: unknown }).activeOrganizationId
            const active = await activeMembershipWithAdapter(adapter, session.user.id, nonEmptyString(rawOrg))
            if (active === null) {
              throw apiError(409, { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before submitting." }, responseHeaders)
            }
            if (active.organizationId !== headerOrg) {
              throw apiError(409, { code: "WORKSPACE_CHANGED", message: "Your active workspace changed. Reload the page before submitting." }, responseHeaders)
            }
            targetOrgId = active.organizationId
          } else if (headerOrg !== null && headerOrg !== targetOrgId) {
            throw apiError(409, { code: "WORKSPACE_CHANGED", message: "Your active workspace changed. Reload the page before submitting." }, responseHeaders)
          }
          // Authorize caller permission BEFORE resolving target membership/invitation rows so
          // unauthorized callers cannot distinguish same-workspace IDs from absent or foreign IDs (S5).
          const db = config.database as unknown as ProvisionDb
          if (path === "/organization/remove-member") {
            const caller = await dbFirst<{ id: string; role: string }>(db, `select "id", "role" from "member" where "organizationId" = ? and "userId" = ?`, targetOrgId, session.user.id)
            if (caller === null) throw apiError(403, { code: "FORBIDDEN", message: "You do not have permission to remove members from this workspace." }, responseHeaders)
            const targetKey = nonEmptyString(body.memberIdOrEmail)
            const isSelf = targetKey !== null && (targetKey === caller.id || targetKey.toLowerCase() === session.user.email.toLowerCase())
            if (!hasOrgRole(caller.role, ["owner", "admin"]) && !isSelf) {
              throw apiError(403, { code: "FORBIDDEN", message: "You do not have permission to remove members from this workspace." }, responseHeaders)
            }
            if (targetKey !== null) {
              const target = targetKey.includes("@")
                ? await dbFirst<{ id: string }>(db, `select m."id" from "member" m join "user" u on u."id" = m."userId" where m."organizationId" = ? and lower(u."email") = lower(?)`, targetOrgId, targetKey)
                : await dbFirst<{ id: string }>(db, `select "id" from "member" where "organizationId" = ? and "id" = ?`, targetOrgId, targetKey)
              if (target === null) throw apiError(400, { code: "MEMBER_NOT_FOUND", message: "Member not found" }, responseHeaders)
            }
          } else if (path === "/organization/update-member-role") {
            const caller = await dbFirst<{ id: string; role: string }>(db, `select "id", "role" from "member" where "organizationId" = ? and "userId" = ?`, targetOrgId, session.user.id)
            if (caller === null || !hasOrgRole(caller.role, ["owner", "admin"])) {
              throw apiError(403, { code: "FORBIDDEN", message: "You do not have permission to update members in this workspace." }, responseHeaders)
            }
            const targetId = nonEmptyString(body.memberId)
            if (targetId !== null) {
              const target = await dbFirst<{ id: string }>(db, `select "id" from "member" where "organizationId" = ? and "id" = ?`, targetOrgId, targetId)
              if (target === null) throw apiError(400, { code: "MEMBER_NOT_FOUND", message: "Member not found" }, responseHeaders)
            }
          }
          if (bodyOrg === null) {
            return { context: { ...ctx, body: { ...body, organizationId: targetOrgId } } }
          }
          return
        }
        if (path === "/organization/cancel-invitation") {
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          if (session === null) return
          const invitationId = nonEmptyString((ctx.body as { invitationId?: unknown } | undefined)?.invitationId)
          if (invitationId === null) return
          const db = config.database as unknown as ProvisionDb
          // Authorize caller permission BEFORE resolving target invitation IDs: callers without
          // admin/owner rights anywhere get a uniform 403; admins see only invitations in
          // workspaces they administer (400 INVITATION_NOT_FOUND for absent, foreign, or unprivileged orgs).
          const adminMemberships = await dbAll<{ organizationId: string; role: string }>(
            db,
            `select "organizationId", "role" from "member" where "userId" = ?`,
            session.user.id,
          )
          const adminOrgIds = new Set(adminMemberships.filter((m) => hasOrgRole(m.role, ["owner", "admin"])).map((m) => m.organizationId))
          if (adminOrgIds.size === 0) {
            throw apiError(403, { code: "FORBIDDEN", message: "You do not have permission to cancel invitations." }, responseHeaders)
          }
          const scoped = await dbFirst<{ id: string; organizationId: string }>(
            db,
            `select i."id", i."organizationId" from "invitation" i where i."id" = ?`,
            invitationId,
          )
          if (scoped === null || !adminOrgIds.has(scoped.organizationId)) {
            throw apiError(400, { code: "INVITATION_NOT_FOUND", message: INVITATION_GONE }, responseHeaders)
          }
          return
        }
        if (path === "/organization/reject-invitation") {
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          if (session === null) return
          const invitationId = nonEmptyString((ctx.body as { invitationId?: unknown } | undefined)?.invitationId)
          if (invitationId === null) return
          const db = config.database as unknown as ProvisionDb
          // Scope invitation lookup to the caller's own email so foreign invitations match absent ones.
          const scoped = await dbFirst<{ id: string }>(
            db,
            `select "id" from "invitation" where "id" = ? and lower("email") = lower(?)`,
            invitationId,
            session.user.email,
          )
          if (scoped === null) {
            throw apiError(400, { code: "INVITATION_NOT_FOUND", message: INVITATION_GONE }, responseHeaders)
          }
          return
        }
        if (path === "/organization/set-active") {
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          if (session === null) throw apiError(401, { code: "UNAUTHORIZED", message: "Unauthorized" }, responseHeaders)
          const body = ctx.body as { organizationId?: unknown; organizationSlug?: unknown } | undefined
          if (body?.organizationId === null && body?.organizationSlug === undefined) {
            const db = config.database as unknown as ProvisionDb
            const cleared = await commitClearActiveSession(db, { token: session.session.token, userId: session.user.id })
            if (cleared === null) {
              throw apiError(401, { code: "UNAUTHORIZED", message: "Unauthorized" }, responseHeaders)
            }
            await setSessionCookie(ctx, {
              session: { ...session.session, activeOrganizationId: null, updatedAt: new Date(cleared.updatedAt) },
              user: session.user,
            })
            return null
          }
          const hasIdField = body?.organizationId !== undefined
          const hasSlugField = body?.organizationSlug !== undefined
          const requestedId = nonEmptyString(body?.organizationId)
          const requestedSlug = nonEmptyString(body?.organizationSlug)
          if ((hasIdField && requestedId === null) || (hasSlugField && requestedSlug === null)) {
            throw apiError(403, { code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE }, responseHeaders)
          }
          const currentOrgId = nonEmptyString((session.session as { activeOrganizationId?: unknown }).activeOrganizationId)
          if (requestedId === null && requestedSlug === null && currentOrgId === null) return

          const db = config.database as unknown as ProvisionDb
          type MemberOrgRow = { id: string; name: string; slug: string; logo: string | null; metadata: string | null; createdAt: string | number }
          let row: MemberOrgRow | null = null
          // Every lookup is strictly scoped to the caller's own memberships so absent orgs,
          // non-member orgs, and mismatched ID/slug pairs produce one uniform 403 refusal.
          if (requestedId !== null && requestedSlug !== null) {
            row = await dbFirst<MemberOrgRow>(
              db,
              `select o."id", o."name", o."slug", o."logo", o."metadata", o."createdAt"
               from "member" m join "organization" o on o."id" = m."organizationId"
               where m."userId" = ? and m."organizationId" = ? and o."slug" = ?`,
              session.user.id, requestedId, requestedSlug,
            )
          } else if (requestedId !== null) {
            row = await dbFirst<MemberOrgRow>(
              db,
              `select o."id", o."name", o."slug", o."logo", o."metadata", o."createdAt"
               from "member" m join "organization" o on o."id" = m."organizationId"
               where m."userId" = ? and m."organizationId" = ?`,
              session.user.id, requestedId,
            )
          } else if (requestedSlug !== null) {
            row = await dbFirst<MemberOrgRow>(
              db,
              `select o."id", o."name", o."slug", o."logo", o."metadata", o."createdAt"
               from "member" m join "organization" o on o."id" = m."organizationId"
               where m."userId" = ? and o."slug" = ?`,
              session.user.id, requestedSlug,
            )
          } else if (currentOrgId !== null) {
            row = await dbFirst<MemberOrgRow>(
              db,
              `select o."id", o."name", o."slug", o."logo", o."metadata", o."createdAt"
               from "member" m join "organization" o on o."id" = m."organizationId"
               where m."userId" = ? and m."organizationId" = ?`,
              session.user.id, currentOrgId,
            )
          }
          if (row === null) {
            throw apiError(403, { code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE }, responseHeaders)
          }
          if (isProvisioningMetadata(row.metadata)) {
            throw apiError(409, { code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." }, responseHeaders)
          }
          const updated = await commitActivateSession(db, {
            token: session.session.token,
            userId: session.user.id,
            organizationId: row.id,
            ...requestedSlug === null ? {} : { expectedSlug: requestedSlug },
          })
          if (updated === null) {
            if (!(await isSessionLive(db, session.session.token, session.user.id))) {
              throw apiError(401, { code: "UNAUTHORIZED", message: "Unauthorized" }, responseHeaders)
            }
            const latest = await dbFirst<{ metadata: string | null }>(
              db,
              `select o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."userId" = ? and m."organizationId" = ?`,
              session.user.id, row.id,
            )
            if (latest !== null && isProvisioningMetadata(latest.metadata)) {
              throw apiError(409, { code: "WORKSPACE_PROVISIONING", message: "This workspace is still being provisioned." }, responseHeaders)
            }
            throw apiError(403, { code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE }, responseHeaders)
          }
          await setSessionCookie(ctx, {
            session: { ...session.session, activeOrganizationId: updated.activeOrganizationId, updatedAt: new Date(updated.updatedAt) },
            user: session.user,
          })
          return {
            id: row.id,
            name: row.name,
            slug: row.slug,
            logo: row.logo,
            createdAt: row.createdAt,
            metadata: publicMetadata(row.metadata),
          }
        }
        if (path === "/organization/accept-invitation") {
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          if (session === null) throw apiError(401, { code: "UNAUTHORIZED", message: "Unauthorized" }, responseHeaders)
          const body = ctx.body as { invitationId?: unknown } | undefined
          const invitationId = nonEmptyString(body?.invitationId)
          if (invitationId === null) {
            throw apiError(400, { code: "VALIDATION_ERROR", message: "invitationId is required." }, responseHeaders)
          }
          const db = config.database as unknown as ProvisionDb
          const billing = await billingOf(config.database as unknown as BillingDb, settingsOf(config.billing?.config))
          const outcome = await commitAcceptInvitation(db, {
            invitationId,
            userId: session.user.id,
            userEmail: session.user.email,
            emailVerified: session.user.emailVerified,
            sessionToken: session.session.token,
            billing,
            baseURL: config.baseURL,
          })
          if (outcome.kind === "refused") {
            throw apiError(
              outcome.status,
              { code: outcome.code, message: outcome.message, ...outcome.refusal === undefined ? {} : { refusal: outcome.refusal } },
              responseHeaders,
            )
          }
          await setSessionCookie(ctx, {
            session: { ...session.session, activeOrganizationId: outcome.member.organizationId, updatedAt: new Date(outcome.updatedAt) },
            user: session.user,
          })
          return { invitation: outcome.invitation, member: outcome.member }
        }
        if (path === "/api-key/create") {
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          const isClientRequest = ctx.request !== undefined || ctx.headers !== undefined
          const rawBody = ctx.body
          if (rawBody === null || typeof rawBody !== "object" || Array.isArray(rawBody)) {
            throw apiError(400, { code: "VALIDATION_ERROR", message: "Invalid API key request body." }, responseHeaders)
          }
          const rawRecord = rawBody as Record<string, unknown>
          if (
            isClientRequest
            && (rawRecord.refillAmount !== undefined
              || rawRecord.refillInterval !== undefined
              || rawRecord.rateLimitMax !== undefined
              || rawRecord.rateLimitTimeWindow !== undefined
              || rawRecord.rateLimitEnabled !== undefined
              || rawRecord.permissions !== undefined
              || (rawRecord.remaining !== undefined && rawRecord.remaining !== null))
          ) {
            throw apiError(400, { code: "SERVER_ONLY_PROPERTY", message: "This property can only be set from the server." }, responseHeaders)
          }
          if (ctx.request !== undefined && rawRecord.userId !== undefined) {
            throw apiError(401, { code: "UNAUTHORIZED_SESSION", message: "Unauthorized or invalid session" }, responseHeaders)
          }
          const parsed = createApiKeyRequestSchema.safeParse(rawRecord)
          if (!parsed.success) {
            throw apiError(400, { code: "VALIDATION_ERROR", message: parsed.error.issues[0]?.message ?? "Invalid API key request." }, responseHeaders)
          }
          const body = parsed.data
          const userId = session?.user.id ?? (isClientRequest ? null : nonEmptyString(body.userId))
          if (userId === null) {
            throw apiError(401, { code: "UNAUTHORIZED_SESSION", message: "Unauthorized or invalid session" }, responseHeaders)
          }
          const orgId = nonEmptyString(body.organizationId)
          if (body.referenceId !== undefined && body.referenceId !== orgId) {
            throw apiError(400, { code: "INVALID_ORGANIZATION", message: "Use organizationId to scope a workspace API key." }, responseHeaders)
          }
          if (orgId === null) {
            throw apiError(400, { code: "ORGANIZATION_ID_REQUIRED", message: "A workspace id is required to create an API key." }, responseHeaders)
          }
          if ((body.refillAmount !== undefined && body.refillInterval === undefined) || (body.refillInterval !== undefined && body.refillAmount === undefined)) {
            throw apiError(400, { code: "REFILL_AMOUNT_AND_INTERVAL_REQUIRED", message: "refillAmount and refillInterval must be provided together." }, responseHeaders)
          }
          if (typeof body.expiresIn === "number") {
            const days = body.expiresIn / (3600 * 24)
            if (days < 1 || days > 365) {
              throw apiError(400, { code: days < 1 ? "EXPIRES_IN_IS_TOO_SMALL" : "EXPIRES_IN_IS_TOO_LARGE", message: "expiresIn must be between 1 and 365 days." }, responseHeaders)
            }
          }
          const db = config.database as unknown as ProvisionDb
          const outcome = await commitCreateApiKey(db, {
            userId,
            organizationId: orgId,
            sessionToken: session?.session.token ?? null,
            name: body.name ?? null,
            prefix: body.prefix ?? null,
            expiresIn: body.expiresIn ?? null,
            metadata: body.metadata ?? null,
            permissions: body.permissions ?? null,
            remaining: body.remaining ?? null,
            refillAmount: body.refillAmount ?? null,
            refillInterval: body.refillInterval ?? null,
            ...body.rateLimitEnabled === undefined ? {} : { rateLimitEnabled: body.rateLimitEnabled },
            ...body.rateLimitTimeWindow === undefined ? {} : { rateLimitTimeWindow: body.rateLimitTimeWindow },
            ...body.rateLimitMax === undefined ? {} : { rateLimitMax: body.rateLimitMax },
          })
          if (outcome.kind === "refused") {
            throw apiError(outcome.status, { code: outcome.code, message: outcome.message }, responseHeaders)
          }
          return outcome.apiKey
        }
        if (path === "/oauth2/consent") {
          const body = ctx.body as { accept?: unknown; oauth_query?: unknown } | undefined
          if (body?.accept === false) return
          const session = await getSessionFromCtx(ctx, { disableCookieCache: true })
          const responseHeaders = responseHeadersOf(ctx)
          if (session === null) return
          const headers = ctx.headers ?? ctx.request?.headers
          const expectedOrg = nonEmptyString(headers?.get("x-norn-expected-org"))
          if (expectedOrg === null) {
            throw apiError(400, { code: "WORKSPACE_SCOPE_REQUIRED", message: "Workspace scope header is required to approve assistant access." }, responseHeaders)
          }
          const approvalToken = nonEmptyString(headers?.get(CONSENT_APPROVAL_HEADER))
          if (approvalToken === null) {
            throw apiError(400, { code: "CONSENT_APPROVAL_REQUIRED", message: "Reload this consent screen before approving assistant access." }, responseHeaders)
          }
          const oauthQuery = nonEmptyString(body?.oauth_query)
          if (oauthQuery === null) {
            throw apiError(400, { code: "INVALID_OAUTH_QUERY", message: "Missing OAuth authorization request." }, responseHeaders)
          }
          const queryParams = new URLSearchParams(oauthQuery)
          const clientId = nonEmptyString(queryParams.get("client_id"))
          const redirectUri = nonEmptyString(queryParams.get("redirect_uri"))
          if (clientId === null || redirectUri === null) {
            throw apiError(400, { code: "INVALID_OAUTH_QUERY", message: "Invalid OAuth authorization request." }, responseHeaders)
          }
          const codeChallenge = nonEmptyString(queryParams.get("code_challenge"))
          const state = nonEmptyString(queryParams.get("state"))
          const db = config.database as unknown as ProvisionDb
          const [tokenHash, queryHash] = await Promise.all([sha256Hex(approvalToken), sha256Hex(oauthQuery)])
          // Atomically claim the single-use approval token, serialized per (userId, clientId, grantRef)
          // so no concurrent consent request for the same triple can interleave with consent or rollback.
          let claimed: { id: string; value: string } | null
          try {
            claimed = await dbFirst<{ id: string; value: string }>(
              db,
              `update "verification"
               set "identifier" = 'oauth-approval-claimed:' || "id"
               where "identifier" = ?
                 and ${LIVE_SESSION_SQL}
                 and exists (select 1 from "session" s where s."id" = ? and s."token" = ? and s."userId" = ? and ${LIVE_SESSION_S_SQL})
               returning "id", "value"`,
              `oauth-approval:${tokenHash}`,
              session.session.id,
              session.session.token,
              session.user.id,
            )
          } catch (error) {
            if (error instanceof Error && error.message.includes("oauth.approval_conflict")) {
              throw apiError(409, { code: "CONSENT_IN_PROGRESS", message: "Another approval for this assistant is already in progress. Try again in a moment." }, responseHeaders)
            }
            throw error
          }
          if (claimed === null) {
            throw apiError(409, { code: "CONSENT_APPROVAL_EXPIRED", message: "This approval request expired or was already used. Start connecting again from your assistant." }, responseHeaders)
          }
          const store = oauthScopeStore.getStore()
          if (store !== undefined) {
            store.claimedApprovalId = claimed.id
            store.isConsentAccept = true
          }
          let approval: {
            sessionId?: unknown
            sessionToken?: unknown
            userId?: unknown
            clientId?: unknown
            redirectUri?: unknown
            codeChallenge?: unknown
            state?: unknown
            organizationId?: unknown
            memberId?: unknown
            grantRef?: unknown
            queryHash?: unknown
          }
          try {
            approval = JSON.parse(claimed.value) as typeof approval
          } catch {
            throw apiError(409, { code: "CONSENT_APPROVAL_EXPIRED", message: "This approval request is no longer valid." }, responseHeaders)
          }
          if (
            approval.sessionId !== session.session.id
            || approval.sessionToken !== session.session.token
            || approval.userId !== session.user.id
            || approval.clientId !== clientId
            || approval.redirectUri !== redirectUri
            || (approval.codeChallenge ?? null) !== codeChallenge
            || (approval.state ?? null) !== state
            || approval.organizationId !== expectedOrg
            || approval.queryHash !== queryHash
            || typeof approval.memberId !== "string"
            || approval.memberId === ""
            || approval.grantRef !== encodeGrantRef(expectedOrg, approval.memberId)
          ) {
            throw apiError(409, { code: "WORKSPACE_CHANGED", message: "Your session or active workspace changed. Reload this page and approve again for the workspace you intend." }, responseHeaders)
          }
          const adapter = await getCurrentAdapter(ctx.context.adapter) as unknown as AdapterLike
          const rawOrg = (session.session as { activeOrganizationId?: unknown }).activeOrganizationId
          const member = await activeMembershipWithAdapter(adapter, session.user.id, nonEmptyString(rawOrg))
          if (member === null) {
            throw apiError(409, { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before authorizing an assistant." }, responseHeaders)
          }
          if (expectedOrg !== member.organizationId || member.memberId !== approval.memberId) {
            throw apiError(409, { code: "WORKSPACE_CHANGED", message: "Your active workspace changed. Reload this page and approve again for the workspace you intend." }, responseHeaders)
          }
          const grantRef = encodeGrantRef(member.organizationId, member.memberId)
          // Snapshot any existing consent while holding the exclusive per-(user,client,grant) claim
          // and persist that snapshot on the claimed approval so rollback can restore exact prior
          // scopes even if the session is revoked mid-request.
          const existingConsent = await dbFirst<ConsentSnapshot>(
            db,
            `select "id", "scopes", "resources", "requestedUserInfoClaims", "updatedAt"
             from "oauthConsent"
             where "clientId" = ? and "userId" = ? and "referenceId" = ?`,
            clientId,
            session.user.id,
            grantRef,
          )
          if (existingConsent !== null) {
            await dbRun(
              db,
              `update "verification"
               set "value" = json_set("value", '$.prevConsent', json(?))
               where "id" = ? and "identifier" = 'oauth-approval-claimed:' || "id"`,
              JSON.stringify(existingConsent),
              claimed.id,
            )
          }
          if (store !== undefined) {
            store.referenceId = grantRef
            store.memberId = member.memberId
            store.consentUserId = session.user.id
            store.consentClientId = clientId
            store.previousConsent = existingConsent
          }
        }
      }),
    },
    databaseHooks: {
      user: {
        create: {
          // Defense in depth: refuse an uninvited address before any row is written, by any path.
          before: async (user) => {
            if (!signUpAllowed(config.signUpEmails, String(user.email))) {
              throw new APIError("FORBIDDEN", { code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE })
            }
          },
          // The user-insert trigger commits the pending marker with the user; session creation retries incomplete provisioning.
          after: async (user, ctx) => {
            if (ctx !== null) await provisionWorkspace(config.database as unknown as ProvisionDb, config.secret, user, config.defaultEnvironmentId, await billingModeOn(config.database as unknown as BillingDb))
          },
        },
        // A deleted account's Polar customer (if they ever checked out) goes too; best effort, never blocks the deletion.
        ...config.billing === undefined ? {} : { delete: { after: forgetPolarCustomer(config.billing) } },
      },
      session: {
        create: {
          before: async (session, ctx) => {
            const method = ctx === null ? undefined : AUTH_METHODS[ctx.path as keyof typeof AUTH_METHODS]
            const adapter = ctx === null ? undefined : await getCurrentAdapter(ctx.context.adapter)
            const db = config.database as unknown as ProvisionDb
            if (adapter !== undefined && await stateOf(db, session.userId) === "pending") {
              const user = await adapter.findOne<{ id: string; name: string; email: string }>({ model: "user", where: [{ field: "id", value: session.userId }] })
              if (user === null) throw new Error("Cannot provision a workspace for a missing user")
              await provisionWorkspace(db, config.secret, user, config.defaultEnvironmentId, await billingModeOn(config.database as unknown as BillingDb))
            }
            const pick = async (where: { field: string; value: string }[]) => {
              if (adapter === undefined) return undefined
              const rows = await adapter.findMany<{ organizationId: string }>({
                model: "member",
                where: [{ field: "userId", value: session.userId }, ...where],
                sortBy: { field: "createdAt", direction: "asc" },
                limit: 100,
              })
              for (const row of rows) {
                const org = await adapter.findOne<{ metadata?: unknown }>({ model: "organization", where: [{ field: "id", value: row.organizationId }] })
                if (org !== null && !isProvisioningMetadata(org.metadata)) return row
              }
              return undefined
            }
            // The stage's default organization when the user belongs to it, else the earliest ready membership
            // (the same rule activeMembership uses when a session has none).
            const preferred = config.defaultOrganizationId === undefined ? undefined : await pick([{ field: "organizationId", value: config.defaultOrganizationId }])
            const member = preferred ?? await pick([])
            return { data: { ...session, authMethod: method ?? null, activeOrganizationId: member?.organizationId ?? null } }
          },
        },
      },
    },
  } satisfies BetterAuthOptions
}

export interface Membership {
  readonly memberId: string
  readonly organizationId: string
  readonly organizationName: string
  readonly role: string
  /**
   * The workspace's data environment (cell): the reserved `environmentId` that server-side provisioning wrote into the
   * organization's metadata, or null. Clients cannot write it (organization create and update strip reserved keys).
   */
  readonly environmentId: string | null
}

async function activeMembershipWithAdapter(
  adapter: AdapterLike,
  userId: string,
  activeOrganizationId: string | null | undefined,
): Promise<Membership | null> {
  const where = [{ field: "userId", value: userId }, ...activeOrganizationId == null ? [] : [{ field: "organizationId", value: activeOrganizationId }]]
  const rows = await adapter.findMany<{ id: string; organizationId: string; role: string }>({
    model: "member",
    where,
    sortBy: { field: "createdAt", direction: "asc" },
    limit: activeOrganizationId == null ? 100 : 1,
  })
  let member: { id: string; organizationId: string; role: string } | undefined
  let org: { name: string; metadata?: unknown } | null = null
  for (const candidate of rows) {
    const found = await adapter.findOne<{ name: string; metadata?: unknown }>({ model: "organization", where: [{ field: "id", value: candidate.organizationId }] })
    if (found === null || isProvisioningMetadata(found.metadata)) continue
    member = candidate
    org = found
    break
  }
  if (member === undefined || org === null) return null
  const environmentId = metadataObject(org.metadata).environmentId
  return { memberId: member.id, organizationId: member.organizationId, organizationName: org.name, role: member.role, environmentId: typeof environmentId === "string" && environmentId.trim() !== "" ? environmentId : null }
}

/**
 * The organization a session acts in: its active organization, or (for a
 * session created before its user's first organization existed, as at sign-up)
 * the user's earliest ready membership. Null when the user belongs to none.
 */
export async function activeMembership(auth: Auth, userId: string, activeOrganizationId: string | null | undefined): Promise<Membership | null> {
  const { adapter } = await auth.$context
  return activeMembershipWithAdapter(adapter as unknown as AdapterLike, userId, activeOrganizationId)
}

const isOAuthMembershipAbort = (error: unknown): boolean =>
  error instanceof Error && error.message.includes("oauth.membership_invalid")

export function createAuth(config: AuthConfig) {
  const self: Self = {}
  const auth = betterAuth(authOptions(config, self))
  self.auth = auth
  const db = config.database as unknown as ProvisionDb
  const consentResponseFailed = async (response: Response, store: OAuthConsentScope): Promise<boolean> => {
    if (response.status >= 400) return true
    if (!store.isConsentAccept) return false
    const location = response.headers.get("location")
    if (location !== null) {
      try {
        const parsed = new URL(location, config.baseURL)
        if (parsed.searchParams.has("error") || !parsed.searchParams.has("code")) return true
      } catch {
        return true
      }
      return false
    }
    const contentType = response.headers.get("content-type") ?? ""
    if (contentType.includes("application/json")) {
      const parsed = await response.clone().json().catch(() => null) as { url?: unknown; redirect?: unknown; error?: unknown } | null
      if (parsed === null || typeof parsed.error === "string") return true
      if (typeof parsed.url === "string") {
        try {
          const target = new URL(parsed.url, config.baseURL)
          if (target.searchParams.has("error") || !target.searchParams.has("code")) return true
        } catch {
          return true
        }
        return false
      }
      return true
    }
    return true
  }
  const cleanupApproval = async (store: OAuthConsentScope, rollbackConsent: boolean) => {
    const claimId = store.claimedApprovalId
    if (claimId === undefined) return
    // Roll back any staged consent insert or update BEFORE releasing the exclusive claimed approval
    // row, guarded on the claimed approval still existing (proving code issuance did not commit).
    if (rollbackConsent && store.consentClientId !== undefined && store.consentUserId !== undefined && store.referenceId !== undefined) {
      if (store.previousConsent === null) {
        await dbRun(
          db,
          `delete from "oauthConsent"
           where "clientId" = ? and "userId" = ? and "referenceId" = ?
             and exists (select 1 from "verification" where "id" = ? and "identifier" = 'oauth-approval-claimed:' || "id")`,
          store.consentClientId,
          store.consentUserId,
          store.referenceId,
          claimId,
        ).catch(() => undefined)
      } else if (store.previousConsent !== undefined) {
        const prev = store.previousConsent
        await dbRun(
          db,
          `update "oauthConsent"
           set "scopes" = ?, "resources" = ?, "requestedUserInfoClaims" = ?, "updatedAt" = ?
           where "id" = ? and "clientId" = ? and "userId" = ? and "referenceId" = ?
             and exists (select 1 from "verification" where "id" = ? and "identifier" = 'oauth-approval-claimed:' || "id")`,
          prev.scopes,
          prev.resources,
          prev.requestedUserInfoClaims,
          prev.updatedAt,
          prev.id,
          store.consentClientId,
          store.consentUserId,
          store.referenceId,
          claimId,
        ).catch(() => undefined)
      }
    }
    await dbRun(db, `delete from "verification" where "id" = ?`, claimId).catch(() => undefined)
  }
  const rawHandler = auth.handler
  auth.handler = async (request: Request) => {
    const store: OAuthConsentScope = {}
    let failed = false
    try {
      const response = await oauthScopeStore.run(store, () => rawHandler(request))
      failed = await consentResponseFailed(response, store)
      return response
    } catch (error) {
      failed = true
      if (isOAuthMembershipAbort(error)) {
        return Response.json(
          { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before authorizing an assistant." },
          { status: 409, headers: { "cache-control": "no-store" } },
        )
      }
      throw error
    } finally {
      await cleanupApproval(store, failed)
    }
  }
  const rawApi = auth.api
  auth.api = new Proxy(rawApi, {
    get(target, prop, receiver) {
      const value = Reflect.get(target, prop, receiver)
      if (typeof value !== "function") return value
      return (...args: unknown[]) => {
        const outer = oauthScopeStore.getStore()
        const store = outer ?? {}
        const invoke = async () => {
          let failed = false
          try {
            const result = await (value as (...a: unknown[]) => unknown).apply(target, args)
            if (store.isConsentAccept) {
              if (result instanceof Response) {
                failed = await consentResponseFailed(result, store)
              } else {
                const url = (result as { url?: unknown } | null)?.url
                if (typeof url !== "string") {
                  failed = true
                } else {
                  try {
                    const parsed = new URL(url, config.baseURL)
                    if (parsed.searchParams.has("error") || !parsed.searchParams.has("code")) failed = true
                  } catch {
                    failed = true
                  }
                }
              }
            }
            return result
          } catch (error) {
            failed = true
            if (isOAuthMembershipAbort(error)) {
              throw new APIError("CONFLICT", { code: "NO_ACTIVE_WORKSPACE", message: "Select a ready workspace before authorizing an assistant." })
            }
            throw error
          } finally {
            if (outer === undefined) await cleanupApproval(store, failed)
          }
        }
        return outer === undefined ? oauthScopeStore.run(store, invoke) : invoke()
      }
    },
  })
  return auth
}
export type Auth = ReturnType<typeof createAuth>
