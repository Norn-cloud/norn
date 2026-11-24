import { hostToken, type NornActor } from "./actor.ts"
import { activeMembership, atomically, isOrganizationArchived, isProvisioningMetadata, publicMetadata, BILLING_PAYER, BILLED, PROVISIONING, type Auth, type ProvisionDb } from "./auth.ts"
import { admitCell, admitMember, entitlementOf, limitRaised, refusalMessage, type Admission, type BillingDb, type BillingSettings } from "./billing.ts"

/**
 * Governed workspace lifecycle: listing, switching, creating (with isolated cell provisioning on
 * NORN_HOST when configured), renaming, archiving/restoring (undo), permanent deletion, workspace
 * requests (when self-serve cell provisioning is unavailable), and atomic invitation acceptance.
 *
 * Every authorization check runs on the server against D1's membership rows and Better Auth's
 * verified session; the client receives precomputed capability flags for display only and never
 * decides permissions itself.
 *
 * Creating a workspace and accepting an invitation each run as one database transaction so an
 * interruption or a plan-limit refusal (migration 0011) never leaves half a change. When NORN_HOST
 * and STUDIO_DEFAULT_ENVIRONMENT_ID are configured, creating a workspace also provisions an empty
 * isolated cell via `POST /v1/cell:provision` (HOST-PROVISION-1) and rolls the workspace back if
 * the host refuses.
 */

export interface WorkspaceContext {
  readonly auth: Auth
  readonly db: D1Database
  readonly billing: BillingSettings | undefined
  readonly baseURL: string
  readonly host?: Fetcher
  readonly defaultEnvironmentId?: string
  readonly defaultOrganizationId?: string
}

export interface WorkspaceItem {
  readonly id: string
  readonly name: string
  readonly slug: string
  readonly role: string
  readonly active: boolean
  readonly hasCell: boolean
  readonly archived: boolean
  readonly archivedAt: string | null
  readonly canRename: boolean
  readonly canArchive: boolean
  readonly canDelete: boolean
}

export interface WorkspaceRequestItem {
  readonly id: string
  readonly name: string
  readonly note: string
  readonly status: "pending" | "approved" | "dismissed"
  readonly requesterName: string
  readonly requesterEmail: string
  readonly createdAt: string
  readonly canManage: boolean
  readonly canWithdraw: boolean
}

export type WorkspaceCreation =
  | { readonly mode: "self-serve"; readonly allowed: true }
  | { readonly mode: "self-serve"; readonly allowed: false; readonly reason: string; readonly upgradeUrl: string | null }
  | { readonly mode: "request" }

