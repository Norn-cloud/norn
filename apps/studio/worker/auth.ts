import { apiKey } from "@better-auth/api-key"
import { betterAuth, getCurrentAdapter, type BetterAuthOptions } from "better-auth"
import { APIError } from "better-auth/api"
import { createAccessControl } from "better-auth/plugins/access"
import { genericOAuth } from "better-auth/plugins/generic-oauth"
import { jwt } from "better-auth/plugins/jwt"
import { magicLink } from "better-auth/plugins/magic-link"
import { organization } from "better-auth/plugins/organization"
import { adminAc, defaultStatements, memberAc, ownerAc } from "better-auth/plugins/organization/access"
import { hashPassword, verifyPassword, type PasswordScrypt } from "./password.ts"

/**
 * Better Auth for the hosted Studio: email + password, email magic link,
 * organizations (owner/admin/member/viewer) and organization-owned API keys
 * for service accounts. Identity only: what each role may do in Norn is
 * decided by Norn's AccessIR/Cedar policy on the server, never here or in the client.
 */

/** The sign-in path that created a session, as Norn's `actor.authMethod`. */
export const AUTH_METHODS = { "/sign-up/email": "password", "/sign-in/email": "password", "/magic-link/verify": "magic_link" } as const
export type AuthMethod = (typeof AUTH_METHODS)[keyof typeof AUTH_METHODS] | "api_key"

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
  /** Absent when this deployment has no mail sender: magic links then refuse. */
  readonly sendMagicLink?: (mail: MagicLinkMail) => Promise<void>
  /**
   * The organization a new session starts in when its user is a member, e.g. the
   * seeded `org_demo` on preview; otherwise the user's earliest membership.
   */
  readonly defaultOrganizationId?: string
  /** "js" where the runtime has no `node:crypto.scrypt` (celld); absent or "native" otherwise. */
  readonly scrypt?: PasswordScrypt
  /** The tool gateway (#772) people link to their Norn account; absent on stages without one. */
  readonly gateway?: GatewayClient
}

/** Studio's OAuth client at the tool gateway (Executor v2). The secret comes from Infisical by name. */
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
    disableSignUp: true,
    disableImplicitSignUp: true,
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

/** The running instance, for hooks that need its database outside an endpoint context. */
interface Self { auth?: { readonly $context: Promise<{ adapter: unknown }> } }

