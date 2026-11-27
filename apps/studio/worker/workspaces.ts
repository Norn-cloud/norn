import {
  activeMembership,
  atomicallyWithResults,
  commitAcceptInvitation,
  commitActivateSession,
  commitRevokeApiKey,
  dbAll,
  dbFirst,
  isProvisioningMetadata,
  isSessionLive,
  listWorkspaceApiKeys,
  LIVE_SESSION_S_SQL,
  publicMetadata,
  BILLING_PAYER,
  BILLED,
  READY_ORG_SQL,
  WORKSPACE_ACCESS_DENIED_MESSAGE,
  type Auth,
  type ProvisionDb,
  type WorkspaceApiKeySummary,
} from "./auth.ts"
import { admitCell, entitlementOf, limitRaised, refusalMessage, type Admission, type BillingDb, type BillingSettings } from "./billing.ts"
import { environmentOf } from "./scope.ts"

/**
 * Studio's workspace routes: listing the signed-in user's workspaces for the honest switcher,
 * switching the session's active workspace, and atomic workspace creation / invitation acceptance
 * for billing enforcement (migration 0011).
 *
 * Self-serve workspace creation with host cell provisioning, archiving, deletion, and workspace
 * requests live in the follow-up lifecycle work (tracked on #1414). On deployments connected to a
 * Norn host (`NORN_HOST`), `POST /api/workspaces` refuses rather than creating an unprovisioned org.
 */

export interface WorkspaceContext {
  readonly auth: Auth
  readonly db: D1Database
  readonly billing: BillingSettings | undefined
  readonly baseURL: string
  readonly host?: Fetcher
}

export interface WorkspaceItem {
  readonly id: string
  readonly name: string
  readonly slug: string
  readonly role: string
  readonly active: boolean
  readonly hasCell: boolean
}

export interface WorkspacesDocument {
  readonly workspaces: readonly WorkspaceItem[]
  readonly planLabel: string | null
}

function withCookies(response: Response, source: Headers | undefined): Response {
  const cookies = source?.getSetCookie() ?? []
  if (cookies.length === 0) return response
  const headers = new Headers(response.headers)
  for (const cookie of cookies) headers.append("set-cookie", cookie)
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers })
}

const json = (body: unknown, status = 200, sessionHeaders?: Headers) =>
  withCookies(Response.json(body, { status, headers: { "cache-control": "no-store" } }), sessionHeaders)
/** Better Auth's error shape, so a client reads these as it reads Better Auth's. */
const failure = (status: number, code: string, message: string, sessionHeaders?: Headers) =>
  json({ code, message }, status, sessionHeaders)
/** A plan limit, explained from what the server authorized: the caller's own cells, or the invitation's workspace. */
function planLimit(admission: Admission | undefined, fallback: string, sessionHeaders?: Headers): Response {
  const refusal = admission?.kind === "plan-limit" ? admission : undefined
  return json({ code: "PLAN_LIMIT", message: refusal === undefined ? fallback : refusalMessage(refusal), ...refusal === undefined ? {} : { refusal } }, 403, sessionHeaders)
}

function formatPlanLabel(plan: string): string {
  if (plan === "free") return "Free plan"
  return `${plan[0]?.toUpperCase() ?? ""}${plan.slice(1)} plan`
}

async function body(request: Request): Promise<Record<string, unknown> | null> {
  const value = await request.json().catch(() => null)
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null
}

const provisionDb = (db: D1Database) => db as unknown as ProvisionDb

/** GET /api/workspaces: the signed-in user's ready workspaces and current plan label. */
export async function listWorkspaces(request: Request, context: WorkspaceContext): Promise<Response> {
  const { headers: sessionHeaders, response: session } = await context.auth.api.getSession({ headers: request.headers, returnHeaders: true })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
  const active = await activeMembership(context.auth, session.user.id, session.session.activeOrganizationId)
  type OrgRow = { id: string; name: string; slug: string; metadata: string | null; role: string }
  const rows = await dbAll<OrgRow>(provisionDb(context.db),
    `select o."id", o."name", o."slug", o."metadata", m."role"
     from "member" m
     join "organization" o on o."id" = m."organizationId"
     where m."userId" = ?
     order by m."createdAt" asc, o."id" asc`,
    session.user.id)

  const workspaces: WorkspaceItem[] = []
  for (const row of rows) {
    if (isProvisioningMetadata(row.metadata)) continue
    workspaces.push({
      id: row.id,
      name: row.name,
      slug: row.slug,
      role: row.role,
      active: active?.organizationId === row.id,
      hasCell: environmentOf(row.metadata) !== null,
    })
  }

  let planLabel: string | null = null
  if (context.billing !== undefined) {
    const entitlement = await entitlementOf(context.db as unknown as BillingDb, context.billing, session.user.id)
    planLabel = formatPlanLabel(entitlement.plan)
  }

  const document: WorkspacesDocument = { workspaces, planLabel }
  return json(document, 200, sessionHeaders)
}

