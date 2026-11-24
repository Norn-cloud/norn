import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { billingModeStatement } from "../billing.ts"
import worker, { type Env } from "../index.ts"
import type { WorkspacesDocument } from "../workspaces.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"
const MIGRATIONS = join(import.meta.dir, "../migrations")
const BASE_DIGEST = "a".repeat(64)

function migratedDb(): Database {
  const db = new Database(":memory:")
  for (const file of readdirSync(MIGRATIONS).filter((name) => name.endsWith(".sql")).sort()) {
    db.exec(readFileSync(join(MIGRATIONS, file), "utf8"))
  }
  return db
}

interface HostCall {
  readonly method: string
  readonly pathname: string
  readonly searchParams: Record<string, string>
  readonly claims: Record<string, unknown>
  readonly body: unknown
}

function mockHost(options: { readonly fail?: boolean; readonly digest?: string } = {}) {
  const calls: HostCall[] = []
  const digest = options.digest ?? BASE_DIGEST
  const host = {
    fetch: async (request: Request) => {
      const url = new URL(request.url)
      const authHeader = request.headers.get("authorization") ?? ""
      const token = authHeader.startsWith("Bearer ") ? authHeader.slice(7) : ""
      const claims = token.split(".")[1] ? JSON.parse(Buffer.from(token.split(".")[1]!, "base64url").toString()) as Record<string, unknown> : {}
      const text = await request.text()
      const body = text === "" ? null : JSON.parse(text) as { baseSha256?: string; artifacts?: unknown[] }
      calls.push({ method: request.method, pathname: url.pathname, searchParams: Object.fromEntries(url.searchParams), claims, body })
      if (url.pathname === "/v1/cell:provision") {
        if (options.fail) return Response.json({ code: "cell.unopenable", message: "storage unavailable" }, { status: 503 })
        if (body?.baseSha256 !== digest) {
          return Response.json({
            code: "cell.base_mismatch",
            message: "the submission was authored against another trusted base",
            explanation: `this host's trusted base is sha256 ${digest}; rebuild the submission against it`,
          }, { status: 409 })
        }
        return Response.json({ outcome: "provisioned", cellId: `cell_${url.searchParams.get("orgId")}`, definitionsVersion: 1, receiptSha256: "b".repeat(64), baseSha256: digest })
      }
      return Response.json({ artifacts: [] })
    },
  } as unknown as Fetcher
  return { host, calls }
}

function stage(options: { host?: Fetcher; defaultEnvironmentId?: string; defaultOrg?: string; billing?: boolean } = {}) {
  const db = migratedDb()
  db.run(billingModeStatement(options.billing === true))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...options.host === undefined ? {} : { NORN_HOST: options.host },
    ...options.defaultEnvironmentId === undefined ? {} : { STUDIO_DEFAULT_ENVIRONMENT_ID: options.defaultEnvironmentId },
    ...options.defaultOrg === undefined ? {} : { STUDIO_DEFAULT_ORG: options.defaultOrg },
  }
  return { db, env }
}

let ipCounter = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", `198.51.100.${++ipCounter % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const mutate = (env: Env, method: "POST" | "PATCH" | "DELETE", path: string, body?: unknown, headers: HeadersInit = {}) =>
  call(env, path, {
    method,
    ...body === undefined ? {} : { body: JSON.stringify(body) },
    headers: { origin: ORIGIN, ...body === undefined ? {} : { "content-type": "application/json" }, ...headers },
  })
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

async function enroll(env: Env, email: string, name = email.split("@")[0] ?? "User") {
  expect((await mutate(env, "POST", "/api/auth/sign-up/email", { email, password: "correct horse battery", name })).status).toBe(200)
  const signedIn = await mutate(env, "POST", "/api/auth/sign-in/email", { email, password: "correct horse battery" })
  expect(signedIn.status).toBe(200)
  const db = env.AUTH_DB as unknown as Database
  const { id: userId } = db.query(`select "id" from "user" where "email" = ?`).get(email) as { id: string }
  return { cookie: cookieOf(signedIn), userId }
}

