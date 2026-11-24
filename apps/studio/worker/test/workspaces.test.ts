import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { createAuth } from "../auth.ts"
import { billingModeStatement } from "../billing.ts"
import worker, { type Env } from "../index.ts"
import type { WorkspacesDocument } from "../workspaces.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"
const MIGRATIONS = join(import.meta.dir, "../migrations")

function migratedDb(): Database {
  const db = new Database(":memory:")
  for (const file of readdirSync(MIGRATIONS).filter((name) => name.endsWith(".sql")).sort()) {
    db.exec(readFileSync(join(MIGRATIONS, file), "utf8"))
  }
  return db
}

function stage(options: { host?: Fetcher; defaultEnvironmentId?: string; billing?: boolean } = {}) {
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
  }
  return { db, env }
}

const stubHost = { fetch: async () => Response.json({ artifacts: [] }) } as unknown as Fetcher

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
  const { organizationId } = db.query(`select "organizationId" from "member" where "userId" = ? order by "createdAt" asc limit 1`).get(userId) as { organizationId: string }
  return { cookie: cookieOf(signedIn), userId, organizationId }
}

test("GET /api/workspaces lists only the caller's ready workspaces and omits provisioning orgs", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod", billing: true })
  expect((await call(env, "/api/workspaces")).status).toBe(401)
  const alice = await enroll(env, "alice@example.com", "Alice")
  const bob = await enroll(env, "bob@example.com", "Bob")
  // Add Alice as a viewer to Bob's workspace and as an owner of a provisioning org.
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_v', ?, ?, 'viewer', ?)`, [bob.organizationId, alice.userId, new Date(Date.now() + 1000).toISOString()])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_prov', 'Provisioning', 'provisioning', '{"norn.provisioning":true}', ?)`, [new Date(Date.now() + 2000).toISOString()])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_prov', 'org_prov', ?, 'owner', ?)`, [alice.userId, new Date(Date.now() + 2000).toISOString()])

  const doc = (await (await call(env, "/api/workspaces", { headers: { cookie: alice.cookie } })).json()) as WorkspacesDocument
  expect(doc.planLabel).toBe("Free plan")
  expect(doc.workspaces).toHaveLength(2)
  expect(doc.workspaces[0]).toMatchObject({ id: alice.organizationId, name: "Alice's workspace", role: "owner", active: true, hasCell: true })
  expect(doc.workspaces[1]).toMatchObject({ id: bob.organizationId, name: "Bob's workspace", role: "viewer", active: false, hasCell: true })
  expect(doc.workspaces.some((w) => w.id === "org_prov")).toBe(false)
})