export interface WorkspacesDocument {
  readonly workspaces: readonly WorkspaceItem[]
  readonly archived: readonly WorkspaceItem[]
  readonly creation: WorkspaceCreation
  readonly requests: readonly WorkspaceRequestItem[]
  readonly planLabel: string | null
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
const MAX_NAME_LENGTH = 80
const MAX_NOTE_LENGTH = 500
const MAX_PENDING_REQUESTS = 5
const ENVIRONMENT = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/
const USER_METHODS = new Set(["password", "magic_link"])

/** Cached trusted-base SHA-256 per NORN_HOST binding so provisioning usually succeeds in one round-trip. */
const baseDigests = new WeakMap<Fetcher, string>()

const hasRole = (role: string | null | undefined, allowed: readonly string[]): boolean =>
  typeof role === "string" && role.split(",").map((part) => part.trim()).some((part) => allowed.includes(part))
const isOwner = (role: string | null | undefined) => hasRole(role, ["owner"])
const isAdmin = (role: string | null | undefined) => hasRole(role, ["owner", "admin"])

function slugFromName(name: string): string {
  const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "workspace"
  return `${base}-${crypto.randomUUID().slice(0, 8)}`
}

function environmentOf(metadata: unknown): string | null {
  let value = metadata
  if (typeof value === "string") {
    try { value = JSON.parse(value) } catch { return null }
  }
  const envId = (value as { environmentId?: unknown } | null)?.environmentId
  return typeof envId === "string" && ENVIRONMENT.test(envId) ? envId : null
}

function formatPlanLabel(plan: string): string {
  if (plan === "free") return "Free plan"
  return `${plan[0]?.toUpperCase() ?? ""}${plan.slice(1)} plan`
}

async function body(request: Request): Promise<Record<string, unknown> | null> {
  const value = await request.json().catch(() => null)
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null
}

type SqlResult = { first<T>(): Promise<T | null>; all<T>(): Promise<{ results: T[] }>; run(): Promise<unknown> }
type QueryableStatement = { bind?: (...params: (string | null)[]) => SqlResult; get?: (...params: (string | null)[]) => unknown; all?: (...params: (string | null)[]) => unknown[]; run?: (...params: (string | null)[]) => unknown }
type QueryableDb = { prepare(sql: string): QueryableStatement }

async function first<T>(db: D1Database | ProvisionDb, sql: string, ...params: (string | null)[]): Promise<T | null> {
  const statement = (db as unknown as QueryableDb).prepare(sql)
  if (statement.bind) return statement.bind(...params).first<T>()
  return (statement.get?.(...params) as T | undefined) ?? null
}

async function all<T>(db: D1Database | ProvisionDb, sql: string, ...params: (string | null)[]): Promise<T[]> {
  const statement = (db as unknown as QueryableDb).prepare(sql)
  if (statement.bind) return (await statement.bind(...params).all<T>()).results
  return (statement.all?.(...params) ?? []) as T[]
}

async function run(db: D1Database | ProvisionDb, sql: string, ...params: (string | null)[]): Promise<void> {
  const statement = (db as unknown as QueryableDb).prepare(sql)
  if (statement.bind) await statement.bind(...params).run()
  else statement.run?.(...params)
}

const missingTable = (error: unknown, table: string) =>
  String(error instanceof Error ? error.message : error).includes(`no such table: ${table}`)

/** Make the workspace the session's active one, as Better Auth's routes do. */
async function activate(auth: Auth, token: string, organizationId: string | null): Promise<void> {
  const context = await auth.$context
  await context.internalAdapter.updateSession(token, { activeOrganizationId: organizationId })
}

/** Find the user's earliest non-archived, ready membership (excluding `excludeId` if given). */
async function nextActiveWorkspace(db: D1Database, userId: string, excludeId?: string): Promise<string | null> {
  type Candidate = { organizationId: string; metadata: string | null }
  let rows: Candidate[]
  try {
    rows = excludeId === undefined
      ? await all<Candidate>(db,
          `select m."organizationId", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" left join "workspace_archive" a on a."organization_id" = m."organizationId" where m."userId" = ? and a."organization_id" is null order by m."createdAt" asc, m."id" asc`,
          userId)
      : await all<Candidate>(db,
          `select m."organizationId", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" left join "workspace_archive" a on a."organization_id" = m."organizationId" where m."userId" = ? and m."organizationId" <> ? and a."organization_id" is null order by m."createdAt" asc, m."id" asc`,
          userId, excludeId)
  } catch (error) {
    if (!missingTable(error, "workspace_archive")) throw error
    rows = excludeId === undefined
      ? await all<Candidate>(db, `select m."organizationId", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."userId" = ? order by m."createdAt" asc, m."id" asc`, userId)
      : await all<Candidate>(db, `select m."organizationId", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."userId" = ? and m."organizationId" <> ? order by m."createdAt" asc, m."id" asc`, userId, excludeId)
  }
  return rows.find((row) => !isProvisioningMetadata(row.metadata))?.organizationId ?? null
}

/**
 * Provision an empty tenant cell on NORN_HOST (HOST-PROVISION-1: `POST /v1/cell:provision` with
 * `artifacts: []`, prepended by the host's own trusted base). Negotiates the host's trusted-base
 * SHA-256 once on `cell.base_mismatch` and caches it per host binding.
 */
export async function provisionEmptyCell(
  host: Fetcher,
  auth: Auth,
  actor: NornActor,
  orgId: string,
  environmentId: string,
): Promise<{ readonly ok: true } | { readonly ok: false; readonly status: number; readonly code: string }> {
  let token: string
  try {
    token = await hostToken(auth, actor)
  } catch {
    return { ok: false, status: 503, code: "host.token_unavailable" }
  }
  const query = new URLSearchParams({ orgId, environmentId }).toString()
  const send = async (baseSha256: string) => host.fetch(new Request(`https://norn-host/v1/cell:provision?${query}`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify({ baseSha256, artifacts: [] }),
  }))
  const attempt = async (digest: string, allowRetry: boolean): Promise<{ readonly ok: true } | { readonly ok: false; readonly status: number; readonly code: string }> => {
    let response: Response
    try {
      response = await send(digest)
    } catch {
      return { ok: false, status: 503, code: "host.unreachable" }
    }
    const payload = (await response.json().catch(() => null)) as { code?: unknown; explanation?: unknown; baseSha256?: unknown } | null
    if (response.ok) {
      if (typeof payload?.baseSha256 === "string" && /^[0-9a-f]{64}$/.test(payload.baseSha256)) baseDigests.set(host, payload.baseSha256)
      else baseDigests.set(host, digest)
      return { ok: true }
    }
    if (allowRetry && response.status === 409 && payload?.code === "cell.base_mismatch" && typeof payload.explanation === "string") {
      const matched = /sha256 ([0-9a-f]{64})/.exec(payload.explanation)
      if (matched?.[1] !== undefined) {
        baseDigests.set(host, matched[1])
        return attempt(matched[1], false)
      }
    }
    return { ok: false, status: response.status, code: typeof payload?.code === "string" ? payload.code : "host.refused" }
  }
  return attempt(baseDigests.get(host) ?? "0".repeat(64), true)
}

