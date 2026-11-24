import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { EXPECTED_ORG_HEADER, resolveActor } from "../actor.ts"
import { commitAcceptInvitation, commitActivateSession, commitClearActiveSession, commitCreateApiKey, createAuth, DISABLED_ORG_PATHS, WORKSPACE_ACCESS_DENIED_MESSAGE, type ProvisionDb } from "../auth.ts"
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

  // Mismatched organizationId + organizationSlug returns the same uniform 403 WORKSPACE_ACCESS_DENIED
  // without clearing activeOrganizationId.
  const mismatch = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: "org_ready", organizationSlug: "prov-slug" }, { cookie: alice.cookie })
  expect(mismatch.status).toBe(403)
  expect((await mismatch.json()) as Record<string, unknown>).toEqual({ code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE })
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(alice.userId)).toEqual({ activeOrganizationId: alice.organizationId })

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
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(invitee.userId)).toEqual({ activeOrganizationId: invitee.organizationId })
})

test("set-active leaks no org existence, never clears active org on refusal, and discovery endpoints are disabled", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const alice = await enroll(env, "enum-alice@example.com", "Alice")
  const bob = await enroll(env, "enum-bob@example.com", "Bob")
  const now = new Date().toISOString()
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_bob_ready', 'Bob Ready', 'bob-ready', '{"environmentId":"prod"}', ?)`, [now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_br', 'org_bob_ready', ?, 'owner', ?)`, [bob.userId, now])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_bob_prov', 'Bob Prov', 'bob-prov', '{"norn.provisioning":true}', ?)`, [now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_bp', 'org_bob_prov', ?, 'owner', ?)`, [bob.userId, now])

  // Every unauthorized / nonexistent / mismatched combination yields the exact same 403 payload and preserves activeOrganizationId.
  const bodies = [
    { organizationId: "org_does_not_exist" },
    { organizationId: "org_bob_ready" },
    { organizationId: "org_bob_prov" },
    { organizationSlug: "slug-does-not-exist" },
    { organizationSlug: "bob-ready" },
    { organizationSlug: "bob-prov" },
    { organizationId: alice.organizationId, organizationSlug: "bob-ready" },
    { organizationId: "org_bob_ready", organizationSlug: "slug-does-not-exist" },
    { organizationId: null, organizationSlug: "bob-ready" },
    { organizationId: "   " },
    { organizationSlug: "" },
  ]
  for (const body of bodies) {
    const res = await mutate(env, "POST", "/api/auth/organization/set-active", body, { cookie: alice.cookie })
    expect({ body, status: res.status }).toEqual({ body, status: 403 })
    expect((await res.json()) as Record<string, unknown>).toEqual({ code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE })
    expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(alice.userId)).toEqual({ activeOrganizationId: alice.organizationId })
  }

  // Disabled Better Auth org discovery endpoints refuse 404 across GET/POST, trailing slashes, and mixed case.
  for (const path of DISABLED_ORG_PATHS) {
    for (const variant of [path, `${path}/`, path.toUpperCase()]) {
      const getRes = await call(env, `/api/auth${variant}`, { headers: { cookie: alice.cookie } })
      expect({ method: "GET", variant, status: getRes.status }).toEqual({ method: "GET", variant, status: 404 })
      const postRes = await mutate(env, "POST", `/api/auth${variant}`, { slug: "bob-ready", organizationId: "org_bob_ready" }, { cookie: alice.cookie })
      expect({ method: "POST", variant, status: postRes.status }).toEqual({ method: "POST", variant, status: 404 })
    }
  }
})

