import { atomically, publicMetadata, BILLING_PAYER, BILLED, type Auth, type ProvisionDb } from "./auth.ts"
import { admitCell, admitMember, limitRaised, refusalMessage, type Admission, type BillingDb, type BillingSettings } from "./billing.ts"

/**
 * Creating a workspace and accepting an invitation, each as one database transaction. Better Auth's
 * own routes write the workspace and its owner (or the accepted invitation and the member) in separate
 * statements, so an interruption or a plan-limit refusal between them leaves half a change: a workspace
 * without its owner holding a payer's cell, or an accepted invitation without its member that can never
 * be retried. With billing on, the Worker refuses those routes (index.ts) and serves these instead.
 *
 * Authorization is Better Auth's, unchanged: its session, and for an invitation the same checks its
 * accept route makes (pending and unexpired, addressed to the session's email, verified address,
 * membership limit). Each check is made again inside the write, against the rows and the database's
 * clock at commit, so nothing that changes between the checks and the write (an expiry, a changed or
 * unverified address, a concurrent acceptance) can pass. The database enforces the plan limits
 * (migration 0011) inside the transaction, so a refusal rolls the whole change back.
 */

export interface WorkspaceContext {
  readonly auth: Auth
  readonly db: D1Database
  readonly billing: BillingSettings | undefined
  readonly baseURL: string
}

const json = (body: unknown, status = 200) => Response.json(body, { status, headers: { "cache-control": "no-store" } })
/** Better Auth's error shape, so a client reads these as it reads Better Auth's. */
const failure = (status: number, code: string, message: string) => json({ code, message }, status)
/** A plan limit, explained from what the server authorized: the caller's own cells, or the invitation's workspace. */
function planLimit(admission: Admission | undefined, fallback: string): Response {
  const refusal = admission?.kind === "plan-limit" ? admission : undefined
  return json({ code: "PLAN_LIMIT", message: refusal === undefined ? fallback : refusalMessage(refusal), ...refusal === undefined ? {} : { refusal } }, 403)
}

const INVITATION_GONE = "This invitation is no longer open: it expired or was withdrawn. Ask the workspace's admins for a new one."
/** Better Auth's default membership limit (its accept route refuses at 100 members). */
const MEMBERSHIP_LIMIT = 100

async function body(request: Request): Promise<Record<string, unknown> | null> {
  const value = await request.json().catch(() => null)
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null
}

async function first<T>(db: D1Database | ProvisionDb, sql: string, ...params: string[]): Promise<T | null> {
  const statement = (db as ProvisionDb).prepare(sql)
  if (statement.bind) return statement.bind(...params).first<T>()
  return (statement.get?.(...params) as T | undefined) ?? null
}

/** Make the new workspace the session's active one, as Better Auth's routes do. */
async function activate(auth: Auth, token: string, organizationId: string): Promise<void> {
  const context = await auth.$context
  await context.internalAdapter.updateSession(token, { activeOrganizationId: organizationId })
}