/** GET /api/workspaces: the signed-in user's workspaces, capabilities, creation mode and requests. */
export async function listWorkspaces(request: Request, context: WorkspaceContext): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const active = await activeMembership(context.auth, session.user.id, session.session.activeOrganizationId)
  type OrgRow = { id: string; name: string; slug: string; metadata: string | null; role: string; archived_at: string | null }
  let rows: OrgRow[]
  try {
    rows = await all<OrgRow>(context.db,
      `select o."id", o."name", o."slug", o."metadata", m."role", a."archived_at"
       from "member" m
       join "organization" o on o."id" = m."organizationId"
       left join "workspace_archive" a on a."organization_id" = o."id"
       where m."userId" = ?
       order by m."createdAt" asc, o."id" asc`,
      session.user.id)
  } catch (error) {
    if (!missingTable(error, "workspace_archive")) throw error
    rows = (await all<Omit<OrgRow, "archived_at">>(context.db,
      `select o."id", o."name", o."slug", o."metadata", m."role"
       from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."userId" = ?
       order by m."createdAt" asc, o."id" asc`,
      session.user.id)).map((row) => ({ ...row, archived_at: null }))
  }

  const workspaces: WorkspaceItem[] = []
  const archived: WorkspaceItem[] = []
  for (const row of rows) {
    if (isProvisioningMetadata(row.metadata)) continue
    const isDefault = context.defaultOrganizationId !== undefined && row.id === context.defaultOrganizationId
    const isArchived = row.archived_at !== null
    const owner = isOwner(row.role)
    const item: WorkspaceItem = {
      id: row.id,
      name: row.name,
      slug: row.slug,
      role: row.role,
      active: !isArchived && active?.organizationId === row.id,
      hasCell: environmentOf(row.metadata) !== null,
      archived: isArchived,
      archivedAt: row.archived_at,
      canRename: !isArchived && isAdmin(row.role),
      canArchive: !isArchived && owner && !isDefault,
      canDelete: owner && !isDefault,
    }
    if (isArchived) {
      // Only owners can restore or permanently delete an archived workspace.
      if (owner) archived.push(item)
    } else {
      workspaces.push(item)
    }
  }

  const billingDb = context.db as unknown as BillingDb
  let creation: WorkspaceCreation
  if (context.host !== undefined && context.defaultEnvironmentId !== undefined) {
    const admission = await admitCell(billingDb, context.billing, context.baseURL, session.user.id)
    creation = admission.kind === "admitted"
      ? { mode: "self-serve", allowed: true }
      : { mode: "self-serve", allowed: false, reason: refusalMessage(admission), upgradeUrl: admission.upgradeUrl }
  } else {
    creation = { mode: "request" }
  }

  let planLabel: string | null = null
  if (context.billing !== undefined) {
    const entitlement = await entitlementOf(billingDb, context.billing, session.user.id)
    planLabel = formatPlanLabel(entitlement.plan)
  }

  const activeIsAdmin = active !== null && isAdmin(active.role)
  type RequestRow = { id: string; user_id: string; organization_id: string | null; name: string; note: string; status: "pending" | "approved" | "dismissed"; created_at: string; requester_name: string; requester_email: string }
  let requestRows: RequestRow[] = []
  try {
    requestRows = activeIsAdmin
      ? await all<RequestRow>(context.db,
          `select r."id", r."user_id", r."organization_id", r."name", r."note", r."status", r."created_at", u."name" as "requester_name", u."email" as "requester_email"
           from "workspace_request" r
           join "user" u on u."id" = r."user_id"
           where r."user_id" = ? or r."organization_id" = ?
           order by case when r."status" = 'pending' then 0 else 1 end asc, r."created_at" desc limit 50`,
          session.user.id, active.organizationId)
      : await all<RequestRow>(context.db,
          `select r."id", r."user_id", r."organization_id", r."name", r."note", r."status", r."created_at", u."name" as "requester_name", u."email" as "requester_email"
           from "workspace_request" r
           join "user" u on u."id" = r."user_id"
           where r."user_id" = ?
           order by case when r."status" = 'pending' then 0 else 1 end asc, r."created_at" desc limit 50`,
          session.user.id)
  } catch (error) {
    if (!missingTable(error, "workspace_request")) throw error
  }

  const requests: WorkspaceRequestItem[] = requestRows.map((row) => ({
    id: row.id,
    name: row.name,
    note: row.note,
    status: row.status,
    requesterName: row.requester_name,
    requesterEmail: row.requester_email,
    createdAt: row.created_at,
    canManage: row.status === "pending" && activeIsAdmin && row.organization_id === active.organizationId,
    canWithdraw: row.status === "pending" && row.user_id === session.user.id,
  }))

  const document: WorkspacesDocument = { workspaces, archived, creation, requests, planLabel }
  return json(document)
}

