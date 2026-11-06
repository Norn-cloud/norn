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
  /** Absent when this deployment has no mail sender: magic links then refuse. */
  readonly sendMagicLink?: (mail: MagicLinkMail) => Promise<void>
}

const MINUTE = 60

/** The running instance, for hooks that need its database outside an endpoint context. */
interface Self { auth?: { readonly $context: Promise<{ adapter: unknown }> } }

type AuthAdapter = Awaited<ReturnType<typeof getCurrentAdapter>>

async function provisionWorkspace(adapter: AuthAdapter, secret: string, user: { id: string; name: string; email: string }) {
  const identity = await workspaceIdentity(secret, user.id)
  const find = () => adapter.findOne<{ id: string; metadata?: string | null }>({ model: "organization", where: [{ field: "id", value: identity.id }] })
  let org = await find()
  if (org === null) {
    try {
      const data = { id: identity.id, name: `${user.name || user.email}'s workspace`, slug: `u-${identity.id.slice(10)}`, metadata: identity.metadata, createdAt: new Date() }
      org = await adapter.create<typeof data>({ model: "organization", forceAllowId: true, data })
    } catch (error) {
      org = await find()
      if (org === null) throw error
    }
  }
  if (org.metadata !== identity.metadata) throw new Error("Workspace ownership could not be verified")
  const member = await adapter.findOne<{ id: string }>({ model: "member", where: [{ field: "organizationId", value: identity.id }, { field: "userId", value: user.id }] })
  if (member === null) await adapter.create({ model: "member", data: { organizationId: identity.id, userId: user.id, role: "owner", createdAt: new Date() } })
}

/** A server-authenticated workspace identity, independent of user-editable slugs. */
async function workspaceIdentity(secret: string, userId: string) {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"])
  const sign = async (value: string) => Array.from(new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(value))), (byte) => byte.toString(16).padStart(2, "0")).join("")
  const id = `workspace_${await sign(`id:${userId}`)}`
  return { id, metadata: JSON.stringify({ studioProvisioningProof: await sign(`owner:${userId}:${id}`) }) }
}

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
        sendMagicLink: async ({ email, url }) => {
          if (config.sendMagicLink === undefined) throw new APIError("SERVICE_UNAVAILABLE", { message: "Email sign-in is not available on this deployment." })
          await config.sendMagicLink({ to: email, url })
        },
      }),
      // Short-lived tokens the Worker mints for the Norn host (#817); the host verifies them against /api/auth/jwks.
      jwt({ jwt: { issuer: config.baseURL, audience: "norn-host", expirationTime: "5m" } }),
    ],
    databaseHooks: {
      user: {
        create: {
          // This hook runs after commit; session creation retries any incomplete provisioning.
          after: async (user, ctx) => {
            if (ctx !== null) await provisionWorkspace(await getCurrentAdapter(ctx.context.adapter), config.secret, user)
          },
        },
      },
      session: {
        create: {
          before: async (session, ctx) => {
            const method = ctx === null ? undefined : AUTH_METHODS[ctx.path as keyof typeof AUTH_METHODS]
            if (ctx !== null) {
              const adapter = await getCurrentAdapter(ctx.context.adapter)
              const existing = await adapter.findMany<{ organizationId: string }>({ model: "member", where: [{ field: "userId", value: session.userId }], limit: 1 })
              if (existing.length === 0) {
                const user = await adapter.findOne<{ id: string; name: string; email: string }>({ model: "user", where: [{ field: "id", value: session.userId }] })
                if (user === null) throw new Error("Cannot provision a workspace for a missing user")
                await provisionWorkspace(adapter, config.secret, user)
              }
            }
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
