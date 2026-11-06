import { apiKey } from "@better-auth/api-key"
import { betterAuth, getCurrentAdapter, type BetterAuthOptions } from "better-auth"
import { APIError } from "better-auth/api"
import { createAccessControl } from "better-auth/plugins/access"
import { jwt } from "better-auth/plugins/jwt"
import { magicLink } from "better-auth/plugins/magic-link"
import { organization } from "better-auth/plugins/organization"
import { adminAc, defaultStatements, memberAc, ownerAc } from "better-auth/plugins/organization/access"

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
  readonly trustedOrigins?: readonly string[]
  /** Absent when this deployment has no mail sender: magic links then refuse. */
  readonly sendMagicLink?: (mail: MagicLinkMail) => Promise<void>
}

const MINUTE = 60

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
    trustedOrigins: [...(config.trustedOrigins ?? [config.baseURL])],
    // No sign-in on sign-up: Better Auth then answers a registered address exactly as a new one
    // (no session either way), so sign-up does not reveal which emails have accounts.
    emailAndPassword: {
      enabled: true,
      autoSignIn: false,
      minPasswordLength: 12,
      maxPasswordLength: 128,
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
            // The earliest membership, the same rule activeMembership uses when a session has none.
            const [member] = ctx === null ? [] : await (await getCurrentAdapter(ctx.context.adapter)).findMany<{ organizationId: string }>({
              model: "member",
              where: [{ field: "userId", value: session.userId }],
              sortBy: { field: "createdAt", direction: "asc" },
              limit: 1,
            })
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