/** POST /api/workspaces { name, slug, logo?, metadata?, keepCurrentActiveOrganization? }: the workspace and its owner in one transaction. */
export async function createWorkspace(request: Request, context: WorkspaceContext): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const input = await body(request)
  const name = typeof input?.name === "string" ? input.name.trim() : ""
  const slug = typeof input?.slug === "string" ? input.slug.trim() : ""
  const logo = typeof input?.logo === "string" ? input.logo : null
  if (name === "" || slug === "") return failure(400, "VALIDATION_ERROR", "A workspace needs a name and a slug.")
  if (await first(context.db, `select 1 as "taken" from "organization" where "slug" = ?`, slug) !== null) return failure(400, "ORGANIZATION_ALREADY_EXISTS", "Organization already exists")
  const id = crypto.randomUUID()
  const at = new Date().toISOString()
  // Server-only keys: the payer, read by migration 0011's triggers, only while billing is on.
  const metadata = { ...publicMetadata(input?.metadata), ...context.billing === undefined ? {} : { [BILLING_PAYER]: session.user.id, [BILLED]: true } }
  const member = { id: crypto.randomUUID(), organizationId: id, userId: session.user.id, role: "owner", createdAt: at }
  try {
    await atomically(context.db as unknown as ProvisionDb, [
      { sql: `insert into "organization" ("id", "name", "slug", "logo", "metadata", "createdAt") values (?, ?, ?, ?, ?, ?)`, params: [id, name, slug, logo, JSON.stringify(metadata), at] },
      { sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values (?, ?, ?, ?, ?)`, params: [member.id, id, member.userId, member.role, at] },
    ])
  } catch (error) {
    if (limitRaised(error) === "cells") return planLimit(await admitCell(context.db as unknown as BillingDb, context.billing, context.baseURL, session.user.id), "Your plan's workspace limit is reached.")
    if (/UNIQUE constraint failed: organization\.slug/.test(error instanceof Error ? error.message : "")) return failure(400, "ORGANIZATION_ALREADY_EXISTS", "Organization already exists")
    throw error
  }
  if (input?.keepCurrentActiveOrganization !== true) await activate(context.auth, session.session.token, id)
  return json({ id, name, slug, logo, metadata: publicMetadata(metadata), createdAt: at, members: [member] })
}

/** An invitation's expiry as stored (Better Auth writes dates as text; tests may write epoch ms). */
const expiry = (value: unknown) => typeof value === "number" ? value : typeof value === "string" ? Date.parse(value) : Number.NaN

/**
 * POST /api/workspaces/invitations/accept { invitationId }: the invitation turns accepted and the member
 * joins in one transaction. A plan-limit refusal rolls both back: the invitation stays pending, and a
 * retry joins once a seat frees.
 */
export async function acceptWorkspaceInvitation(request: Request, context: WorkspaceContext): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const input = await body(request)
  if (typeof input?.invitationId !== "string") return failure(400, "VALIDATION_ERROR", "invitationId is required.")
  const invitationId = input.invitationId
  // Better Auth's accept checks, in its order, against the invitation as stored.
  const invitation = await first<{ id: string; organizationId: string; email: string; role: string | null; status: string; expiresAt: unknown }>(context.db,
    `select "id", "organizationId", "email", "role", "status", "expiresAt" from "invitation" where "id" = ?`, invitationId)
  if (invitation === null || !(expiry(invitation.expiresAt) > Date.now()) || invitation.status !== "pending") return failure(400, "INVITATION_NOT_FOUND", INVITATION_GONE)
  if (invitation.email.toLowerCase() !== session.user.email.toLowerCase()) return failure(403, "YOU_ARE_NOT_THE_RECIPIENT_OF_THE_INVITATION", "You are not the recipient of the invitation")
  if (!session.user.emailVerified) return failure(403, "EMAIL_VERIFICATION_REQUIRED_BEFORE_ACCEPTING_OR_REJECTING_INVITATION", "Email verification required before accepting or rejecting invitation")
  if (await first(context.db, `select 1 as "found" from "organization" where "id" = ?`, invitation.organizationId) === null) return failure(400, "ORGANIZATION_NOT_FOUND", "Organization not found")
  const count = await first<{ n: number }>(context.db, `select count(*) as "n" from "member" where "organizationId" = ?`, invitation.organizationId)
  if ((count?.n ?? 0) >= MEMBERSHIP_LIMIT) return failure(403, "ORGANIZATION_MEMBERSHIP_LIMIT_REACHED", "Organization membership limit reached")
  const member = { id: crypto.randomUUID(), organizationId: invitation.organizationId, userId: session.user.id, role: invitation.role ?? "member", createdAt: new Date().toISOString() }
  try {
    // The invitation turns accepted only if, at the write, it is still pending and unexpired (the
    // database's clock), the session's user still has a verified address it was sent to, its workspace
    // exists and holds fewer than the membership limit; the member is inserted only if that update
    // happened. A transaction is serialized, so two acceptances cannot both count 99 members.
    await atomically(context.db as unknown as ProvisionDb, [
      { sql: `update "invitation" set "status" = 'accepted' where "id" = ? and "status" = 'pending'
        and (case when typeof("expiresAt") in ('integer', 'real') then "expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday("expiresAt") > julianday('now') end)
        and exists (select 1 from "user" u where u."id" = ? and u."emailVerified" = 1 and lower(u."email") = lower("invitation"."email"))
        and exists (select 1 from "organization" o where o."id" = "invitation"."organizationId")
        and (select count(*) from "member" m where m."organizationId" = "invitation"."organizationId") < ${MEMBERSHIP_LIMIT}`, params: [invitationId, session.user.id] },
      { sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select ?, ?, ?, ?, ? where changes() = 1`, params: [member.id, member.organizationId, member.userId, member.role, member.createdAt] },
    ])
  } catch (error) {
    // Only the workspace the stored invitation names: never one the request body or query mentions.
    if (limitRaised(error) === "members") return planLimit(await admitMember(context.db as unknown as BillingDb, context.billing, context.baseURL, invitation.organizationId, session.user.id), "This workspace's member limit is reached.")
    if (/UNIQUE constraint failed: member\./.test(error instanceof Error ? error.message : "")) return failure(400, "INVITATION_NOT_FOUND", INVITATION_GONE)
    throw error
  }
  if (await first(context.db, `select 1 as "joined" from "member" where "id" = ?`, member.id) === null) {
    // Refused at the write: say which check failed, read again now.
    const now = await first<{ n: number }>(context.db, `select count(*) as "n" from "member" where "organizationId" = ?`, invitation.organizationId)
    if ((now?.n ?? 0) >= MEMBERSHIP_LIMIT) return failure(403, "ORGANIZATION_MEMBERSHIP_LIMIT_REACHED", "Organization membership limit reached")
    return failure(400, "INVITATION_NOT_FOUND", INVITATION_GONE)
  }
  await activate(context.auth, session.session.token, member.organizationId)
  return json({ invitation: { ...invitation, status: "accepted" }, member })
}