test("POST /api/workspaces/:id/activate enforces membership and readiness at commit", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const alice = await enroll(env, "act-alice@example.com", "Alice")
  const bob = await enroll(env, "act-bob@example.com", "Bob")
  const outsider = await enroll(env, "act-out@example.com", "Outsider")
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_ab', ?, ?, 'member', ?)`, [bob.organizationId, alice.userId, new Date().toISOString()])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_prov', 'Prov', 'prov', '{"norn.provisioning":true}', ?)`, [new Date().toISOString()])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_ap', 'org_prov', ?, 'owner', ?)`, [alice.userId, new Date().toISOString()])

  // Member succeeds, outsider gets 403, provisioning org gets 409.
  expect((await mutate(env, "POST", `/api/workspaces/${bob.organizationId}/activate`, {}, { cookie: alice.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", `/api/workspaces/${bob.organizationId}/activate`, {}, { cookie: outsider.cookie })).status).toBe(403)
  const prov = await mutate(env, "POST", "/api/workspaces/org_prov/activate", {}, { cookie: alice.cookie })
  expect(prov.status).toBe(409)
  expect(await prov.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })

  // Switch back to Alice's workspace, then simulate a race where membership in Bob's org is deleted
  // between the pre-check and the conditional session update.
  expect((await mutate(env, "POST", `/api/workspaces/${alice.organizationId}/activate`, {}, { cookie: alice.cookie })).status).toBe(200)
  const origPrepare = db.prepare.bind(db)
  let raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "session" set "activeOrganizationId"`)) {
      raced = true
      origPrepare(`delete from "member" where "organizationId" = ? and "userId" = ?`).run(bob.organizationId, alice.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const failed = await mutate(env, "POST", `/api/workspaces/${bob.organizationId}/activate`, {}, { cookie: alice.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(failed.status).toBe(403)
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(alice.userId)).toEqual({ activeOrganizationId: alice.organizationId })
})

test("POST /api/workspaces refuses self-serve creation when NORN_HOST is connected", async () => {
  for (const billing of [false, true]) {
    const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod", billing })
    const user = await enroll(env, `hosted-${billing}@example.com`, "Hosted")
    const created = await mutate(env, "POST", "/api/workspaces", { name: "Blocked", slug: "blocked" }, { cookie: user.cookie })
    expect(created.status).toBe(403)
    expect(await created.json()).toMatchObject({ code: "WORKSPACE_CREATION_UNAVAILABLE" })
    expect(db.query(`select count(*) as n from "organization" where "slug" = 'blocked'`).get()).toEqual({ n: 0 })
  }
})

test("direct Better Auth organization/create and organization/delete are disabled for normal users", async () => {
  // Billing off (with and without NORN_HOST): direct create & delete both refuse 403.
  for (const host of [undefined, stubHost]) {
    const { db, env } = stage({ host, defaultEnvironmentId: "prod", billing: false })
    const owner = await enroll(env, "direct@example.com", "Owner")
    for (const path of ["/api/auth/organization/create", "/api/auth/organization/create/", "/api/auth/Organization/Create"]) {
      const created = await mutate(env, "POST", path, { name: "Sneaky", slug: "sneaky-org" }, { cookie: owner.cookie })
      expect([403, 404]).toContain(created.status)
      if (created.status === 403) expect(await created.json()).toMatchObject({ code: "ORGANIZATION_CREATE_DISABLED" })
    }
    expect(db.query(`select count(*) as n from "organization" where "slug" = 'sneaky-org'`).get()).toEqual({ n: 0 })

    // Seed an API key on the owner's org and confirm direct organization/delete is refused and leaves org + key intact.
    db.run(`insert into "apikey" ("id", "configId", "name", "referenceId", "key", "createdAt", "updatedAt") values ('k_keep', 'default', 'svc', ?, 'hash', '2026', '2026')`, [owner.organizationId])
    const deleted = await mutate(env, "POST", "/api/auth/organization/delete", { organizationId: owner.organizationId }, { cookie: owner.cookie })
    expect(deleted.status).toBe(403)
    expect(await deleted.json()).toMatchObject({ code: "ORGANIZATION_DELETE_DISABLED" })
    expect(db.query(`select count(*) as n from "organization" where "id" = ?`).get(owner.organizationId)).toEqual({ n: 1 })
    expect(db.query(`select count(*) as n from "apikey" where "id" = 'k_keep'`).get()).toEqual({ n: 1 })
  }

  // Billing on: direct create still returns 404 USE_STUDIO_ROUTE (preserving billing route contract).
  const billed = stage({ host: stubHost, billing: true })
  const billedOwner = await enroll(billed.env, "billed-direct@example.com", "Owner")
  const billedCreate = await mutate(billed.env, "POST", "/api/auth/organization/create", { name: "Sneaky", slug: "sneaky-billed" }, { cookie: billedOwner.cookie })
  expect(billedCreate.status).toBe(404)
  expect(await billedCreate.json()).toMatchObject({ code: "USE_STUDIO_ROUTE" })
  const billedDelete = await mutate(billed.env, "POST", "/api/auth/organization/delete", { organizationId: billedOwner.organizationId }, { cookie: billedOwner.cookie })
  expect(billedDelete.status).toBe(403)
  expect(await billedDelete.json()).toMatchObject({ code: "ORGANIZATION_DELETE_DISABLED" })
})

test("Better Auth organization/set-active validates organizationSlug, organizationId, mismatches, and provisioning state", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const alice = await enroll(env, "slug-alice@example.com", "Alice")
  const outsider = await enroll(env, "slug-out@example.com", "Outsider")
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_ready', 'Ready Org', 'ready-slug', '{"environmentId":"prod"}', ?)`, [new Date().toISOString()])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_ready', 'org_ready', ?, 'member', ?)`, [alice.userId, new Date().toISOString()])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_prov', 'Prov Org', 'prov-slug', '{"norn.provisioning":true}', ?)`, [new Date().toISOString()])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_prov', 'org_prov', ?, 'owner', ?)`, [alice.userId, new Date().toISOString()])

  // Provisioning org is refused whether addressed by organizationId OR by organizationSlug.
  for (const body of [{ organizationId: "org_prov" }, { organizationSlug: "prov-slug" }]) {
    const res = await mutate(env, "POST", "/api/auth/organization/set-active", body, { cookie: alice.cookie })
    expect({ body, status: res.status }).toEqual({ body, status: 409 })
    expect(await res.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  }

  // Non-member addressing either org (by ID or by slug) gets 403 (never 409 existence leak).
  for (const body of [{ organizationId: "org_prov" }, { organizationSlug: "prov-slug" }, { organizationSlug: "ready-slug" }]) {
    const res = await mutate(env, "POST", "/api/auth/organization/set-active", body, { cookie: outsider.cookie })
    expect({ body, status: res.status }).toEqual({ body, status: 403 })
  }

  // Mismatched organizationId + organizationSlug is rejected 400.
  const mismatch = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: "org_ready", organizationSlug: "prov-slug" }, { cookie: alice.cookie })
  expect(mismatch.status).toBe(400)
  expect(await mismatch.json()).toMatchObject({ code: "ORGANIZATION_NOT_FOUND" })

  // Valid member switching to a ready org by slug succeeds and updates session.
  const ok = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationSlug: "ready-slug" }, { cookie: alice.cookie })
  expect(ok.status).toBe(200)
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(alice.userId)).toEqual({ activeOrganizationId: "org_ready" })
})