/** POST /api/workspaces/:id/activate: switch the session's active workspace, enforcing membership and readiness at commit. */
export async function activateWorkspace(request: Request, context: WorkspaceContext, organizationId: string): Promise<Response> {
  const { headers: sessionHeaders, response: session } = await context.auth.api.getSession({ headers: request.headers, returnHeaders: true })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
  const db = provisionDb(context.db)
  const membership = await dbFirst<{ role: string; name: string; metadata: string | null }>(db,
    `select m."role", o."name", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`,
    organizationId, session.user.id)
  if (membership === null) return failure(403, "WORKSPACE_ACCESS_DENIED", WORKSPACE_ACCESS_DENIED_MESSAGE, sessionHeaders)
  if (isProvisioningMetadata(membership.metadata)) return failure(409, "WORKSPACE_PROVISIONING", "This workspace is still being provisioned.", sessionHeaders)
  const updated = await commitActivateSession(db, { token: session.session.token, userId: session.user.id, organizationId })
  if (updated === null) {
    if (!(await isSessionLive(db, session.session.token, session.user.id))) {
      return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
    }
    const current = await dbFirst<{ metadata: string | null }>(db,
      `select o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`,
      organizationId, session.user.id)
    if (current !== null && isProvisioningMetadata(current.metadata)) return failure(409, "WORKSPACE_PROVISIONING", "This workspace is still being provisioned.", sessionHeaders)
    return failure(403, "WORKSPACE_ACCESS_DENIED", WORKSPACE_ACCESS_DENIED_MESSAGE, sessionHeaders)
  }
  return json({ activeOrganizationId: updated.activeOrganizationId, name: membership.name, role: membership.role, hasCell: environmentOf(membership.metadata) !== null }, 200, sessionHeaders)
}

/**
 * POST /api/workspaces { name, slug, logo?, metadata?, keepCurrentActiveOrganization? }:
 * On deployments connected to a Norn host, self-serve workspace creation is disabled until host-side
 * cell lifecycle provisioning lands (#1414). On non-host stages (billing enforcement), creates the
 * workspace, its owner, and (unless opted out) its session activation in one transaction so a
 * plan-limit refusal rolls all writes back together.
 */