test("public Better Auth set-active and accept-invitation enforce commit-time predicates and preserve refreshed session cookies", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod", billing: false })
  const owner = await enroll(env, "s4-owner@example.com", "Owner")
  const guest = await enroll(env, "s4-guest@example.com", "Guest")
  const now = new Date().toISOString()
  const agingExpiry = new Date(Date.now() + 3 * 24 * 3_600_000).toISOString()
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [guest.userId])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_s4', 'S4 Org', 's4-org', '{"environmentId":"prod"}', ?)`, [now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_s4', 'org_s4', ?, 'member', ?)`, [owner.userId, now])

  // 1. Public set-active concurrent membership revocation -> 403, session unchanged, refreshed Set-Cookie preserved on aging session.
  db.run(`update "session" set "expiresAt" = ? where "userId" = ?`, [agingExpiry, owner.userId])
  const origPrepare = db.prepare.bind(db)
  let raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "session" set "activeOrganizationId"`)) {
      raced = true
      origPrepare(`delete from "member" where "id" = 'm_s4'`).run()
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const revoked = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: "org_s4" }, { cookie: owner.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(revoked.status).toBe(403)
  expect(revoked.headers.getSetCookie().some((line) => line.includes("session_token="))).toBe(true)
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(owner.userId)).toEqual({ activeOrganizationId: owner.organizationId })

  // 2. Public set-active concurrent provisioning transition -> 409 WORKSPACE_PROVISIONING, session unchanged.
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_s4_2', 'org_s4', ?, 'member', ?)`, [owner.userId, now])
  raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "session" set "activeOrganizationId"`)) {
      raced = true
      origPrepare(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_s4'`).run()
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const provRace = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationSlug: "s4-org" }, { cookie: owner.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(provRace.status).toBe(409)
  expect(await provRace.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(owner.userId)).toEqual({ activeOrganizationId: owner.organizationId })

  // 3. Public accept-invitation (billing OFF): commit-time provisioning race -> 409, invitation stays pending, 0 members, refreshed cookie preserved.
  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = 'org_s4'`)
  const invId = crypto.randomUUID()
  db.run(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, 'org_s4', 's4-guest@example.com', 'member', 'pending', ?, ?, ?)`,
    [invId, new Date(Date.now() + 3_600_000).toISOString(), now, owner.userId])
  db.run(`update "session" set "expiresAt" = ? where "userId" = ?`, [agingExpiry, guest.userId])
  raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "invitation" set "status" = 'accepted'`)) {
      raced = true
      origPrepare(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_s4'`).run()
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const invRace = await mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: invId }, { cookie: guest.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(invRace.status).toBe(409)
  expect(await invRace.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(invRace.headers.getSetCookie().some((line) => line.includes("session_token="))).toBe(true)
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
  expect(db.query(`select count(*) as n from "member" where "organizationId" = 'org_s4' and "userId" = ?`).get(guest.userId)).toEqual({ n: 0 })
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(guest.userId)).toEqual({ activeOrganizationId: guest.organizationId })

  // 4. Ready org: public accept-invitation atomically accepts, inserts member, and activates org on session.
  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = 'org_s4'`)
  const accepted = await mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: invId }, { cookie: guest.cookie })
  expect(accepted.status).toBe(200)
  expect(await accepted.json()).toMatchObject({ invitation: { id: invId, status: "accepted" }, member: { organizationId: "org_s4", userId: guest.userId } })
  expect(db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(guest.userId)).toEqual({ activeOrganizationId: "org_s4" })
})

test("API-key creation enforces role and workspace readiness atomically at insertion and produces verifiable keys", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const owner = await enroll(env, "s6-owner@example.com", "Owner")
  const admin = await enroll(env, "s6-admin@example.com", "Admin")
  const now = new Date().toISOString()
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_s6_admin', ?, ?, 'admin', ?)`, [owner.organizationId, admin.userId, now])
  const countKeys = () => (db.query(`select count(*) as n from "apikey" where "referenceId" = ?`).get(owner.organizationId) as { n: number }).n
  const origPrepare = db.prepare.bind(db)

  // 1. Concurrent demotion of admin -> member right before INSERT writes 0 rows and returns 403.
  let raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`insert into "apikey"`)) {
      raced = true
      origPrepare(`update "member" set "role" = 'member' where "id" = 'm_s6_admin'`).run()
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const demoted = await mutate(env, "POST", "/api/auth/api-key/create", { name: "raced-demote", organizationId: owner.organizationId }, { cookie: admin.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(demoted.status).toBe(403)
  expect(countKeys()).toBe(0)

  // 2. Concurrent membership removal right before INSERT writes 0 rows and returns 403.
  db.run(`update "member" set "role" = 'admin' where "id" = 'm_s6_admin'`)
  raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`insert into "apikey"`)) {
      raced = true
      origPrepare(`delete from "member" where "id" = 'm_s6_admin'`).run()
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const removed = await mutate(env, "POST", "/api/auth/api-key/create", { name: "raced-remove", organizationId: owner.organizationId }, { cookie: admin.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(removed.status).toBe(403)
  expect(countKeys()).toBe(0)

  // 3. Concurrent provisioning transition right before INSERT writes 0 rows and returns 409 WORKSPACE_PROVISIONING.
  raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`insert into "apikey"`)) {
      raced = true
      origPrepare(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = ?`).run(owner.organizationId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const prov = await mutate(env, "POST", "/api/auth/api-key/create", { name: "raced-prov", organizationId: owner.organizationId }, { cookie: owner.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(prov.status).toBe(409)
  expect(await prov.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(countKeys()).toBe(0)

  // 4. Ready workspace: created key verifies via Better Auth verifyApiKey and resolves via resolveActor.
  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = ?`, [owner.organizationId])
  const created = await mutate(env, "POST", "/api/auth/api-key/create", { name: "valid-svc", organizationId: owner.organizationId }, { cookie: owner.cookie })
  expect(created.status).toBe(200)
  const payload = (await created.json()) as { id: string; key: string; referenceId: string }
  expect(payload.referenceId).toBe(owner.organizationId)
  expect(countKeys()).toBe(1)
  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN })
  const verified = await auth.api.verifyApiKey({ body: { key: payload.key } })
  expect(verified.valid).toBe(true)
  expect(verified.key?.referenceId).toBe(owner.organizationId)
  const actor = await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": payload.key } }), auth)
  expect(actor).toEqual({ actorKind: "service", subjectId: payload.id, orgId: owner.organizationId, role: "viewer", authMethod: "api_key" })

  // 5. Malformed request body (e.g. non-number expiresIn, non-string name, invalid metadata) is rejected 400 with 0 new rows.
  for (const badBody of [
    { name: "bad-exp", organizationId: owner.organizationId, expiresIn: "never" },
    { name: 123, organizationId: owner.organizationId },
    { name: "bad-meta", organizationId: owner.organizationId, metadata: "not-an-object" },
    { name: "unknown-field", organizationId: owner.organizationId, extra: true },
  ]) {
    const res = await mutate(env, "POST", "/api/auth/api-key/create", badBody, { cookie: owner.cookie })
    expect({ badBody, status: res.status }).toEqual({ badBody, status: 400 })
  }
  expect(countKeys()).toBe(1)

  // 6. Case- or space-tampered member roles never authorize key creation (exact case-sensitive GLOB matching).
  const tampered = await enroll(env, "s6-tampered@example.com", "Tampered")
  for (const role of ["OWNER", "Admin", "o w n e r", " owner ", "co-owner"]) {
    db.run(`delete from "member" where "userId" = ? and "organizationId" = ?`, [tampered.userId, owner.organizationId])
    db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values (?, ?, ?, ?, ?)`, [`m_t_${role}`, owner.organizationId, tampered.userId, role, now])
    const res = await mutate(env, "POST", "/api/auth/api-key/create", { name: "tampered", organizationId: owner.organizationId }, { cookie: tampered.cookie })
    expect({ role, status: res.status }).toEqual({ role, status: 403 })
  }
  expect(countKeys()).toBe(1)
})

test("revoked or expired sessions commit zero workspace changes across activate, accept-invitation, createWorkspace, and D1 batch adapter", async () => {
  const { db, env } = stage({ defaultEnvironmentId: "prod", billing: false })
  const owner = await enroll(env, "rev-owner@example.com", "Owner")
  const guest = await enroll(env, "rev-guest@example.com", "Guest")
  const now = new Date().toISOString()
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [guest.userId])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_rev', 'Rev Org', 'rev-org', '{"environmentId":"prod"}', ?)`, [now])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_rev_o', 'org_rev', ?, 'owner', ?)`, [owner.userId, now])
  const origPrepare = db.prepare.bind(db)

  // 1. Concurrent sign-out right before activateWorkspace -> 401 UNAUTHORIZED.
  let raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "session" set "activeOrganizationId"`)) {
      raced = true
      origPrepare(`delete from "session" where "userId" = ?`).run(owner.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const actRes = await mutate(env, "POST", "/api/workspaces/org_rev/activate", {}, { cookie: owner.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(actRes.status).toBe(401)

  // 2. Concurrent sign-out right before accept-invitation -> 401, invitation stays pending, 0 members inserted.
  const invId = crypto.randomUUID()
  db.run(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, 'org_rev', 'rev-guest@example.com', 'member', 'pending', ?, ?, ?)`,
    [invId, new Date(Date.now() + 3_600_000).toISOString(), now, owner.userId])
  raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`update "invitation" set "status" = 'accepted'`)) {
      raced = true
      origPrepare(`delete from "session" where "userId" = ?`).run(guest.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const accRes = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: guest.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(accRes.status).toBe(401)
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
  expect(db.query(`select count(*) as n from "member" where "organizationId" = 'org_rev' and "userId" = ?`).get(guest.userId)).toEqual({ n: 0 })

  // 3. Concurrent sign-out right before createWorkspace -> 401, 0 organizations & 0 members inserted.
  const creator = await enroll(env, "rev-creator@example.com", "Creator")
  raced = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!raced && sql.includes(`insert into "organization"`)) {
      raced = true
      origPrepare(`delete from "session" where "userId" = ?`).run(creator.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const createRes = await mutate(env, "POST", "/api/workspaces", { name: "Ghost", slug: "ghost-ws" }, { cookie: creator.cookie })
  db.prepare = origPrepare
  expect(raced).toBe(true)
  expect(createRes.status).toBe(401)
  expect(db.query(`select count(*) as n from "organization" where "slug" = 'ghost-ws'`).get()).toEqual({ n: 0 })

  // 4. Implicit active-organization mutation (POST /api/auth/organization/update without organizationId) requires x-norn-expected-org.
  const implicit = await enroll(env, "implicit-org@example.com", "Implicit")
  expect((await mutate(env, "POST", "/api/auth/organization/update", { data: { name: "Renamed" } }, { cookie: implicit.cookie })).status).toBe(400)
  expect((await mutate(env, "POST", "/api/auth/organization/update", { data: { name: "Renamed" } }, { cookie: implicit.cookie, [EXPECTED_ORG_HEADER]: "org_wrong" })).status).toBe(409)
  expect((await mutate(env, "POST", "/api/auth/organization/update", { data: { name: "Renamed" } }, { cookie: implicit.cookie, [EXPECTED_ORG_HEADER]: implicit.organizationId })).status).toBe(200)

  // 5. D1-shaped adapter (bind/first/all/run/batch) exercises commitActivateSession, commitAcceptInvitation, and commitCreateApiKey end to end.
  const d1Wrapper: ProvisionDb = {
    prepare(sql: string) {
      return {
        bind(...params: (string | number | null)[]) {
          return {
            sql, params,
            first: async <T>() => (db.query(sql).get(...params) as T | null) ?? null,
            all: async <T>() => ({ results: db.query(sql).all(...params) as T[] }),
            run: async () => { db.query(sql).run(...params); return { success: true } },
          }
        },
      }
    },
    batch: async (stmts: unknown[]) => {
      const results: { results: unknown[] }[] = []
      db.transaction(() => {
        for (const s of stmts as { sql: string; params: (string | number | null)[] }[]) {
          if (/\breturning\b/i.test(s.sql)) {
            results.push({ results: db.query(s.sql).all(...s.params) })
          } else {
            db.query(s.sql).run(...s.params)
            results.push({ results: [] })
          }
        }
      })()
      return results
    },
  }
  const d1User = await enroll(env, "d1-user@example.com", "D1")
  const d1Session = db.query(`select "token" from "session" where "userId" = ?`).get(d1User.userId) as { token: string }
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [d1User.userId])
  const d1Inv = crypto.randomUUID()
  db.run(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, 'org_rev', 'd1-user@example.com', 'admin', 'pending', ?, ?, ?)`,
    [d1Inv, new Date(Date.now() + 3_600_000).toISOString(), now, owner.userId])
  const d1Acc = await commitAcceptInvitation(d1Wrapper, {
    invitationId: d1Inv, userId: d1User.userId, userEmail: "d1-user@example.com", emailVerified: true,
    sessionToken: d1Session.token, billing: undefined, baseURL: ORIGIN,
  })
  expect(d1Acc.kind).toBe("accepted")
  const d1Act = await commitActivateSession(d1Wrapper, { token: d1Session.token, userId: d1User.userId, organizationId: d1User.organizationId })
  expect(d1Act?.activeOrganizationId).toBe(d1User.organizationId)
  // Clearing activeOrganizationId (organizationId: null) succeeds while session is live, and refuses once expired.
  const d1Cleared = await commitClearActiveSession(d1Wrapper, { token: d1Session.token, userId: d1User.userId })
  expect(d1Cleared).not.toBeNull()
  expect(d1Cleared?.activeOrganizationId).toBeNull()
  expect((db.query(`select "activeOrganizationId" from "session" where "token" = ?`).get(d1Session.token) as { activeOrganizationId: string | null }).activeOrganizationId).toBeNull()
  const d1Key = await commitCreateApiKey(d1Wrapper, { userId: d1User.userId, organizationId: "org_rev", sessionToken: d1Session.token, name: "d1-key" })
  expect(d1Key.kind).toBe("created")
  db.run(`update "session" set "expiresAt" = ? where "token" = ?`, [new Date(Date.now() - 10_000).toISOString(), d1Session.token])
  expect(await commitClearActiveSession(d1Wrapper, { token: d1Session.token, userId: d1User.userId })).toBeNull()
})

test("S4 null set-active refuses revoked sessions; S5 member/invitation mutations return uniform refusals without existence oracles", async () => {
  const { db, env } = stage()
  const alice = await enroll(env, "s5-alice@example.com", "Alice")
  const bob = await enroll(env, "s5-bob@example.com", "Bob")
  const carol = await enroll(env, "s5-carol@example.com", "Carol")

  // 1. set-active with { organizationId: null } refuses a session revoked between pre-check and write (401).
  const origPrepare = db.prepare.bind(db)
  let revokedOnNull = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!revokedOnNull && sql.toLowerCase().includes(`set "activeorganizationid" = null`)) {
      revokedOnNull = true
      origPrepare(`delete from "session" where "userId" = ?`).run(carol.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const nullRes = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: null }, { cookie: carol.cookie })
  db.prepare = origPrepare
  expect(revokedOnNull).toBe(true)
  expect(nullRes.status).toBe(401)

  // 2. Foreign member IDs vs missing member IDs return identical 400 MEMBER_NOT_FOUND on remove-member and update-member-role.
  const bobMemberId = (db.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(bob.userId, bob.organizationId) as { id: string }).id
  for (const path of ["/api/auth/organization/remove-member", "/api/auth/organization/update-member-role"]) {
    const missingBody = path.endsWith("remove-member")
      ? { memberIdOrEmail: "mem_does_not_exist", organizationId: alice.organizationId }
      : { memberId: "mem_does_not_exist", role: "member", organizationId: alice.organizationId }
    const foreignBody = path.endsWith("remove-member")
      ? { memberIdOrEmail: bobMemberId, organizationId: alice.organizationId }
      : { memberId: bobMemberId, role: "member", organizationId: alice.organizationId }
    const rMissing = await mutate(env, "POST", path, missingBody, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    const rForeign = await mutate(env, "POST", path, foreignBody, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    expect(rMissing.status).toBe(400)
    expect(rForeign.status).toBe(400)
    expect(await rForeign.json()).toEqual(await rMissing.json())
  }
  // Bob's membership is untouched.
  expect(db.query(`select "role" from "member" where "id" = ?`).get(bobMemberId)).toEqual({ role: "owner" })

  // 3. Foreign invitation IDs vs missing invitation IDs return identical 400 INVITATION_NOT_FOUND on cancel-invitation and reject-invitation.
  const bobInv = (await (await mutate(
    env,
    "POST",
    "/api/auth/organization/invite-member",
    { email: "invitee-for-bob@example.com", role: "member", organizationId: bob.organizationId },
    { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId },
  )).json()) as { id: string }

  const cancelMissing = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: "inv_does_not_exist" }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  const cancelForeign = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: bobInv.id }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(cancelMissing.status).toBe(400)
  expect(cancelForeign.status).toBe(400)
  expect(await cancelForeign.json()).toEqual(await cancelMissing.json())

  // Verify Alice's email so reject-invitation tests verified-recipient uniformity (unverified accounts are refused 403 below).
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [alice.userId])
  const rejectMissing = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: "inv_does_not_exist" }, { cookie: alice.cookie })
  const rejectForeign = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: bobInv.id }, { cookie: alice.cookie })
  expect(rejectMissing.status).toBe(400)
  expect(rejectForeign.status).toBe(400)
  expect(await rejectForeign.json()).toEqual(await rejectMissing.json())
  // Bob's invitation is still pending.
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(bobInv.id)).toEqual({ status: "pending" })

  // 4. R3-7: Non-admin callers cannot distinguish same-workspace member/invitation IDs from absent IDs.
  const lowPriv = await enroll(env, "s5-member@example.com", "LowPriv")
  // Remove lowPriv's own owner workspace so they hold NO admin/owner role anywhere, and join Alice's workspace as a plain member.
  db.run(`delete from "member" where "userId" = ?`, [lowPriv.userId])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_lowpriv', ?, ?, 'member', ?)`, [alice.organizationId, lowPriv.userId, new Date().toISOString()])
  db.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [alice.organizationId, lowPriv.userId])
  const aliceMemberId = (db.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(alice.userId, alice.organizationId) as { id: string }).id
  const aliceInv = (await (await mutate(
    env,
    "POST",
    "/api/auth/organization/invite-member",
    { email: "invitee-for-alice@example.com", role: "member", organizationId: alice.organizationId },
    { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId },
  )).json()) as { id: string }

  // update-member-role & remove-member: same-workspace real member ID vs absent ID return identical 403 FORBIDDEN.
  for (const path of ["/api/auth/organization/remove-member", "/api/auth/organization/update-member-role"]) {
    const bodyAbsent = path.endsWith("remove-member")
      ? { memberIdOrEmail: "mem_absent", organizationId: alice.organizationId }
      : { memberId: "mem_absent", role: "admin", organizationId: alice.organizationId }
    const bodyReal = path.endsWith("remove-member")
      ? { memberIdOrEmail: aliceMemberId, organizationId: alice.organizationId }
      : { memberId: aliceMemberId, role: "admin", organizationId: alice.organizationId }
    const rAbsent = await mutate(env, "POST", path, bodyAbsent, { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    const rReal = await mutate(env, "POST", path, bodyReal, { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    expect(rAbsent.status).toBe(403)
    expect(rReal.status).toBe(403)
    expect(await rReal.json()).toEqual(await rAbsent.json())
  }
  // cancel-invitation: non-admin gets identical 403 FORBIDDEN for real same-workspace invitation vs absent invitation.
  const lowCancelAbsent = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: "inv_absent" }, { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  const lowCancelReal = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(lowCancelAbsent.status).toBe(403)
  expect(lowCancelReal.status).toBe(403)
  expect(await lowCancelReal.json()).toEqual(await lowCancelAbsent.json())
  // A user who IS an admin in Bob's workspace but only a plain member in Alice's workspace gets uniform 400 INVITATION_NOT_FOUND for Alice's real invitation vs an absent one.
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_bob_in_alice', ?, ?, 'member', ?)`, [alice.organizationId, bob.userId, new Date().toISOString()])
  const bobCancelAbsent = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: "inv_absent" }, { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId })
  const bobCancelUnpriv = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId })
  expect(bobCancelAbsent.status).toBe(400)
  expect(bobCancelUnpriv.status).toBe(400)
  expect(await bobCancelUnpriv.json()).toEqual(await bobCancelAbsent.json())

  // 5. R4-5: Unused Better Auth GET /api/auth/account-info is disabled (404 NOT_FOUND) so no GET can refresh gateway tokens.
  for (const path of ["/api/auth/account-info", "/api/auth/account-info/", "/api/auth/Account-Info"]) {
    const res = await call(env, path, { headers: { cookie: alice.cookie } })
    expect({ path, status: res.status }).toEqual({ path, status: 404 })
  }

  // 6. R4-6: cancel-invitation and reject-invitation enforce status = 'pending' atomically at the write
  //    (even without relying on trigger 0012), leaving an accepted invitation untouched.
  db.run(`drop trigger if exists "invitation_status_final"`)
  db.run(`update "invitation" set "status" = 'accepted' where "id" = ?`, [aliceInv.id])
  const cancelAccepted = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(cancelAccepted.status).toBe(400)
  expect(await cancelAccepted.json()).toMatchObject({ code: "INVITATION_NOT_FOUND" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(aliceInv.id)).toEqual({ status: "accepted" })

  // 7. R5-3: reject-invitation requires a currently verified email both before lookup and atomically at the SQL write.
  const unverified = await enroll(env, "s5-unverified@example.com", "Unverified")
  const invForUnverified = (await (await mutate(
    env,
    "POST",
    "/api/auth/organization/invite-member",
    { email: "s5-unverified@example.com", role: "member", organizationId: alice.organizationId },
    { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId },
  )).json()) as { id: string }
  // Unverified caller gets uniform 403 for both their own invitation and an absent invitation on
  // BOTH accept-invitation and reject-invitation (no existence oracle), and invitation stays pending.
  const unvAcceptOwn = await mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: invForUnverified.id }, { cookie: unverified.cookie })
  const unvAcceptMissing = await mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: "inv_missing" }, { cookie: unverified.cookie })
  expect(unvAcceptOwn.status).toBe(403)
  expect(unvAcceptMissing.status).toBe(403)
  expect(await unvAcceptOwn.json()).toEqual(await unvAcceptMissing.json())
  const unvOwn = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: invForUnverified.id }, { cookie: unverified.cookie })
  const unvMissing = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: "inv_missing" }, { cookie: unverified.cookie })
  expect(unvOwn.status).toBe(403)
  expect(unvMissing.status).toBe(403)
  expect(await unvOwn.json()).toEqual(await unvMissing.json())
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invForUnverified.id)).toEqual({ status: "pending" })
  // Even if emailVerified flips to 0 right before the atomic UPDATE, the SQL write refuses and returns 403!
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [unverified.userId])
  let unverifiedMidFlight = false
  db.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!unverifiedMidFlight && sql.toLowerCase().includes(`set "status" = 'rejected'`)) {
      unverifiedMidFlight = true
      origPrepare(`update "user" set "emailVerified" = 0 where "id" = ?`).run(unverified.userId)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof db.prepare
  const racedReject = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: invForUnverified.id }, { cookie: unverified.cookie })
  db.prepare = origPrepare
  expect(unverifiedMidFlight).toBe(true)
  expect(racedReject.status).toBe(403)
  expect(await racedReject.json()).toMatchObject({ code: "EMAIL_VERIFICATION_REQUIRED_BEFORE_ACCEPTING_OR_REJECTING_INVITATION" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invForUnverified.id)).toEqual({ status: "pending" })
})