export function authOptions(config: AuthConfig, self: Self = {}) {
  return {
    appName: "Norn Studio",
    database: config.database,
    secret: config.secret,
    baseURL: config.baseURL,
    basePath: "/api/auth",
    // Better Auth refuses state-changing requests whose Origin is not listed (its CSRF check).
    trustedOrigins: [config.baseURL],
    // No sign-in on sign-up: Better Auth then answers a registered address exactly as a new one
    // (no session either way), so sign-up does not reveal which emails have accounts.
    emailAndPassword: {
      enabled: true,
      autoSignIn: false,
      minPasswordLength: 12,
      maxPasswordLength: 128,
      // Better Auth's own hash, computed in pure JS: byte-identical, so accounts move between runtimes.
      ...config.scrypt === "js" ? { password: { hash: hashPassword, verify: verifyPassword } } : {},
      // A registered address skips the four writes a new account makes (user, credential,
      // organization, member); make the same number of database writes so latency does not tell them apart.
      onExistingUserSignUp: async () => {
        const adapter = await getCurrentAdapter((await self.auth?.$context)?.adapter as never)
        for (let pair = 0; pair < 2; pair++) {
          const now = new Date()
          const row = await adapter.create<{ id: string }>({ model: "verification", data: { identifier: `signup-pad:${crypto.randomUUID()}`, value: "", expiresAt: now, createdAt: now, updatedAt: now } })
          await adapter.delete({ model: "verification", where: [{ field: "id", value: row.id }] })
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
      },
    },
    advanced: {
      // Explicit: Better Auth otherwise turns its origin (CSRF) check off whenever NODE_ENV is "test".
      disableOriginCheck: false,
      useSecureCookies: true,
      // The same cookie prefix as Norn's other Better Auth apps; host-only, never shared across subdomains.
      cookiePrefix: "norn",
      defaultCookieAttributes: { httpOnly: true, secure: true, sameSite: "lax" },
      // Cloudflare sets this header itself; X-Forwarded-For is client-controlled.
      ipAddress: { ipAddressHeaders: ["cf-connecting-ip"] },
    },
    plugins: [
      // Only a verified address may accept an invitation (GHSA-fmh4-wcc4-5jm3): until a mail
      // sender verifies addresses, nobody can join an organization by claiming an invited email.
      organization({ ac, roles: ORG_ROLES, creatorRole: "owner", allowUserToCreateOrganization: true, requireEmailVerificationOnInvitation: true }),
      apiKey({ references: "organization", enableMetadata: true, rateLimit: { enabled: true, timeWindow: 60_000, maxRequests: 120 } }),
      magicLink({
        expiresIn: 10 * MINUTE,
        disableSignUp: true,
        sendMagicLink: async ({ email, url }) => {
          if (config.sendMagicLink === undefined) throw new APIError("SERVICE_UNAVAILABLE", { message: "Email sign-in is not available on this deployment." })
          await config.sendMagicLink({ to: email, url })
        },
      }),
      // Short-lived tokens the Worker mints for the Norn host (#817); the host verifies them against /api/auth/jwks.
      jwt({ jwt: { issuer: config.baseURL, audience: "norn-host", expirationTime: "5m" } }),
      ...config.gateway === undefined ? [] : [gatewayLink(config.gateway)],
    ],
    databaseHooks: {
      user: {
        create: {
          // Every account starts with its own organization, so a fresh session has a scope.
          after: async (user, ctx) => {
            if (ctx === null) return
            const now = new Date()
            // Inside the sign-up transaction where the database has one (SQLite); D1 has none.
            const adapter = await getCurrentAdapter(ctx.context.adapter)
            const org = await adapter.create<{ id: string }>({
              model: "organization",
              data: { name: `${user.name || user.email}'s workspace`, slug: `u-${user.id.toLowerCase()}`, createdAt: now },
            })
            await adapter.create({ model: "member", data: { organizationId: org.id, userId: user.id, role: "owner", createdAt: now } })
          },
        },
      },
      session: {
        create: {
          before: async (session, ctx) => {
            const method = ctx === null ? undefined : AUTH_METHODS[ctx.path as keyof typeof AUTH_METHODS]
            const adapter = ctx === null ? undefined : await getCurrentAdapter(ctx.context.adapter)
            const first = (where: { field: string; value: string }[]) => adapter === undefined ? Promise.resolve([]) : adapter.findMany<{ organizationId: string }>({
              model: "member",
              where: [{ field: "userId", value: session.userId }, ...where],
              sortBy: { field: "createdAt", direction: "asc" },
              limit: 1,
            })
            // The stage's default organization when the user belongs to it, else the earliest membership
            // (the same rule activeMembership uses when a session has none).
            const preferred = config.defaultOrganizationId === undefined ? [] : await first([{ field: "organizationId", value: config.defaultOrganizationId }])
            const [member] = preferred.length > 0 ? preferred : await first([])
            return { data: { ...session, authMethod: method ?? null, activeOrganizationId: member?.organizationId ?? null } }
          },
        },
      },
    },
  } satisfies BetterAuthOptions
}

export interface Membership {
  readonly organizationId: string
  readonly organizationName: string
  readonly role: string
}

/**
 * The organization a session acts in: its active organization, or (for a
 * session created before its user's first organization existed, as at sign-up)
 * the user's earliest membership. Null when the user belongs to none.
 */
export async function activeMembership(auth: Auth, userId: string, activeOrganizationId: string | null | undefined): Promise<Membership | null> {
  const { adapter } = await auth.$context
  const where = [{ field: "userId", value: userId }, ...activeOrganizationId == null ? [] : [{ field: "organizationId", value: activeOrganizationId }]]
  const [member] = await adapter.findMany<{ organizationId: string; role: string }>({ model: "member", where, sortBy: { field: "createdAt", direction: "asc" }, limit: 1 })
  if (member === undefined) return null
  const org = await adapter.findOne<{ name: string }>({ model: "organization", where: [{ field: "id", value: member.organizationId }] })
  return org === null ? null : { organizationId: member.organizationId, organizationName: org.name, role: member.role }
}

export function createAuth(config: AuthConfig) {
  const self: Self = {}
  const auth = betterAuth(authOptions(config, self))
  self.auth = auth
  return auth
}
export type Auth = ReturnType<typeof createAuth>