export async function createWorkspace(request: Request, context: WorkspaceContext): Promise<Response> {
  const { headers: sessionHeaders, response: session } = await context.auth.api.getSession({ headers: request.headers, returnHeaders: true })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
  if (context.host !== undefined) {
    return failure(403, "WORKSPACE_CREATION_UNAVAILABLE", "Self-serve workspace creation is not enabled on this deployment.", sessionHeaders)
  }
  const db = provisionDb(context.db)
  const input = await body(request)
  const name = typeof input?.name === "string" ? input.name.trim() : ""
  const slug = typeof input?.slug === "string" ? input.slug.trim() : ""
  const logo = typeof input?.logo === "string" ? input.logo : null
  if (name === "" || slug === "") return failure(400, "VALIDATION_ERROR", "A workspace needs a name and a slug.", sessionHeaders)
  if (await dbFirst(db, `select 1 as "taken" from "organization" where "slug" = ?`, slug) !== null) return failure(400, "ORGANIZATION_ALREADY_EXISTS", "Organization already exists", sessionHeaders)
  const id = crypto.randomUUID()
  const at = new Date().toISOString()
  // Server-only keys: the payer, read by migration 0011's triggers, only while billing is on.
  const metadata = { ...publicMetadata(input?.metadata), ...context.billing === undefined ? {} : { [BILLING_PAYER]: session.user.id, [BILLED]: true } }
  const member = { id: crypto.randomUUID(), organizationId: id, userId: session.user.id, role: "owner", createdAt: at }
  const keepCurrent = input?.keepCurrentActiveOrganization === true
  let batchResults: readonly (readonly Record<string, unknown>[])[]
  try {
    // Guard the initial organization insert on a live, unexpired caller session and chain the
    // member insert + session activation via changes() = 1 with RETURNING so no second clock
    // evaluation or post-batch read can diverge from the committed transaction.
    batchResults = await atomicallyWithResults(db, [
      {
        sql: `insert into "organization" ("id", "name", "slug", "logo", "metadata", "createdAt")
          select ?, ?, ?, ?, ?, ?
          where exists (select 1 from "session" s where s."token" = ? and s."userId" = ? and ${LIVE_SESSION_S_SQL})
          returning "id"`,
        params: [id, name, slug, logo, JSON.stringify(metadata), at, session.session.token, session.user.id],
      },
      {
        sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt")
          select ?, ?, ?, ?, ? where changes() = 1 returning "id"`,
        params: [member.id, id, member.userId, member.role, at],
      },
      ...keepCurrent
        ? []
        : [{
          sql: `update "session" set "activeOrganizationId" = ?, "updatedAt" = ?
            where "token" = ? and "userId" = ? and changes() = 1
            returning "activeOrganizationId", "updatedAt"`,
          params: [id, at, session.session.token, session.user.id],
        }],
    ])
  } catch (error) {
    if (limitRaised(error) === "cells") return planLimit(await admitCell(context.db as unknown as BillingDb, context.billing, context.baseURL, session.user.id), "Your plan's workspace limit is reached.", sessionHeaders)
    if (/UNIQUE constraint failed: organization\.slug/.test(error instanceof Error ? error.message : "")) return failure(400, "ORGANIZATION_ALREADY_EXISTS", "Organization already exists", sessionHeaders)
    throw error
  }
  const createdOrg = batchResults[0]?.[0]
  const createdMember = batchResults[1]?.[0]
  const activatedSession = keepCurrent ? { activeOrganizationId: session.session.activeOrganizationId } : batchResults[2]?.[0]
  if (createdOrg === undefined || createdMember === undefined || activatedSession === undefined) {
    return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
  }
  return json({ id, name, slug, logo, metadata: publicMetadata(metadata), createdAt: at, members: [member] }, 200, sessionHeaders)
}

/**
 * POST /api/workspaces/invitations/accept { invitationId }: the invitation turns accepted, the
 * member joins, and the session activates the workspace in one serialized transaction with
 * commit-time readiness and membership predicates.
 */
export async function acceptWorkspaceInvitation(request: Request, context: WorkspaceContext): Promise<Response> {
  const { headers: sessionHeaders, response: session } = await context.auth.api.getSession({ headers: request.headers, returnHeaders: true })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
  const input = await body(request)
  if (typeof input?.invitationId !== "string") return failure(400, "VALIDATION_ERROR", "invitationId is required.", sessionHeaders)
  const outcome = await commitAcceptInvitation(provisionDb(context.db), {
    invitationId: input.invitationId,
    userId: session.user.id,
    userEmail: session.user.email,
    emailVerified: session.user.emailVerified,
    sessionToken: session.session.token,
    billing: context.billing,
    baseURL: context.baseURL,
  })
  if (outcome.kind === "refused") {
    return json(
      { code: outcome.code, message: outcome.message, ...outcome.refusal === undefined ? {} : { refusal: outcome.refusal } },
      outcome.status,
      sessionHeaders,
    )
  }
  return json({ invitation: outcome.invitation, member: outcome.member }, 200, sessionHeaders)
}

export interface WorkspaceApiKeysDocument {
  readonly apiKeys: readonly WorkspaceApiKeySummary[]
}

/** GET /api/workspaces/:orgId/api-keys: read-only workspace-scoped API key list (#1859). */
export async function listWorkspaceApiKeysRoute(
  request: Request,
  context: WorkspaceContext,
  organizationId: string,
): Promise<Response> {
  // Read-only GET: disable session refresh so listing API keys performs zero DB/cookie mutations.
  const session = await context.auth.api.getSession({
    headers: request.headers,
    query: { disableCookieCache: true, disableRefresh: true },
  })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const expectedOrg = request.headers.get("x-norn-expected-org")?.trim() ?? ""
  if (expectedOrg !== "" && expectedOrg !== organizationId) {
    return failure(409, "WORKSPACE_CHANGED", "Your active workspace changed. Reload the page before submitting.")
  }
  const outcome = await listWorkspaceApiKeys(provisionDb(context.db), {
    userId: session.user.id,
    organizationId,
    sessionToken: session.session.token,
  })
  if (outcome.kind === "refused") {
    return failure(outcome.status, outcome.code, outcome.message)
  }
  const document: WorkspaceApiKeysDocument = { apiKeys: outcome.apiKeys }
  return json(document, 200)
}

/** DELETE /api/workspaces/:orgId/api-keys/:keyId: atomic workspace-scoped API key revocation (#1859). */
export async function revokeWorkspaceApiKeyRoute(
  request: Request,
  context: WorkspaceContext,
  organizationId: string,
  keyId: string,
): Promise<Response> {
  const { headers: sessionHeaders, response: session } = await context.auth.api.getSession({ headers: request.headers, returnHeaders: true })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.", sessionHeaders)
  const expectedOrg = request.headers.get("x-norn-expected-org")?.trim() ?? ""
  if (expectedOrg !== "" && expectedOrg !== organizationId) {
    return failure(409, "WORKSPACE_CHANGED", "Your active workspace changed. Reload the page before submitting.", sessionHeaders)
  }
  const outcome = await commitRevokeApiKey(provisionDb(context.db), {
    userId: session.user.id,
    organizationId,
    keyId,
    sessionToken: session.session.token,
  })
  if (outcome.kind === "refused") {
    return failure(outcome.status, outcome.code, outcome.message, sessionHeaders)
  }
  return json({ revoked: true, id: outcome.id }, 200, sessionHeaders)
}