test("GET /api/workspaces lists only the caller's workspaces, computes server capabilities, and reflects creation mode", async () => {
  const { host } = mockHost()
  const { db, env } = stage({ host, defaultEnvironmentId: "prod" })
  const alice = await enroll(env, "alice@example.com", "Alice")
  const bob = await enroll(env, "bob@example.com", "Bob")
  const bobOrg = (db.query(`select "organizationId" from "member" where "userId" = ?`).get(bob.userId) as { organizationId: string }).organizationId
  // Add Alice as a viewer to Bob's org.
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_v', ?, ?, 'viewer', ?)`, [bobOrg, alice.userId, new Date(Date.now() + 1000).toISOString()])

  const doc = (await (await call(env, "/api/workspaces", { headers: { cookie: alice.cookie } })).json()) as WorkspacesDocument
  expect(doc.creation).toEqual({ mode: "self-serve", allowed: true })
  expect(doc.workspaces).toHaveLength(2)
  expect(doc.workspaces[0]).toMatchObject({ name: "Alice's workspace", role: "owner", active: true, hasCell: true, canRename: true, canArchive: true, canDelete: true })
  expect(doc.workspaces[1]).toMatchObject({ id: bobOrg, name: "Bob's workspace", role: "viewer", active: false, canRename: false, canArchive: false, canDelete: false })

  // Without NORN_HOST or STUDIO_DEFAULT_ENVIRONMENT_ID, creation mode is "request".
  const noEnv = stage({ host }).env
  const carol = await enroll(noEnv, "carol@example.com", "Carol")
  const reqDoc = (await (await call(noEnv, "/api/workspaces", { headers: { cookie: carol.cookie } })).json()) as WorkspacesDocument
  expect(reqDoc.creation).toEqual({ mode: "request" })
})

test("POST /api/workspaces provisions an isolated cell on NORN_HOST (negotiating baseSha256), activates it, and rolls back on host failure", async () => {
  const { host, calls } = mockHost()
  const { db, env } = stage({ host, defaultEnvironmentId: "rehearsal2" })
  const alice = await enroll(env, "provision@example.com", "Alice")

  // Cross-origin is refused before anything runs.
  expect((await mutate(env, "POST", "/api/workspaces", { name: "Evil" }, { cookie: alice.cookie, origin: "https://evil.example" })).status).toBe(403)

  // First creation negotiates the host's baseSha256 (probe 409 -> retry 200) and caches it.
  const created = await mutate(env, "POST", "/api/workspaces", { name: "Operations", metadata: { environmentId: "forged", "norn.billed": 0, note: "kept" } }, { cookie: alice.cookie })
  expect(created.status).toBe(200)
  const payload = (await created.json()) as { id: string; name: string; metadata: Record<string, unknown> }
  expect(payload.name).toBe("Operations")
  expect(payload.metadata).toEqual({ note: "kept" })
  expect(calls).toHaveLength(2)
  expect(calls[0]!.pathname).toBe("/v1/cell:provision")
  expect(calls[0]!.searchParams).toEqual({ orgId: payload.id, environmentId: "rehearsal2" })
  expect(calls[0]!.claims.norn).toMatchObject({ kind: "user", org: payload.id, role: "owner", amr: "password" })
  expect(calls[1]!.body).toEqual({ baseSha256: BASE_DIGEST, artifacts: [] })

  // /api/me now points to the newly created workspace and its server-assigned environment.
  const me = (await (await call(env, "/api/me", { headers: { cookie: alice.cookie } })).json()) as { organization: { id: string; name: string }; environmentId: string }
  expect(me).toMatchObject({ organization: { id: payload.id, name: "Operations" }, environmentId: "rehearsal2" })

  // Second creation uses the cached baseSha256 in a single round-trip.
  const second = await mutate(env, "POST", "/api/workspaces", { name: "Finance" }, { cookie: alice.cookie })
  expect(second.status).toBe(200)
  expect(calls).toHaveLength(3)

  // If NORN_HOST fails to provision, the newly created org + member + billing_workspace roll back cleanly.
  const failing = mockHost({ fail: true })
  const failedStage = stage({ host: failing.host, defaultEnvironmentId: "rehearsal2", billing: true })
  const user = await enroll(failedStage.env, "rollback@example.com", "Roll")
  // Raise limit so the billing trigger admits the insert and the host failure triggers rollback.
  failedStage.db.run(`insert into "billing_limit" ("payer_id", "plan", "status", "cells", "members", "provider", "provider_ref", "as_of", "updated_at") values (?, 'pro', 'active', 3, 10, 'test', 'sub_1', 1, 1)`, [user.userId])
  const failed = await mutate(failedStage.env, "POST", "/api/workspaces", { name: "Broken" }, { cookie: user.cookie })
  expect(failed.status).toBe(502)
  expect(await failed.json()).toMatchObject({ code: "CELL_PROVISION_FAILED" })
  expect(failedStage.db.query(`select count(*) as n from "organization" where "name" = 'Broken'`).get()).toEqual({ n: 0 })
  expect(failedStage.db.query(`select count(*) as n from "billing_workspace" where "payer_id" = ?`).get(user.userId)).toEqual({ n: 1 })
  expect(db.query(`select count(*) as n from "organization"`).get()).toEqual({ n: 3 })
})

