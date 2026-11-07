import { apiKey } from "@better-auth/api-key"
import { betterAuth, getCurrentAdapter, type BetterAuthOptions } from "better-auth"
import { APIError } from "better-auth/api"
import { createAccessControl } from "better-auth/plugins/access"
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
  /** Optional server-held default cell for newly provisioned workspaces. */
  readonly defaultEnvironmentId?: string
  /** Absent when this deployment has no mail sender: magic links then refuse. */
  readonly sendMagicLink?: (mail: MagicLinkMail) => Promise<void>
  /**
   * The organization a new session starts in when its user is a member, e.g. the
   * seeded `org_demo` on preview; otherwise the user's earliest membership.
   */
  readonly defaultOrganizationId?: string
  /** "js" where the runtime has no `node:crypto.scrypt` (celld); absent or "native" otherwise. */
  readonly scrypt?: PasswordScrypt
}

const MINUTE = 60

/** The running instance, for hooks that need its database outside an endpoint context. */
interface Self { auth?: { readonly $context: Promise<{ adapter: unknown }> } }

type SqlStatement = { readonly sql: string; readonly params: readonly string[] }
type ProvisionDb = {
  prepare(sql: string): { bind?: (...params: string[]) => { first<T>(): Promise<T | null> }; get?: (...params: string[]) => unknown; run?: (...params: string[]) => unknown }
  batch?: (statements: unknown[]) => Promise<unknown>
  transaction?: (fn: () => void) => () => void
}

async function stateOf(db: ProvisionDb, userId: string): Promise<string | undefined> {
  const statement = db.prepare(`select "status" from "workspace_provisioning" where "userId" = ?`)
  const row = statement.bind ? await statement.bind(userId).first<{ status: string }>() : statement.get?.(userId) as { status: string } | undefined
  return row?.status
}

/** Execute all workspace writes and completion in one database transaction. */
async function provisionBatch(db: ProvisionDb, statements: readonly SqlStatement[]): Promise<void> {
  if (db.batch) {
    const prepared = statements.map(({ sql, params }) => {
      const statement = db.prepare(sql)
      if (!statement.bind) throw new Error("D1 statement binding unavailable")
      return statement.bind(...params)
    })
    await db.batch(prepared)
  } else if (db.transaction) {
    db.transaction(() => { for (const { sql, params } of statements) db.prepare(sql).run?.(...params) })()
  } else throw new Error("Atomic provisioning is unavailable")
}

async function provisionWorkspace(db: ProvisionDb, secret: string, user: { id: string; name: string; email: string }, defaultEnvironmentId?: string) {
  if (await stateOf(db, user.id) !== "pending") return
  const identity = await workspaceIdentity(secret, user.id)
  const metadata = JSON.stringify({ ...JSON.parse(identity.metadata), ...defaultEnvironmentId ? { environmentId: defaultEnvironmentId } : {} })
  const at = new Date().toISOString()
  const pending = `exists (select 1 from "workspace_provisioning" where "userId" = ? and "status" = 'pending')`
  await provisionBatch(db, [
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

/** Reserved metadata is assigned only by server-side provisioning. */
const reservedMetadata = (key: string) => key === "environmentId" || key.startsWith("norn.")
function metadataObject(value: unknown): Record<string, unknown> {
  if (typeof value === "string") { try { return metadataObject(JSON.parse(value)) } catch { return {} } }
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {}
}
const publicMetadata = (value: unknown) => Object.fromEntries(Object.entries(metadataObject(value)).filter(([key]) => !reservedMetadata(key)))

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
      organization({ ac, roles: ORG_ROLES, creatorRole: "owner", allowUserToCreateOrganization: true, requireEmailVerificationOnInvitation: true,
        organizationHooks: {
          beforeCreateOrganization: async ({ organization }) => ({ data: { ...organization, ...organization.metadata === undefined ? {} : { metadata: publicMetadata(organization.metadata) } } }),
          beforeUpdateOrganization: async ({ organization, member }) => {
            if (organization.metadata === undefined) return
            const adapter = await getCurrentAdapter((await self.auth?.$context)?.adapter as never)
            const stored = await adapter.findOne<{ metadata?: string | null }>({ model: "organization", where: [{ field: "id", value: member.organizationId }] })
            if (stored === null) throw new Error("Organization no longer exists")
            const protectedFields = Object.fromEntries(Object.entries(metadataObject(stored.metadata)).filter(([key]) => reservedMetadata(key)))
            return { data: { ...organization, metadata: { ...publicMetadata(organization.metadata), ...protectedFields } } }
          },
        },
      }),
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
          // The user-insert trigger commits the pending marker with the user; session creation retries incomplete provisioning.
          after: async (user, ctx) => {
            if (ctx !== null) await provisionWorkspace(config.database as unknown as ProvisionDb, config.secret, user, config.defaultEnvironmentId)
          },
        },
      },
      session: {
        create: {
          before: async (session, ctx) => {
            const method = ctx === null ? undefined : AUTH_METHODS[ctx.path as keyof typeof AUTH_METHODS]
            const adapter = ctx === null ? undefined : await getCurrentAdapter(ctx.context.adapter)
            if (adapter !== undefined && await stateOf(config.database as unknown as ProvisionDb, session.userId) === "pending") {
              const user = await adapter.findOne<{ id: string; name: string; email: string }>({ model: "user", where: [{ field: "id", value: session.userId }] })
              if (user === null) throw new Error("Cannot provision a workspace for a missing user")
              await provisionWorkspace(config.database as unknown as ProvisionDb, config.secret, user, config.defaultEnvironmentId)
            }
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
  /**
   * The workspace's data environment (cell): the reserved `environmentId` that server-side provisioning wrote into the
   * organization's metadata, or null. Clients cannot write it (organization create and update strip reserved keys).
   */
  readonly environmentId: string | null
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
  const org = await adapter.findOne<{ name: string; metadata?: unknown }>({ model: "organization", where: [{ field: "id", value: member.organizationId }] })
  if (org === null) return null
  const environmentId = metadataObject(org.metadata).environmentId
  return { organizationId: member.organizationId, organizationName: org.name, role: member.role, environmentId: typeof environmentId === "string" && environmentId.trim() !== "" ? environmentId : null }
}

export function createAuth(config: AuthConfig) {
  const self: Self = {}
  const auth = betterAuth(authOptions(config, self))
  self.auth = auth
  return auth
}
export type Auth = ReturnType<typeof createAuth>