test("API-key creation refuses non-members, low-privilege roles, missing orgs, provisioning orgs, and mismatched referenceId", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const owner = await enroll(env, "key-owner@example.com", "Owner")
  const admin = await enroll(env, "key-admin@example.com", "Admin")
  const member = await enroll(env, "key-member@example.com", "Member")
  const viewer = await enroll(env, "key-viewer@example.com", "Viewer")
  const outsider = await enroll(env, "key-out@example.com", "Outsider")
  const now = new Date().toISOString()
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('mk_adm', ?, ?, 'admin', ?)`, [owner.organizationId, admin.userId, now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('mk_mem', ?, ?, 'member', ?)`, [owner.organizationId, member.userId, now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('mk_view', ?, ?, 'viewer', ?)`, [owner.organizationId, viewer.userId, now])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_prov_k', 'Prov', 'prov-k', '{"norn.provisioning":true}', ?)`, [now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('mk_prov', 'org_prov_k', ?, 'owner', ?)`, [owner.userId, now])

  // Non-member, member, and viewer cannot create keys for owner's workspace.
  for (const caller of [outsider, member, viewer]) {
    const res = await mutate(env, "POST", "/api/auth/api-key/create", { name: "bad", organizationId: owner.organizationId }, { cookie: caller.cookie })
    expect(res.status).toBe(403)
  }

  // Non-existent org is refused 403.
  expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "ghost", organizationId: "org_missing" }, { cookie: owner.cookie })).status).toBe(403)

  // Provisioning org is refused 409 WORKSPACE_PROVISIONING.
  const provKey = await mutate(env, "POST", "/api/auth/api-key/create", { name: "early", organizationId: "org_prov_k" }, { cookie: owner.cookie })
  expect(provKey.status).toBe(409)
  expect(await provKey.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })

  // Blank organizationId and mismatched referenceId are refused 400.
  expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "blank", organizationId: "   " }, { cookie: owner.cookie })).status).toBe(400)
  const forgedRef = await mutate(env, "POST", "/api/auth/api-key/create", { name: "forged", organizationId: owner.organizationId, referenceId: "org_other" }, { cookie: owner.cookie })
  expect(forgedRef.status).toBe(400)
  expect(await forgedRef.json()).toMatchObject({ code: "INVALID_ORGANIZATION" })

  // Owner and admin on the ready workspace succeed; server-side auth.api.createApiKey enforces the same rules.
  expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "owner-svc", organizationId: owner.organizationId }, { cookie: owner.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "admin-svc", organizationId: owner.organizationId }, { cookie: admin.cookie })).status).toBe(200)
  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN })
  await expect(auth.api.createApiKey({ body: { name: "prov-api", organizationId: "org_prov_k" }, headers: new Headers({ cookie: owner.cookie }) })).rejects.toThrow()
  await expect(auth.api.createApiKey({ body: { name: "out-api", organizationId: owner.organizationId }, headers: new Headers({ cookie: outsider.cookie }) })).rejects.toThrow()
})

test("accepting an invitation to a provisioning workspace is refused at pre-check and at commit", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const owner = await enroll(env, "inv-owner@example.com", "Owner")
  const invitee = await enroll(env, "inv-guest@example.com", "Guest")
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [invitee.userId])
  const invId = crypto.randomUUID()
  const now = new Date().toISOString()
  db.run(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, ?, ?, 'member', 'pending', ?, ?, ?)`,
    [invId, owner.organizationId, "inv-guest@example.com", new Date(Date.now() + 3_600_000).toISOString(), now, owner.userId])

  // Pre-check refusal when org is provisioning.
  db.run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = ?`, [owner.organizationId])
  const blocked = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie })
  expect(blocked.status).toBe(409)
  expect(await blocked.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })

  // Commit-time race: org turns provisioning right before the atomic acceptance update.
  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = ?`, [owner.organizationId])
  const origPrepare = db.prepare.bind(db)
  let raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "invitation" set "status" = 'accepted'`)) {
      raced = true
      origPrepare(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = ?`).run(owner.organizationId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const racedRes = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(racedRes.status).toBe(409)
  expect(await racedRes.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
})