test("when NORN_HOST is connected without STUDIO_DEFAULT_ENVIRONMENT_ID, self-serve POST /api/workspaces refuses", async () => {
  const { host } = mockHost()
  const { env } = stage({ host })
  const user = await enroll(env, "noenv@example.com", "NoEnv")
  const response = await mutate(env, "POST", "/api/workspaces", { name: "Blocked" }, { cookie: user.cookie })
  expect(response.status).toBe(403)
  expect(await response.json()).toMatchObject({ code: "SELF_SERVE_UNAVAILABLE" })
})

test("activate, rename, archive/restore (undo), and permanent delete enforce server-side role checks and isolation", async () => {
  const { host } = mockHost()
  const { db, env } = stage({ host, defaultEnvironmentId: "prod", defaultOrg: "org_shared" })
  const owner = await enroll(env, "owner@example.com", "Owner")
  const admin = await enroll(env, "admin@example.com", "Admin")
  const member = await enroll(env, "member@example.com", "Member")
  const outsider = await enroll(env, "outsider@example.com", "Outsider")

  // Owner creates a second workspace "Lab".
  const created = (await (await mutate(env, "POST", "/api/workspaces", { name: "Lab" }, { cookie: owner.cookie })).json()) as { id: string }
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_adm', ?, ?, 'admin', ?)`, [created.id, admin.userId, new Date().toISOString()])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_mem', ?, ?, 'member', ?)`, [created.id, member.userId, new Date().toISOString()])

  // Switch active: member succeeds, outsider is refused 403.
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/activate`, {}, { cookie: member.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/activate`, {}, { cookie: outsider.cookie })).status).toBe(403)

  // Rename: admin and owner succeed; member and outsider are refused 403; empty name is 400.
  expect((await mutate(env, "PATCH", `/api/workspaces/${created.id}`, { name: "Lab Renamed" }, { cookie: admin.cookie })).status).toBe(200)
  expect((await mutate(env, "PATCH", `/api/workspaces/${created.id}`, { name: "Bad" }, { cookie: member.cookie })).status).toBe(403)
  expect((await mutate(env, "PATCH", `/api/workspaces/${created.id}`, { name: "Bad" }, { cookie: outsider.cookie })).status).toBe(403)
  expect((await mutate(env, "PATCH", `/api/workspaces/${created.id}`, { name: "   " }, { cookie: owner.cookie })).status).toBe(400)

  // Archive: admin and member are refused 403; owner succeeds and switches active org automatically.
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/archive`, {}, { cookie: admin.cookie })).status).toBe(403)
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/archive`, {}, { cookie: member.cookie })).status).toBe(403)
  const archived = await mutate(env, "POST", `/api/workspaces/${created.id}/archive`, {}, { cookie: owner.cookie })
  expect(archived.status).toBe(200)
  const archivedBody = (await archived.json()) as { archived: boolean; nextActiveOrganizationId: string | null }
  expect(archivedBody.archived).toBe(true)
  expect(archivedBody.nextActiveOrganizationId).not.toBe(created.id)
  expect(archivedBody.nextActiveOrganizationId).not.toBeNull()

  // Archived workspace cannot be activated (neither via /api/workspaces nor Better Auth set-active) or renamed.
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/activate`, {}, { cookie: owner.cookie })).status).toBe(409)
  expect((await mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: created.id }, { cookie: owner.cookie })).status).toBe(409)
  // Non-member hitting set-active on archived org still gets 403 (no existence oracle).
  expect((await mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: created.id }, { cookie: outsider.cookie })).status).toBe(403)
  expect((await mutate(env, "PATCH", `/api/workspaces/${created.id}`, { name: "Nope" }, { cookie: owner.cookie })).status).toBe(409)

  // Archived workspace appears in owner's archived list, not active list, and NOT in member's archived list.
  const ownerList = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(ownerList.workspaces.some((w) => w.id === created.id)).toBe(false)
  expect(ownerList.archived.map((w) => w.id)).toEqual([created.id])
  const memberList = (await (await call(env, "/api/workspaces", { headers: { cookie: member.cookie } })).json()) as WorkspacesDocument
  expect(memberList.archived).toEqual([])

  // Restore (undo): admin refused 403; owner restores and reactivates.
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/restore`, { activate: true }, { cookie: admin.cookie })).status).toBe(403)
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/restore`, { activate: true }, { cookie: owner.cookie })).status).toBe(200)
  const afterRestore = (await (await call(env, "/api/me", { headers: { cookie: owner.cookie } })).json()) as { organization: { id: string; name: string } }
  expect(afterRestore.organization).toMatchObject({ id: created.id, name: "Lab Renamed" })

  // Create an org API key for the workspace, then permanently delete it: API key must be revoked too.
  db.run(`insert into "apikey" ("id", "configId", "name", "referenceId", "key", "createdAt", "updatedAt") values ('k1', 'default', 'svc', ?, 'hash', '2026', '2026')`, [created.id])
  expect((await mutate(env, "DELETE", `/api/workspaces/${created.id}`, undefined, { cookie: admin.cookie })).status).toBe(403)
  expect((await mutate(env, "DELETE", `/api/workspaces/${created.id}`, undefined, { cookie: member.cookie })).status).toBe(403)
  const deleted = await mutate(env, "DELETE", `/api/workspaces/${created.id}`, undefined, { cookie: owner.cookie })
  expect(deleted.status).toBe(200)
  expect(db.query(`select count(*) as n from "organization" where "id" = ?`).get(created.id)).toEqual({ n: 0 })
  expect(db.query(`select count(*) as n from "member" where "organizationId" = ?`).get(created.id)).toEqual({ n: 0 })
  expect(db.query(`select count(*) as n from "apikey" where "referenceId" = ?`).get(created.id)).toEqual({ n: 0 })
})