/** POST /api/workspaces { name, slug?, logo?, metadata?, keepCurrentActiveOrganization? }: workspace + owner (+ cell when NORN_HOST is configured). */
export async function createWorkspace(request: Request, context: WorkspaceContext): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  if (context.host !== undefined && context.defaultEnvironmentId === undefined) {
    return failure(403, "SELF_SERVE_UNAVAILABLE", "This deployment requires an admin to provision new workspaces. Submit a workspace request instead.")
  }
  const method = (session.session as { authMethod?: string | null }).authMethod
  if (context.host !== undefined && (method == null || !USER_METHODS.has(method))) {
    return failure(403, "FORBIDDEN", "Sign in with a user session to create a workspace.")
  }
  const input = await body(request)
  const name = typeof input?.name === "string" ? input.name.trim() : ""
  const explicitSlug = input?.slug
  if (name === "" || name.length > MAX_NAME_LENGTH) return failure(400, "VALIDATION_ERROR", `A workspace name must be between 1 and ${MAX_NAME_LENGTH} characters.`)
  if (explicitSlug !== undefined && (typeof explicitSlug !== "string" || explicitSlug.trim() === "")) {
    return failure(400, "VALIDATION_ERROR", "A workspace needs a name and a slug.")
  }
  const slug = typeof explicitSlug === "string" ? explicitSlug.trim() : slugFromName(name)
  const logo = typeof input?.logo === "string" ? input.logo : null
  if (await first(context.db, `select 1 as "taken" from "organization" where "slug" = ?`, slug) !== null) return failure(400, "ORGANIZATION_ALREADY_EXISTS", "Organization already exists")
  const id = crypto.randomUUID()
  const at = new Date().toISOString()
  // Server-only keys: environmentId (from deployment config, never client input) and billing payer (when billing is on).
  // When NORN_HOST is connected, start in an explicit provisioning state with no environmentId so concurrent reads
  // cannot list or activate the workspace before cell provisioning succeeds.
  const environmentId = context.defaultEnvironmentId
  const needsHostProvision = context.host !== undefined && environmentId !== undefined
  const baseMetadata = {
    ...publicMetadata(input?.metadata),
    ...context.billing === undefined ? {} : { [BILLING_PAYER]: session.user.id, [BILLED]: true },
  }
  const initialMetadata = needsHostProvision
    ? { ...baseMetadata, [PROVISIONING]: true }
    : { ...baseMetadata, ...environmentId === undefined ? {} : { environmentId } }
  const readyMetadata = { ...baseMetadata, ...environmentId === undefined ? {} : { environmentId } }
  const member = { id: crypto.randomUUID(), organizationId: id, userId: session.user.id, role: "owner", createdAt: at }
  try {
    await atomically(context.db as unknown as ProvisionDb, [
      { sql: `insert into "organization" ("id", "name", "slug", "logo", "metadata", "createdAt") values (?, ?, ?, ?, ?, ?)`, params: [id, name, slug, logo, JSON.stringify(initialMetadata), at] },
      { sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values (?, ?, ?, ?, ?)`, params: [member.id, id, member.userId, member.role, at] },
    ])
  } catch (error) {
    if (limitRaised(error) === "cells") return planLimit(await admitCell(context.db as unknown as BillingDb, context.billing, context.baseURL, session.user.id), "Your plan's workspace limit is reached.")
    if (/UNIQUE constraint failed: organization\.slug/.test(error instanceof Error ? error.message : "")) return failure(400, "ORGANIZATION_ALREADY_EXISTS", "Organization already exists")
    throw error
  }
  if (needsHostProvision) {
    const rollback = async () => {
      // Never leave an unprovisioned org behind or burn the user's billed cell quota.
      await atomically(context.db as unknown as ProvisionDb, [
        { sql: `delete from "member" where "organizationId" = ?`, params: [id] },
        { sql: `delete from "organization" where "id" = ?`, params: [id] },
      ])
    }
    const actor: NornActor = { actorKind: "user", subjectId: session.user.id, orgId: id, role: "owner", authMethod: method as NornActor["authMethod"] }
    let provisioned: { readonly ok: true } | { readonly ok: false; readonly status: number; readonly code: string }
    try {
      provisioned = await provisionEmptyCell(context.host, context.auth, actor, id, environmentId)
      if (provisioned.ok) {
        await run(context.db, `update "organization" set "metadata" = ? where "id" = ?`, JSON.stringify(readyMetadata), id)
      }
    } catch {
      await rollback()
      return failure(502, "CELL_PROVISION_FAILED", "Could not provision a cell for this workspace right now. Try again shortly.")
    }
    if (!provisioned.ok) {
      await rollback()
      return failure(502, "CELL_PROVISION_FAILED", "Could not provision a cell for this workspace right now. Try again shortly.")
    }
  }
  if (input?.keepCurrentActiveOrganization !== true) await activate(context.auth, session.session.token, id)
  return json({ id, name, slug, logo, metadata: publicMetadata(readyMetadata), createdAt: at, members: [member] })
}

/** POST /api/workspaces/:id/activate: switch the session's active workspace (members only, non-archived, ready only). */
export async function activateWorkspace(request: Request, context: WorkspaceContext, organizationId: string): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const membership = await first<{ role: string; name: string; metadata: string | null }>(context.db,
    `select m."role", o."name", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`,
    organizationId, session.user.id)
  if (membership === null) return failure(403, "FORBIDDEN", "You are not a member of this workspace.")
  if (isProvisioningMetadata(membership.metadata)) return failure(409, "WORKSPACE_PROVISIONING", "This workspace is still being provisioned.")
  try {
    const archived = await first(context.db, `select 1 as "found" from "workspace_archive" where "organization_id" = ?`, organizationId)
    if (archived !== null) return failure(409, "WORKSPACE_ARCHIVED", "Restore this workspace before opening it.")
  } catch (error) {
    if (!missingTable(error, "workspace_archive")) throw error
  }
  await activate(context.auth, session.session.token, organizationId)
  return json({ activeOrganizationId: organizationId, name: membership.name, role: membership.role, hasCell: environmentOf(membership.metadata) !== null })
}

/** PATCH /api/workspaces/:id { name }: rename a workspace (owner or admin only; non-archived, ready only). */
export async function renameWorkspace(request: Request, context: WorkspaceContext, organizationId: string): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const membership = await first<{ role: string; metadata: string | null }>(context.db,
    `select m."role", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`, organizationId, session.user.id)
  if (membership === null || !isAdmin(membership.role)) {
    return failure(403, "FORBIDDEN", "Only workspace owners and admins can rename a workspace.")
  }
  if (isProvisioningMetadata(membership.metadata)) return failure(409, "WORKSPACE_PROVISIONING", "This workspace is still being provisioned.")
  try {
    const archived = await first(context.db, `select 1 as "found" from "workspace_archive" where "organization_id" = ?`, organizationId)
    if (archived !== null) return failure(409, "WORKSPACE_ARCHIVED", "Restore this workspace before renaming it.")
  } catch (error) {
    if (!missingTable(error, "workspace_archive")) throw error
  }
  const input = await body(request)
  const name = typeof input?.name === "string" ? input.name.trim() : ""
  if (name === "" || name.length > MAX_NAME_LENGTH) {
    return failure(400, "VALIDATION_ERROR", `A workspace name must be between 1 and ${MAX_NAME_LENGTH} characters.`)
  }
  await run(context.db, `update "organization" set "name" = ? where "id" = ?`, name, organizationId)
  return json({ id: organizationId, name })
}

/** POST /api/workspaces/:id/archive: reversibly archive a workspace (owner only). Keeps billing_workspace intact. */
export async function archiveWorkspace(request: Request, context: WorkspaceContext, organizationId: string): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const membership = await first<{ role: string; metadata: string | null }>(context.db,
    `select m."role", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`, organizationId, session.user.id)
  if (membership === null || !isOwner(membership.role)) {
    return failure(403, "FORBIDDEN", "Only a workspace owner can archive a workspace.")
  }
  if (isProvisioningMetadata(membership.metadata)) return failure(409, "WORKSPACE_PROVISIONING", "This workspace is still being provisioned.")
  if (context.defaultOrganizationId !== undefined && organizationId === context.defaultOrganizationId) {
    return failure(403, "FORBIDDEN", "The deployment's shared default workspace cannot be archived.")
  }
  const existing = await first(context.db, `select 1 as "found" from "workspace_archive" where "organization_id" = ?`, organizationId)
  if (existing !== null) return failure(409, "ALREADY_ARCHIVED", "This workspace is already archived.")
  const at = new Date().toISOString()
  try {
    await atomically(context.db as unknown as ProvisionDb, [
      { sql: `insert into "workspace_archive" ("organization_id", "archived_at", "archived_by") values (?, ?, ?)`, params: [organizationId, at, session.user.id] },
      { sql: `update "session" set "activeOrganizationId" = null where "activeOrganizationId" = ?`, params: [organizationId] },
    ])
  } catch (error) {
    if (/UNIQUE constraint failed: workspace_archive\.organization_id/.test(error instanceof Error ? error.message : "")) {
      return failure(409, "ALREADY_ARCHIVED", "This workspace is already archived.")
    }
    throw error
  }
  const nextId = await nextActiveWorkspace(context.db, session.user.id, organizationId)
  if (session.session.activeOrganizationId === organizationId || session.session.activeOrganizationId == null) {
    await activate(context.auth, session.session.token, nextId)
  }
  return json({ id: organizationId, archived: true, archivedAt: at, nextActiveOrganizationId: nextId })
}

/** POST /api/workspaces/:id/restore { activate? }: undo archive (owner only). */
export async function restoreWorkspace(request: Request, context: WorkspaceContext, organizationId: string): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const membership = await first<{ role: string }>(context.db,
    `select "role" from "member" where "organizationId" = ? and "userId" = ?`, organizationId, session.user.id)
  if (membership === null || !isOwner(membership.role)) {
    return failure(403, "FORBIDDEN", "Only a workspace owner can restore a workspace.")
  }
  const existing = await first(context.db, `select 1 as "found" from "workspace_archive" where "organization_id" = ?`, organizationId)
  if (existing === null) return failure(404, "NOT_ARCHIVED", "This workspace is not archived.")
  const input = await body(request)
  await run(context.db, `delete from "workspace_archive" where "organization_id" = ?`, organizationId)
  if (await first(context.db, `select 1 as "found" from "organization" where "id" = ?`, organizationId) === null) {
    return failure(404, "ORGANIZATION_NOT_FOUND", "Workspace no longer exists.")
  }
  if (input?.activate === true || session.session.activeOrganizationId == null) {
    await activate(context.auth, session.session.token, organizationId)
  }
  return json({ id: organizationId, archived: false })
}

/** DELETE /api/workspaces/:id: permanently delete a workspace and revoke its API keys (owner only). */
export async function deleteWorkspace(request: Request, context: WorkspaceContext, organizationId: string): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const membership = await first<{ role: string; metadata: string | null }>(context.db,
    `select m."role", o."metadata" from "member" m join "organization" o on o."id" = m."organizationId" where m."organizationId" = ? and m."userId" = ?`, organizationId, session.user.id)
  if (membership === null || !isOwner(membership.role)) {
    return failure(403, "FORBIDDEN", "Only a workspace owner can delete a workspace.")
  }
  if (isProvisioningMetadata(membership.metadata)) return failure(409, "WORKSPACE_PROVISIONING", "This workspace is still being provisioned.")
  if (context.defaultOrganizationId !== undefined && organizationId === context.defaultOrganizationId) {
    return failure(403, "FORBIDDEN", "The deployment's shared default workspace cannot be deleted.")
  }
  // Delete child rows explicitly in the same transaction so SQLite without PRAGMA foreign_keys=ON
  // behaves identically to D1, and revoke organization API keys before deleting the organization.
  const statements = [
    { sql: `update "session" set "activeOrganizationId" = null where "activeOrganizationId" = ?`, params: [organizationId] },
    { sql: `delete from "apikey" where "referenceId" = ?`, params: [organizationId] },
    { sql: `delete from "invitation" where "organizationId" = ?`, params: [organizationId] },
    { sql: `delete from "member" where "organizationId" = ?`, params: [organizationId] },
    { sql: `delete from "organization" where "id" = ?`, params: [organizationId] },
  ]
  try {
    await atomically(context.db as unknown as ProvisionDb, [
      { sql: `update "workspace_request" set "organization_id" = null where "organization_id" = ?`, params: [organizationId] },
      { sql: `delete from "workspace_archive" where "organization_id" = ?`, params: [organizationId] },
      ...statements,
    ])
  } catch (error) {
    if (!missingTable(error, "workspace_archive") && !missingTable(error, "workspace_request")) throw error
    await atomically(context.db as unknown as ProvisionDb, statements)
  }
  const nextId = await nextActiveWorkspace(context.db, session.user.id, organizationId)
  if (session.session.activeOrganizationId === organizationId || session.session.activeOrganizationId == null) {
    await activate(context.auth, session.session.token, nextId)
  }
  return json({ id: organizationId, deleted: true, nextActiveOrganizationId: nextId })
}

/** POST /api/workspaces/requests { name, note? }: record a workspace request for admins to review. */
export async function createWorkspaceRequest(request: Request, context: WorkspaceContext): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const input = await body(request)
  const name = typeof input?.name === "string" ? input.name.trim() : ""
  const note = typeof input?.note === "string" ? input.note.trim() : ""
  if (name === "" || name.length > MAX_NAME_LENGTH) {
    return failure(400, "VALIDATION_ERROR", `A workspace name must be between 1 and ${MAX_NAME_LENGTH} characters.`)
  }
  if (note.length > MAX_NOTE_LENGTH) {
    return failure(400, "VALIDATION_ERROR", `A note can be at most ${MAX_NOTE_LENGTH} characters.`)
  }
  const pending = await first<{ n: number }>(context.db,
    `select count(*) as "n" from "workspace_request" where "user_id" = ? and "status" = 'pending'`, session.user.id)
  if ((pending?.n ?? 0) >= MAX_PENDING_REQUESTS) {
    return failure(429, "TOO_MANY_REQUESTS", `You already have ${MAX_PENDING_REQUESTS} pending workspace requests. Withdraw one or wait for an admin to review them.`)
  }
  const active = await activeMembership(context.auth, session.user.id, session.session.activeOrganizationId)
  const id = crypto.randomUUID()
  const at = new Date().toISOString()
  await run(context.db,
    `insert into "workspace_request" ("id", "user_id", "organization_id", "name", "note", "status", "created_at") values (?, ?, ?, ?, ?, 'pending', ?)`,
    id, session.user.id, active?.organizationId ?? null, name, note, at)
  return json({ id, name, note, status: "pending", createdAt: at }, 201)
}

/** POST /api/workspaces/requests/:id/decide { decision: "approved" | "dismissed" }: admin reviews or requester withdraws. */
export async function decideWorkspaceRequest(request: Request, context: WorkspaceContext, requestId: string): Promise<Response> {
  const session = await context.auth.api.getSession({ headers: request.headers })
  if (session === null) return failure(401, "UNAUTHORIZED", "Sign in first.")
  const input = await body(request)
  const decision = input?.decision
  if (decision !== "approved" && decision !== "dismissed") {
    return failure(400, "VALIDATION_ERROR", `Decision must be "approved" or "dismissed".`)
  }
  const row = await first<{ id: string; user_id: string; organization_id: string | null; status: string }>(context.db,
    `select "id", "user_id", "organization_id", "status" from "workspace_request" where "id" = ?`, requestId)
  if (row === null) return failure(404, "REQUEST_NOT_FOUND", "No such workspace request.")
  const selfWithdraw = decision === "dismissed" && row.user_id === session.user.id
  if (!selfWithdraw) {
    if (row.organization_id === null) return failure(403, "FORBIDDEN", "Only a workspace admin can decide this request.")
    const membership = await first<{ role: string }>(context.db,
      `select "role" from "member" where "organizationId" = ? and "userId" = ?`, row.organization_id, session.user.id)
    if (membership === null || !isAdmin(membership.role)) {
      return failure(403, "FORBIDDEN", "Only a workspace admin can decide this request.")
    }
  }
  if (row.status !== "pending") return failure(409, "REQUEST_ALREADY_DECIDED", "This workspace request was already handled.")
  const at = new Date().toISOString()
  await run(context.db,
    `update "workspace_request" set "status" = ?, "decided_at" = ?, "decided_by" = ? where "id" = ? and "status" = 'pending'`,
    decision, at, session.user.id, requestId)
  const updated = await first<{ status: string; decided_at: string | null; decided_by: string | null }>(context.db,
    `select "status", "decided_at", "decided_by" from "workspace_request" where "id" = ?`, requestId)
  if (updated?.status !== decision || updated.decided_by !== session.user.id || updated.decided_at !== at) {
    return failure(409, "REQUEST_ALREADY_DECIDED", "This workspace request was already handled.")
  }
  return json({ id: requestId, status: decision, decidedAt: at })
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
  if (await isOrganizationArchived(context.auth, invitation.organizationId)) return failure(409, "WORKSPACE_ARCHIVED", "This workspace is archived. Ask an owner to restore it before joining.")
  const count = await first<{ n: number }>(context.db, `select count(*) as "n" from "member" where "organizationId" = ?`, invitation.organizationId)
  if ((count?.n ?? 0) >= MEMBERSHIP_LIMIT) return failure(403, "ORGANIZATION_MEMBERSHIP_LIMIT_REACHED", "Organization membership limit reached")
  const member = { id: crypto.randomUUID(), organizationId: invitation.organizationId, userId: session.user.id, role: invitation.role ?? "member", createdAt: new Date().toISOString() }
  const acceptStatements = (checkArchive: boolean) => [
    { sql: `update "invitation" set "status" = 'accepted' where "id" = ? and "status" = 'pending'
      and (case when typeof("expiresAt") in ('integer', 'real') then "expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday("expiresAt") > julianday('now') end)
      and exists (select 1 from "user" u where u."id" = ? and u."emailVerified" = 1 and lower(u."email") = lower("invitation"."email"))
      and exists (select 1 from "organization" o where o."id" = "invitation"."organizationId")${checkArchive ? `\n      and not exists (select 1 from "workspace_archive" a where a."organization_id" = "invitation"."organizationId")` : ""}
      and (select count(*) from "member" m where m."organizationId" = "invitation"."organizationId") < ${MEMBERSHIP_LIMIT}`, params: [invitationId, session.user.id] },
    { sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select ?, ?, ?, ?, ? where changes() = 1`, params: [member.id, member.organizationId, member.userId, member.role, member.createdAt] },
  ]
  try {
    // The invitation turns accepted only if, at the write, it is still pending and unexpired (the
    // database's clock), the session's user still has a verified address it was sent to, its workspace
    // exists, is not archived, and holds fewer than the membership limit; the member is inserted only
    // if that update happened. A transaction is serialized, so two acceptances cannot both count 99 members.
    try {
      await atomically(context.db as unknown as ProvisionDb, acceptStatements(true))
    } catch (error) {
      if (!missingTable(error, "workspace_archive")) throw error
      await atomically(context.db as unknown as ProvisionDb, acceptStatements(false))
    }
  } catch (error) {
    // Only the workspace the stored invitation names: never one the request body or query mentions.
    if (limitRaised(error) === "members") return planLimit(await admitMember(context.db as unknown as BillingDb, context.billing, context.baseURL, invitation.organizationId, session.user.id), "This workspace's member limit is reached.")
    if (/UNIQUE constraint failed: member\./.test(error instanceof Error ? error.message : "")) return failure(400, "INVITATION_NOT_FOUND", INVITATION_GONE)
    throw error
  }
  if (await first(context.db, `select 1 as "joined" from "member" where "id" = ?`, member.id) === null) {
    // Refused at the write: say which check failed, read again now.
    if (await isOrganizationArchived(context.auth, invitation.organizationId)) return failure(409, "WORKSPACE_ARCHIVED", "This workspace is archived. Ask an owner to restore it before joining.")
    const now = await first<{ n: number }>(context.db, `select count(*) as "n" from "member" where "organizationId" = ?`, invitation.organizationId)
    if ((now?.n ?? 0) >= MEMBERSHIP_LIMIT) return failure(403, "ORGANIZATION_MEMBERSHIP_LIMIT_REACHED", "Organization membership limit reached")
    return failure(400, "INVITATION_NOT_FOUND", INVITATION_GONE)
  }
  await activate(context.auth, session.session.token, member.organizationId)
  return json({ invitation: { ...invitation, status: "accepted" }, member })
}