test("archiving a workspace does not bypass billing cell limits", async () => {
  const { host } = mockHost()
  const { db, env } = stage({ host, defaultEnvironmentId: "prod", billing: true })
  const owner = await enroll(env, "billed@example.com", "Billed")
  const ownOrg = (db.query(`select "organizationId" from "member" where "userId" = ?`).get(owner.userId) as { organizationId: string }).organizationId
  // Free plan allows 1 workspace: second creation is refused.
  expect((await mutate(env, "POST", "/api/workspaces", { name: "Two" }, { cookie: owner.cookie })).status).toBe(403)
  const listBefore = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(listBefore.creation).toMatchObject({ mode: "self-serve", allowed: false })
  expect(listBefore.planLabel).toBe("Free plan")
  // Archiving the first workspace MUST NOT free its billed slot (because restore would yield 2 active cells!).
  expect((await mutate(env, "POST", `/api/workspaces/${ownOrg}/archive`, {}, { cookie: owner.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", "/api/workspaces", { name: "Sneaky" }, { cookie: owner.cookie })).status).toBe(403)
})

test("workspace requests: members record and withdraw; admins see org requests and approve or dismiss; non-admins cannot approve", async () => {
  const { db, env } = stage()
  const owner = await enroll(env, "req-owner@example.com", "Owner")
  const member = await enroll(env, "req-member@example.com", "Member")
  const outsider = await enroll(env, "req-outsider@example.com", "Outsider")
  const sharedOrg = (db.query(`select "organizationId" from "member" where "userId" = ?`).get(owner.userId) as { organizationId: string }).organizationId
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_req', ?, ?, 'member', ?)`, [sharedOrg, member.userId, new Date().toISOString()])
  expect((await mutate(env, "POST", `/api/workspaces/${sharedOrg}/activate`, {}, { cookie: member.cookie })).status).toBe(200)

  // Member submits a workspace request.
  const created = await mutate(env, "POST", "/api/workspaces/requests", { name: "Analytics Sandbox", note: "For Q4 projections" }, { cookie: member.cookie })
  expect(created.status).toBe(201)
  const req = (await created.json()) as { id: string; name: string; status: string }
  expect(req).toMatchObject({ name: "Analytics Sandbox", status: "pending" })

  // Member sees it as withdrawable, NOT manageable; cannot approve their own request.
  const memberDoc = (await (await call(env, "/api/workspaces", { headers: { cookie: member.cookie } })).json()) as WorkspacesDocument
  expect(memberDoc.requests).toHaveLength(1)
  expect(memberDoc.requests[0]).toMatchObject({ id: req.id, canManage: false, canWithdraw: true })
  expect((await mutate(env, "POST", `/api/workspaces/requests/${req.id}/decide`, { decision: "approved" }, { cookie: member.cookie })).status).toBe(403)

  // Outsider does not see it and cannot decide it.
  const outsiderDoc = (await (await call(env, "/api/workspaces", { headers: { cookie: outsider.cookie } })).json()) as WorkspacesDocument
  expect(outsiderDoc.requests).toEqual([])
  expect((await mutate(env, "POST", `/api/workspaces/requests/${req.id}/decide`, { decision: "dismissed" }, { cookie: outsider.cookie })).status).toBe(403)

  // Owner sees it as manageable and approves it; a second decision is refused 409.
  const ownerDoc = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(ownerDoc.requests[0]).toMatchObject({ id: req.id, canManage: true })
  expect((await mutate(env, "POST", `/api/workspaces/requests/${req.id}/decide`, { decision: "approved" }, { cookie: owner.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", `/api/workspaces/requests/${req.id}/decide`, { decision: "dismissed" }, { cookie: owner.cookie })).status).toBe(409)

  // Pending request cap: 5 allowed, 6th returns 429; withdrawing one frees a slot.
  let firstPending = ""
  for (let i = 0; i < 5; i++) {
    const res = await mutate(env, "POST", "/api/workspaces/requests", { name: `Req ${i}` }, { cookie: member.cookie })
    expect(res.status).toBe(201)
    if (i === 0) firstPending = ((await res.json()) as { id: string }).id
  }
  expect((await mutate(env, "POST", "/api/workspaces/requests", { name: "Too many" }, { cookie: member.cookie })).status).toBe(429)
  expect((await mutate(env, "POST", `/api/workspaces/requests/${firstPending}/decide`, { decision: "dismissed" }, { cookie: member.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", "/api/workspaces/requests", { name: "Now fits" }, { cookie: member.cookie })).status).toBe(201)
})

test("#1414: accepting an invitation to an archived workspace is refused until restored", async () => {
  const { db, env } = stage()
  const owner = await enroll(env, "owner@example.com", "Owner")
  const invitee = await enroll(env, "invitee@example.com", "Invitee")
  db.prepare(`update "user" set "emailVerified" = 1 where "id" = ?`).run(invitee.userId)
  const created = (await (await mutate(env, "POST", "/api/workspaces", { name: "Archived Target" }, { cookie: owner.cookie })).json()) as { id: string }
  const invId = crypto.randomUUID()
  const now = new Date().toISOString()
  db.prepare(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, ?, ?, 'member', 'pending', ?, ?, ?)`)
    .run(invId, created.id, "invitee@example.com", new Date(Date.now() + 3_600_000).toISOString(), now, owner.userId)

  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/archive`, {}, { cookie: owner.cookie })).status).toBe(200)
  const listed = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(listed.archived.find((w) => w.id === created.id)?.canArchive).toBe(false)

  const blocked = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie })
  expect(blocked.status).toBe(409)
  expect(((await blocked.json()) as { code: string }).code).toBe("WORKSPACE_ARCHIVED")
  expect(db.prepare(`select count(*) as n from "member" where "organizationId" = ? and "userId" = ?`).get(created.id, invitee.userId)).toEqual({ n: 0 })

  // After restore, the same pending invitation succeeds.
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/restore`, { activate: false }, { cookie: owner.cookie })).status).toBe(200)
  // Race: archive commits right after the pre-check and before the atomic acceptance write -> invitation stays pending, 409 returned.
  const origPrepare = db.prepare.bind(db)
  let injected = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!injected && sql.includes(`update "invitation" set "status" = 'accepted'`)) {
      injected = true
      origPrepare(`insert into "workspace_archive" ("organization_id", "archived_at", "archived_by") values (?, ?, ?)`).run(created.id, new Date().toISOString(), owner.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const raced = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie })
  db.prepare = origPrepare
  expect(raced.status).toBe(409)
  expect(((await raced.json()) as { code: string }).code).toBe("WORKSPACE_ARCHIVED")
  expect(db.prepare(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
  expect(db.prepare(`select count(*) as n from "member" where "organizationId" = ? and "userId" = ?`).get(created.id, invitee.userId)).toEqual({ n: 0 })

  // Restore once more and accept cleanly.
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/restore`, { activate: false }, { cookie: owner.cookie })).status).toBe(200)
  const accepted = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie })
  expect(accepted.status).toBe(200)
  expect(db.prepare(`select count(*) as n from "member" where "organizationId" = ? and "userId" = ?`).get(created.id, invitee.userId)).toEqual({ n: 1 })
})

test("#1414: deleting the archiving user preserves archive state (ON DELETE SET NULL) and keeps API keys blocked", async () => {
  const { db, env } = stage()
  db.exec("PRAGMA foreign_keys = ON")
  const owner = await enroll(env, "archiver@example.com", "Archiver")
  const created = (await (await mutate(env, "POST", "/api/workspaces", { name: "Locked Org" }, { cookie: owner.cookie })).json()) as { id: string }
  expect((await mutate(env, "POST", `/api/workspaces/${created.id}/archive`, {}, { cookie: owner.cookie })).status).toBe(200)
  // Deleting the user who archived the org must NOT cascade-delete workspace_archive.
  db.prepare(`delete from "user" where "id" = ?`).run(owner.userId)
  const archiveRow = db.prepare(`select "organization_id", "archived_by" from "workspace_archive" where "organization_id" = ?`).get(created.id) as { organization_id: string; archived_by: string | null } | null
  expect(archiveRow).toEqual({ organization_id: created.id, archived_by: null })
})

test("#1414: workspace is hidden and unactivatable while host cell provisioning is in flight, and rolls back if host throws", async () => {
  let releaseProvision!: () => void
  let provisionStarted!: () => void
  const started = new Promise<void>((resolve) => { provisionStarted = resolve })
  const gate = new Promise<void>((resolve) => { releaseProvision = resolve })
  let throwOnCall = false
  const gatedHost = {
    fetch: async (request: Request) => {
      const url = new URL(request.url)
      if (url.pathname === "/v1/cell:provision") {
        if (throwOnCall) throw new Error("unexpected host crash")
        provisionStarted()
        await gate
        return Response.json({ outcome: "provisioned", cellId: "c1", definitionsVersion: 1, receiptSha256: "b".repeat(64), baseSha256: BASE_DIGEST })
      }
      return Response.json({ artifacts: [] })
    },
  } as unknown as Fetcher

  const { db, env } = stage({ host: gatedHost, defaultEnvironmentId: "prod" })
  const owner = await enroll(env, "in-flight@example.com", "Owner")

  // Kick off creation without awaiting completion yet.
  const creating = mutate(env, "POST", "/api/workspaces", { name: "In Flight Cell" }, { cookie: owner.cookie })
  await started
  const pendingOrg = db.prepare(`select "id" from "organization" where "name" = 'In Flight Cell'`).get() as { id: string } | null
  expect(pendingOrg).not.toBeNull()

  // While provisioning is in flight: list omits it and activate/delete refuse with 409 WORKSPACE_PROVISIONING.
  const listedMid = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(listedMid.workspaces.some((w) => w.id === pendingOrg!.id)).toBe(false)
  const actMid = await mutate(env, "POST", `/api/workspaces/${pendingOrg!.id}/activate`, {}, { cookie: owner.cookie })
  expect(actMid.status).toBe(409)
  expect(((await actMid.json()) as { code: string }).code).toBe("WORKSPACE_PROVISIONING")
  const delMid = await mutate(env, "DELETE", `/api/workspaces/${pendingOrg!.id}`, undefined, { cookie: owner.cookie })
  expect(delMid.status).toBe(409)
  expect(((await delMid.json()) as { code: string }).code).toBe("WORKSPACE_PROVISIONING")

  // Once provisioning resolves, creation completes and the workspace appears with hasCell: true.
  releaseProvision()
  const finished = await creating
  expect(finished.status).toBe(200)
  const listedAfter = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(listedAfter.workspaces.find((w) => w.id === pendingOrg!.id)).toMatchObject({ name: "In Flight Cell", hasCell: true, active: true })

  // If the host throws unexpectedly, the workspace and its membership are rolled back cleanly.
  throwOnCall = true
  const crashed = await mutate(env, "POST", "/api/workspaces", { name: "Crashed Cell" }, { cookie: owner.cookie })
  expect(crashed.status).toBe(502)
  expect(db.prepare(`select count(*) as n from "organization" where "name" = 'Crashed Cell'`).get()).toEqual({ n: 0 })
})

test("#1414: pending workspace requests stay visible ahead of 50+ newer decided requests", async () => {
  const { db, env } = stage()
  const owner = await enroll(env, "history@example.com", "History Owner")
  const activeOrg = (db.query(`select "organizationId" from "member" where "userId" = ?`).get(owner.userId) as { organizationId: string }).organizationId
  // Insert 5 old pending requests, then 55 newer decided requests.
  for (let i = 0; i < 5; i++) {
    db.prepare(`insert into "workspace_request" ("id", "user_id", "organization_id", "name", "note", "status", "created_at") values (?, ?, ?, ?, '', 'pending', ?)`)
      .run(`old-pending-${i}`, owner.userId, activeOrg, `Old Pending ${i}`, `2026-01-01T00:00:0${i}.000Z`)
  }
  for (let i = 0; i < 55; i++) {
    db.prepare(`insert into "workspace_request" ("id", "user_id", "organization_id", "name", "note", "status", "created_at") values (?, ?, ?, ?, '', 'dismissed', ?)`)
      .run(`new-decided-${i}`, owner.userId, activeOrg, `New Decided ${i}`, `2026-11-01T00:${String(i).padStart(2, "0")}:00.000Z`)
  }
  const doc = (await (await call(env, "/api/workspaces", { headers: { cookie: owner.cookie } })).json()) as WorkspacesDocument
  expect(doc.requests).toHaveLength(50)
  const pendingIds = doc.requests.filter((r) => r.status === "pending").map((r) => r.id).sort()
  expect(pendingIds).toEqual(["old-pending-0", "old-pending-1", "old-pending-2", "old-pending-3", "old-pending-4"])
})

test("#1414: Better Auth direct org routes cannot bypass request-only mode, archive state, or default-org protection", async () => {
  // 1. Request-only deployment (NORN_HOST without STUDIO_DEFAULT_ENVIRONMENT_ID, billing off):
  // direct /api/auth/organization/create is refused with 403 SELF_SERVE_UNAVAILABLE.
  const { host: noEnvHost } = mockHost()
  const requestOnly = stage({ host: noEnvHost })
  const requester = await enroll(requestOnly.env, "req-direct@example.com", "Requester")
  const directCreate = await mutate(requestOnly.env, "POST", "/api/auth/organization/create", { name: "Sneaky", slug: "sneaky-org" }, { cookie: requester.cookie })
  expect(directCreate.status).toBe(403)
  expect(await directCreate.json()).toMatchObject({ code: "SELF_SERVE_UNAVAILABLE" })

  // 2. Archived org: direct Better Auth update, invite-member, and accept-invitation (billing off) all refuse 409.
  const { host } = mockHost()
  const { db, env } = stage({ host, defaultEnvironmentId: "prod", defaultOrg: "org_shared" })
  const owner = await enroll(env, "direct-owner@example.com", "Owner")
  const invitee = await enroll(env, "direct-invitee@example.com", "Invitee")
  db.prepare(`update "user" set "emailVerified" = 1 where "id" = ?`).run(invitee.userId)
  const target = (await (await mutate(env, "POST", "/api/workspaces", { name: "Direct Guarded" }, { cookie: owner.cookie })).json()) as { id: string }

  // Seed a pending invitation before archiving.
  const invId = crypto.randomUUID()
  db.prepare(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, ?, ?, 'member', 'pending', ?, ?, ?)`)
    .run(invId, target.id, "direct-invitee@example.com", new Date(Date.now() + 3_600_000).toISOString(), new Date().toISOString(), owner.userId)
  expect((await mutate(env, "POST", `/api/workspaces/${target.id}/archive`, {}, { cookie: owner.cookie })).status).toBe(200)

  const directUpdate = await mutate(env, "POST", "/api/auth/organization/update", { organizationId: target.id, data: { name: "Hijacked" } }, { cookie: owner.cookie })
  expect(directUpdate.status).toBe(409)
  expect(await directUpdate.json()).toMatchObject({ code: "WORKSPACE_ARCHIVED" })

  const directInvite = await mutate(env, "POST", "/api/auth/organization/invite-member", { organizationId: target.id, email: "third@example.com", role: "member" }, { cookie: owner.cookie })
  expect(directInvite.status).toBe(409)
  expect(await directInvite.json()).toMatchObject({ code: "WORKSPACE_ARCHIVED" })

  const directAccept = await mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: invId }, { cookie: invitee.cookie })
  expect(directAccept.status).toBe(409)
  expect(await directAccept.json()).toMatchObject({ code: "WORKSPACE_ARCHIVED" })
  expect(db.prepare(`select count(*) as n from "member" where "organizationId" = ? and "userId" = ?`).get(target.id, invitee.userId)).toEqual({ n: 0 })

  // 3. Shared default org cannot be deleted via Better Auth's direct delete endpoint either.
  db.prepare(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_shared', 'Shared', 'shared', '{}', ?)`).run(new Date().toISOString())
  db.prepare(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_shared_owner', 'org_shared', ?, 'owner', ?)`).run(owner.userId, new Date().toISOString())
  const directDelete = await mutate(env, "POST", "/api/auth/organization/delete", { organizationId: "org_shared" }, { cookie: owner.cookie })
  expect(directDelete.status).toBe(403)
  expect(db.prepare(`select count(*) as n from "organization" where "id" = 'org_shared'`).get()).toEqual({ n: 1 })
})
