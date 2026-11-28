import type { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { EXPECTED_ORG_HEADER, resolveActor } from "../actor.ts"
import { commitAcceptInvitation, commitActivateSession, commitClearActiveSession, commitCreateApiKey, commitRevokeApiKey, createAuth, DISABLED_ORG_PATHS, listWorkspaceApiKeys, WORKSPACE_ACCESS_DENIED_MESSAGE, type ProvisionDb } from "../auth.ts"
import { billingModeStatement } from "../billing.ts"
import type { Env } from "../index.ts"
import type { WorkspaceApiKeysDocument, WorkspacesDocument } from "../workspaces.ts"
import { cookieOf, createMigratedDb, createSqliteD1, createWorkerClient, createWorkerEnv, withSqlRace } from "./harness.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"

function stage(options: { host?: Fetcher; defaultEnvironmentId?: string; billing?: boolean } = {}) {
  const db = createMigratedDb("all")
  db.run(billingModeStatement(options.billing === true))
  return createWorkerEnv({
    db,
    origin: ORIGIN,
    assetsHtml: "studio",
    overrides: {
      ...options.host === undefined ? {} : { NORN_HOST: options.host },
      ...options.defaultEnvironmentId === undefined ? {} : { STUDIO_DEFAULT_ENVIRONMENT_ID: options.defaultEnvironmentId },
    },
  })
}

const stubHost = { fetch: async () => Response.json({ artifacts: [] }) } as unknown as Fetcher
const { call, mutate, enroll: enrollResponse } = createWorkerClient(ORIGIN)

async function enroll(env: Env, email: string, name = email.split("@")[0] ?? "User") {
  const signedIn = await enrollResponse(env, email, name)
  expect(signedIn.status).toBe(200)
  const db = env.AUTH_DB as unknown as Database
  const { id: userId } = db.query(`select "id" from "user" where "email" = ?`).get(email) as { id: string }
  const { organizationId } = db.query(`select "organizationId" from "member" where "userId" = ? order by "createdAt" asc limit 1`).get(userId) as { organizationId: string }
  return { cookie: cookieOf(signedIn), userId, organizationId }
}

const addOrg = (db: Database, id: string, slug: string, metadata: string, name = id, createdAt = new Date().toISOString()) => {
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values (?, ?, ?, ?, ?)`, [id, name, slug, metadata, createdAt])
}
const addMember = (db: Database, organizationId: string, userId: string, role: string, id = `m_${crypto.randomUUID()}`, createdAt = new Date().toISOString()) => {
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values (?, ?, ?, ?, ?)`, [id, organizationId, userId, role, createdAt])
  return id
}
const addInvitation = (db: Database, organizationId: string, email: string, inviterId: string, role = "member") => {
  const id = crypto.randomUUID()
  const now = new Date().toISOString()
  db.run(
    `insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, ?, ?, ?, 'pending', ?, ?, ?)`,
    [id, organizationId, email, role, new Date(Date.now() + 3_600_000).toISOString(), now, inviterId],
  )
  return id
}
const activeOrgOf = (db: Database, userId: string) =>
  (db.query(`select "activeOrganizationId" from "session" where "userId" = ?`).get(userId) as { activeOrganizationId: string | null } | null)?.activeOrganizationId
const countRows = (db: Database, table: string, where: string, ...params: string[]) =>
  (db.query(`select count(*) as n from "${table}" where ${where}`).get(...params) as { n: number }).n
const asProvisionDb = (db: Database, opts?: Parameters<typeof createSqliteD1>[1]) =>
  createSqliteD1(db, opts) as unknown as ProvisionDb

test("GET /api/workspaces lists only the caller's ready workspaces and omits provisioning orgs", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod", billing: true })
  expect((await call(env, "/api/workspaces")).status).toBe(401)
  const alice = await enroll(env, "alice@example.com", "Alice")
  const bob = await enroll(env, "bob@example.com", "Bob")
  addMember(db, bob.organizationId, alice.userId, "viewer", "m_v", new Date(Date.now() + 1000).toISOString())
  addOrg(db, "org_prov", "provisioning", '{"norn.provisioning":true}', "Provisioning", new Date(Date.now() + 2000).toISOString())
  addMember(db, "org_prov", alice.userId, "owner", "m_prov", new Date(Date.now() + 2000).toISOString())

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
  addMember(db, bob.organizationId, alice.userId, "member", "m_ab")
  addOrg(db, "org_prov", "prov", '{"norn.provisioning":true}', "Prov")
  addMember(db, "org_prov", alice.userId, "owner", "m_ap")

  expect((await mutate(env, "POST", `/api/workspaces/${bob.organizationId}/activate`, {}, { cookie: alice.cookie })).status).toBe(200)
  expect((await mutate(env, "POST", `/api/workspaces/${bob.organizationId}/activate`, {}, { cookie: outsider.cookie })).status).toBe(403)
  const prov = await mutate(env, "POST", "/api/workspaces/org_prov/activate", {}, { cookie: alice.cookie })
  expect(prov.status).toBe(409)
  expect(await prov.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })

  expect((await mutate(env, "POST", `/api/workspaces/${alice.organizationId}/activate`, {}, { cookie: alice.cookie })).status).toBe(200)
  const { result: failed, raced } = await withSqlRace(
    db,
    `update "session" set "activeOrganizationId"`,
    (run) => run(`delete from "member" where "organizationId" = ? and "userId" = ?`, bob.organizationId, alice.userId),
    () => mutate(env, "POST", `/api/workspaces/${bob.organizationId}/activate`, {}, { cookie: alice.cookie }),
  )
  expect(raced).toBe(true)
  expect(failed.status).toBe(403)
  expect(activeOrgOf(db, alice.userId)).toBe(alice.organizationId)
})

test("POST /api/workspaces refuses self-serve creation when NORN_HOST is connected", async () => {
  for (const billing of [false, true]) {
    const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod", billing })
    const user = await enroll(env, `hosted-${billing}@example.com`, "Hosted")
    const created = await mutate(env, "POST", "/api/workspaces", { name: "Blocked", slug: "blocked" }, { cookie: user.cookie })
    expect(created.status).toBe(403)
    expect(await created.json()).toMatchObject({ code: "WORKSPACE_CREATION_UNAVAILABLE" })
    expect(countRows(db, "organization", `"slug" = ?`, "blocked")).toBe(0)
  }
})

test("direct Better Auth organization/create and organization/delete are disabled for normal users", async () => {
  for (const host of [undefined, stubHost]) {
    const { db, env } = stage({ host, defaultEnvironmentId: "prod", billing: false })
    const owner = await enroll(env, "direct@example.com", "Owner")
    for (const path of ["/api/auth/organization/create", "/api/auth/organization/create/", "/api/auth/Organization/Create"]) {
      const created = await mutate(env, "POST", path, { name: "Sneaky", slug: "sneaky-org" }, { cookie: owner.cookie })
      expect([403, 404]).toContain(created.status)
      if (created.status === 403) expect(await created.json()).toMatchObject({ code: "ORGANIZATION_CREATE_DISABLED" })
    }
    expect(countRows(db, "organization", `"slug" = ?`, "sneaky-org")).toBe(0)

    db.run(`insert into "apikey" ("id", "configId", "name", "referenceId", "key", "createdAt", "updatedAt") values ('k_keep', 'default', 'svc', ?, 'hash', '2026', '2026')`, [owner.organizationId])
    const deleted = await mutate(env, "POST", "/api/auth/organization/delete", { organizationId: owner.organizationId }, { cookie: owner.cookie })
    expect(deleted.status).toBe(403)
    expect(await deleted.json()).toMatchObject({ code: "ORGANIZATION_DELETE_DISABLED" })
    expect(countRows(db, "organization", `"id" = ?`, owner.organizationId)).toBe(1)
    expect(countRows(db, "apikey", `"id" = ?`, "k_keep")).toBe(1)
  }

  const billed = stage({ host: stubHost, billing: true })
  const billedOwner = await enroll(billed.env, "billed-direct@example.com", "Owner")
  const billedCreate = await mutate(billed.env, "POST", "/api/auth/organization/create", { name: "Sneaky", slug: "sneaky-billed" }, { cookie: billedOwner.cookie })
  expect(billedCreate.status).toBe(404)
  expect(await billedCreate.json()).toMatchObject({ code: "USE_STUDIO_ROUTE" })
  const billedDelete = await mutate(billed.env, "POST", "/api/auth/organization/delete", { organizationId: billedOwner.organizationId }, { cookie: billedOwner.cookie })
  expect(billedDelete.status).toBe(403)
  expect(await billedDelete.json()).toMatchObject({ code: "ORGANIZATION_DELETE_DISABLED" })
})

test("set-active validates slug/id, provisioning state, leaks no org existence, and discovery endpoints are disabled", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const alice = await enroll(env, "enum-alice@example.com", "Alice")
  const bob = await enroll(env, "enum-bob@example.com", "Bob")
  addOrg(db, "org_ready", "ready-slug", '{"environmentId":"prod"}', "Ready Org")
  addMember(db, "org_ready", alice.userId, "member", "m_ready")
  addOrg(db, "org_prov", "prov-slug", '{"norn.provisioning":true}', "Prov Org")
  addMember(db, "org_prov", alice.userId, "owner", "m_prov")
  addOrg(db, "org_bob_ready", "bob-ready", '{"environmentId":"prod"}', "Bob Ready")
  addMember(db, "org_bob_ready", bob.userId, "owner", "m_br")
  addOrg(db, "org_bob_prov", "bob-prov", '{"norn.provisioning":true}', "Bob Prov")
  addMember(db, "org_bob_prov", bob.userId, "owner", "m_bp")

  for (const body of [{ organizationId: "org_prov" }, { organizationSlug: "prov-slug" }]) {
    const res = await mutate(env, "POST", "/api/auth/organization/set-active", body, { cookie: alice.cookie })
    expect({ body, status: res.status }).toEqual({ body, status: 409 })
    expect(await res.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  }

  for (const body of [
    { organizationId: "org_prov" },
    { organizationSlug: "prov-slug" },
    { organizationId: "org_ready" },
    { organizationSlug: "ready-slug" },
  ]) {
    const res = await mutate(env, "POST", "/api/auth/organization/set-active", body, { cookie: bob.cookie })
    expect({ body, status: res.status }).toEqual({ body, status: 403 })
    expect((await res.json()) as Record<string, unknown>).toEqual({ code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE })
    expect(activeOrgOf(db, bob.userId)).toBe(bob.organizationId)
  }

  for (const body of [
    { organizationId: "org_does_not_exist" },
    { organizationId: "org_bob_ready" },
    { organizationId: "org_bob_prov" },
    { organizationSlug: "slug-does-not-exist" },
    { organizationSlug: "bob-ready" },
    { organizationSlug: "bob-prov" },
    { organizationId: "org_ready", organizationSlug: "prov-slug" },
    { organizationId: alice.organizationId, organizationSlug: "bob-ready" },
    { organizationId: "org_bob_ready", organizationSlug: "slug-does-not-exist" },
    { organizationId: null, organizationSlug: "bob-ready" },
    { organizationId: "   " },
    { organizationSlug: "" },
  ]) {
    const res = await mutate(env, "POST", "/api/auth/organization/set-active", body, { cookie: alice.cookie })
    expect({ body, status: res.status }).toEqual({ body, status: 403 })
    expect((await res.json()) as Record<string, unknown>).toEqual({ code: "WORKSPACE_ACCESS_DENIED", message: WORKSPACE_ACCESS_DENIED_MESSAGE })
    expect(activeOrgOf(db, alice.userId)).toBe(alice.organizationId)
  }

  const ok = await mutate(env, "POST", "/api/auth/organization/set-active", { organizationSlug: "ready-slug" }, { cookie: alice.cookie })
  expect(ok.status).toBe(200)
  expect(activeOrgOf(db, alice.userId)).toBe("org_ready")

  for (const path of DISABLED_ORG_PATHS) {
    for (const variant of [path, `${path}/`, path.toUpperCase()]) {
      expect({ method: "GET", variant, status: (await call(env, `/api/auth${variant}`, { headers: { cookie: alice.cookie } })).status }).toEqual({ method: "GET", variant, status: 404 })
      expect({ method: "POST", variant, status: (await mutate(env, "POST", `/api/auth${variant}`, { slug: "bob-ready", organizationId: "org_bob_ready" }, { cookie: alice.cookie })).status }).toEqual({ method: "POST", variant, status: 404 })
    }
  }
})

test("API-key creation refuses non-members, low-privilege roles, missing orgs, provisioning orgs, and mismatched referenceId", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const owner = await enroll(env, "key-owner@example.com", "Owner")
  const admin = await enroll(env, "key-admin@example.com", "Admin")
  const member = await enroll(env, "key-member@example.com", "Member")
  const viewer = await enroll(env, "key-viewer@example.com", "Viewer")
  const outsider = await enroll(env, "key-out@example.com", "Outsider")
  addMember(db, owner.organizationId, admin.userId, "admin", "mk_adm")
  addMember(db, owner.organizationId, member.userId, "member", "mk_mem")
  addMember(db, owner.organizationId, viewer.userId, "viewer", "mk_view")
  addOrg(db, "org_prov_k", "prov-k", '{"norn.provisioning":true}', "Prov")
  addMember(db, "org_prov_k", owner.userId, "owner", "mk_prov")

  for (const caller of [outsider, member, viewer]) {
    expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "bad", organizationId: owner.organizationId }, { cookie: caller.cookie })).status).toBe(403)
  }
  expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "ghost", organizationId: "org_missing" }, { cookie: owner.cookie })).status).toBe(403)

  const provKey = await mutate(env, "POST", "/api/auth/api-key/create", { name: "early", organizationId: "org_prov_k" }, { cookie: owner.cookie })
  expect(provKey.status).toBe(409)
  expect(await provKey.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })

  expect((await mutate(env, "POST", "/api/auth/api-key/create", { name: "blank", organizationId: "   " }, { cookie: owner.cookie })).status).toBe(400)
  const forgedRef = await mutate(env, "POST", "/api/auth/api-key/create", { name: "forged", organizationId: owner.organizationId, referenceId: "org_other" }, { cookie: owner.cookie })
  expect(forgedRef.status).toBe(400)
  expect(await forgedRef.json()).toMatchObject({ code: "INVALID_ORGANIZATION" })

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
  const invId = addInvitation(db, owner.organizationId, "inv-guest@example.com", owner.userId)

  db.run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = ?`, [owner.organizationId])
  const blocked = await mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie })
  expect(blocked.status).toBe(409)
  expect(await blocked.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })

  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = ?`, [owner.organizationId])
  const { result: racedRes, raced } = await withSqlRace(
    db,
    `update "invitation" set "status" = 'accepted'`,
    (run) => run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = ?`, owner.organizationId),
    () => mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: invitee.cookie }),
  )
  expect(raced).toBe(true)
  expect(racedRes.status).toBe(409)
  expect(await racedRes.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
  expect(activeOrgOf(db, invitee.userId)).toBe(invitee.organizationId)
})

test("public Better Auth set-active and accept-invitation enforce commit-time predicates and preserve refreshed session cookies", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod", billing: false })
  const owner = await enroll(env, "s4-owner@example.com", "Owner")
  const guest = await enroll(env, "s4-guest@example.com", "Guest")
  const agingExpiry = new Date(Date.now() + 3 * 24 * 3_600_000).toISOString()
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [guest.userId])
  addOrg(db, "org_s4", "s4-org", '{"environmentId":"prod"}', "S4 Org")
  addMember(db, "org_s4", owner.userId, "member", "m_s4")

  db.run(`update "session" set "expiresAt" = ? where "userId" = ?`, [agingExpiry, owner.userId])
  const { result: revoked, raced: r1 } = await withSqlRace(
    db,
    `update "session" set "activeOrganizationId"`,
    (run) => run(`delete from "member" where "id" = 'm_s4'`),
    () => mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: "org_s4" }, { cookie: owner.cookie }),
  )
  expect(r1).toBe(true)
  expect(revoked.status).toBe(403)
  expect(revoked.headers.getSetCookie().some((line) => line.includes("session_token="))).toBe(true)
  expect(activeOrgOf(db, owner.userId)).toBe(owner.organizationId)

  addMember(db, "org_s4", owner.userId, "member", "m_s4_2")
  const { result: provRace, raced: r2 } = await withSqlRace(
    db,
    `update "session" set "activeOrganizationId"`,
    (run) => run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_s4'`),
    () => mutate(env, "POST", "/api/auth/organization/set-active", { organizationSlug: "s4-org" }, { cookie: owner.cookie }),
  )
  expect(r2).toBe(true)
  expect(provRace.status).toBe(409)
  expect(await provRace.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(activeOrgOf(db, owner.userId)).toBe(owner.organizationId)

  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = 'org_s4'`)
  const invId = addInvitation(db, "org_s4", "s4-guest@example.com", owner.userId)
  db.run(`update "session" set "expiresAt" = ? where "userId" = ?`, [agingExpiry, guest.userId])
  const { result: invRace, raced: r3 } = await withSqlRace(
    db,
    `update "invitation" set "status" = 'accepted'`,
    (run) => run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_s4'`),
    () => mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: invId }, { cookie: guest.cookie }),
  )
  expect(r3).toBe(true)
  expect(invRace.status).toBe(409)
  expect(await invRace.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(invRace.headers.getSetCookie().some((line) => line.includes("session_token="))).toBe(true)
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
  expect(countRows(db, "member", `"organizationId" = 'org_s4' and "userId" = ?`, guest.userId)).toBe(0)
  expect(activeOrgOf(db, guest.userId)).toBe(guest.organizationId)

  db.run(`update "organization" set "metadata" = '{"environmentId":"prod"}' where "id" = 'org_s4'`)
  const accepted = await mutate(env, "POST", "/api/auth/organization/accept-invitation", { invitationId: invId }, { cookie: guest.cookie })
  expect(accepted.status).toBe(200)
  expect(await accepted.json()).toMatchObject({ invitation: { id: invId, status: "accepted" }, member: { organizationId: "org_s4", userId: guest.userId } })
  expect(activeOrgOf(db, guest.userId)).toBe("org_s4")
})

test("API-key creation enforces role and workspace readiness atomically at insertion and produces verifiable keys", async () => {
  const { db, env } = stage({ host: stubHost, defaultEnvironmentId: "prod" })
  const owner = await enroll(env, "s6-owner@example.com", "Owner")
  const admin = await enroll(env, "s6-admin@example.com", "Admin")
  addMember(db, owner.organizationId, admin.userId, "admin", "m_s6_admin")
  const countKeys = () => countRows(db, "apikey", `"referenceId" = ?`, owner.organizationId)

  const { result: demoted, raced: r1 } = await withSqlRace(
    db,
    `insert into "apikey"`,
    (run) => run(`update "member" set "role" = 'member' where "id" = 'm_s6_admin'`),
    () => mutate(env, "POST", "/api/auth/api-key/create", { name: "raced-demote", organizationId: owner.organizationId }, { cookie: admin.cookie }),
  )
  expect(r1).toBe(true)
  expect(demoted.status).toBe(403)
  expect(countKeys()).toBe(0)

  db.run(`update "member" set "role" = 'admin' where "id" = 'm_s6_admin'`)
  const { result: removed, raced: r2 } = await withSqlRace(
    db,
    `insert into "apikey"`,
    (run) => run(`delete from "member" where "id" = 'm_s6_admin'`),
    () => mutate(env, "POST", "/api/auth/api-key/create", { name: "raced-remove", organizationId: owner.organizationId }, { cookie: admin.cookie }),
  )
  expect(r2).toBe(true)
  expect(removed.status).toBe(403)
  expect(countKeys()).toBe(0)

  const { result: prov, raced: r3 } = await withSqlRace(
    db,
    `insert into "apikey"`,
    (run) => run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = ?`, owner.organizationId),
    () => mutate(env, "POST", "/api/auth/api-key/create", { name: "raced-prov", organizationId: owner.organizationId }, { cookie: owner.cookie }),
  )
  expect(r3).toBe(true)
  expect(prov.status).toBe(409)
  expect(await prov.json()).toMatchObject({ code: "WORKSPACE_PROVISIONING" })
  expect(countKeys()).toBe(0)

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
  expect(await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": payload.key } }), auth)).toEqual({
    actorKind: "service", subjectId: payload.id, orgId: owner.organizationId, role: "viewer", authMethod: "api_key",
  })

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

  const tampered = await enroll(env, "s6-tampered@example.com", "Tampered")
  for (const role of ["OWNER", "Admin", "o w n e r", " owner ", "co-owner"]) {
    db.run(`delete from "member" where "userId" = ? and "organizationId" = ?`, [tampered.userId, owner.organizationId])
    addMember(db, owner.organizationId, tampered.userId, role, `m_t_${role}`)
    const res = await mutate(env, "POST", "/api/auth/api-key/create", { name: "tampered", organizationId: owner.organizationId }, { cookie: tampered.cookie })
    expect({ role, status: res.status }).toEqual({ role, status: 403 })
  }
  expect(countKeys()).toBe(1)
})

test("revoked or expired sessions commit zero workspace changes across activate, accept-invitation, createWorkspace, and D1 batch adapter", async () => {
  const { db, env } = stage({ defaultEnvironmentId: "prod", billing: false })
  const owner = await enroll(env, "rev-owner@example.com", "Owner")
  const guest = await enroll(env, "rev-guest@example.com", "Guest")
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [guest.userId])
  addOrg(db, "org_rev", "rev-org", '{"environmentId":"prod"}', "Rev Org")
  addMember(db, "org_rev", owner.userId, "owner", "m_rev_o")

  const { result: actRes, raced: r1 } = await withSqlRace(
    db,
    `update "session" set "activeOrganizationId"`,
    (run) => run(`delete from "session" where "userId" = ?`, owner.userId),
    () => mutate(env, "POST", "/api/workspaces/org_rev/activate", {}, { cookie: owner.cookie }),
  )
  expect(r1).toBe(true)
  expect(actRes.status).toBe(401)

  const invId = addInvitation(db, "org_rev", "rev-guest@example.com", owner.userId)
  const { result: accRes, raced: r2 } = await withSqlRace(
    db,
    `update "invitation" set "status" = 'accepted'`,
    (run) => run(`delete from "session" where "userId" = ?`, guest.userId),
    () => mutate(env, "POST", "/api/workspaces/invitations/accept", { invitationId: invId }, { cookie: guest.cookie }),
  )
  expect(r2).toBe(true)
  expect(accRes.status).toBe(401)
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invId)).toEqual({ status: "pending" })
  expect(countRows(db, "member", `"organizationId" = 'org_rev' and "userId" = ?`, guest.userId)).toBe(0)

  const creator = await enroll(env, "rev-creator@example.com", "Creator")
  const { result: createRes, raced: r3 } = await withSqlRace(
    db,
    `insert into "organization"`,
    (run) => run(`delete from "session" where "userId" = ?`, creator.userId),
    () => mutate(env, "POST", "/api/workspaces", { name: "Ghost", slug: "ghost-ws" }, { cookie: creator.cookie }),
  )
  expect(r3).toBe(true)
  expect(createRes.status).toBe(401)
  expect(countRows(db, "organization", `"slug" = ?`, "ghost-ws")).toBe(0)

  const implicit = await enroll(env, "implicit-org@example.com", "Implicit")
  expect((await mutate(env, "POST", "/api/auth/organization/update", { data: { name: "Renamed" } }, { cookie: implicit.cookie })).status).toBe(400)
  expect((await mutate(env, "POST", "/api/auth/organization/update", { data: { name: "Renamed" } }, { cookie: implicit.cookie, [EXPECTED_ORG_HEADER]: "org_wrong" })).status).toBe(409)
  expect((await mutate(env, "POST", "/api/auth/organization/update", { data: { name: "Renamed" } }, { cookie: implicit.cookie, [EXPECTED_ORG_HEADER]: implicit.organizationId })).status).toBe(200)

  const d1Wrapper = asProvisionDb(db)
  const d1User = await enroll(env, "d1-user@example.com", "D1")
  const d1Session = db.query(`select "token" from "session" where "userId" = ?`).get(d1User.userId) as { token: string }
  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [d1User.userId])
  const d1Inv = addInvitation(db, "org_rev", "d1-user@example.com", owner.userId, "admin")
  expect((await commitAcceptInvitation(d1Wrapper, {
    invitationId: d1Inv, userId: d1User.userId, userEmail: "d1-user@example.com", emailVerified: true,
    sessionToken: d1Session.token, billing: undefined, baseURL: ORIGIN,
  })).kind).toBe("accepted")
  expect((await commitActivateSession(d1Wrapper, { token: d1Session.token, userId: d1User.userId, organizationId: d1User.organizationId }))?.activeOrganizationId).toBe(d1User.organizationId)
  const d1Cleared = await commitClearActiveSession(d1Wrapper, { token: d1Session.token, userId: d1User.userId })
  expect(d1Cleared?.activeOrganizationId).toBeNull()
  expect((db.query(`select "activeOrganizationId" from "session" where "token" = ?`).get(d1Session.token) as { activeOrganizationId: string | null }).activeOrganizationId).toBeNull()
  expect((await commitCreateApiKey(d1Wrapper, { userId: d1User.userId, organizationId: "org_rev", sessionToken: d1Session.token, name: "d1-key" })).kind).toBe("created")
  db.run(`update "session" set "expiresAt" = ? where "token" = ?`, [new Date(Date.now() - 10_000).toISOString(), d1Session.token])
  expect(await commitClearActiveSession(d1Wrapper, { token: d1Session.token, userId: d1User.userId })).toBeNull()
})

test("S4 null set-active refuses revoked sessions; S5 member/invitation mutations return uniform refusals without existence oracles", async () => {
  const { db, env } = stage()
  const alice = await enroll(env, "s5-alice@example.com", "Alice")
  const bob = await enroll(env, "s5-bob@example.com", "Bob")
  const carol = await enroll(env, "s5-carol@example.com", "Carol")

  const { result: nullRes, raced: revokedOnNull } = await withSqlRace(
    db,
    `set "activeorganizationid" = null`,
    (run) => run(`delete from "session" where "userId" = ?`, carol.userId),
    () => mutate(env, "POST", "/api/auth/organization/set-active", { organizationId: null }, { cookie: carol.cookie }),
  )
  expect(revokedOnNull).toBe(true)
  expect(nullRes.status).toBe(401)

  const bobMemberId = (db.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(bob.userId, bob.organizationId) as { id: string }).id
  for (const path of ["/api/auth/organization/remove-member", "/api/auth/organization/update-member-role"]) {
    const bodyFor = (id: string) => path.endsWith("remove-member")
      ? { memberIdOrEmail: id, organizationId: alice.organizationId }
      : { memberId: id, role: "member", organizationId: alice.organizationId }
    const rMissing = await mutate(env, "POST", path, bodyFor("mem_does_not_exist"), { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    const rForeign = await mutate(env, "POST", path, bodyFor(bobMemberId), { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    expect(rMissing.status).toBe(400)
    expect(rForeign.status).toBe(400)
    expect(await rForeign.json()).toEqual(await rMissing.json())
  }
  expect(db.query(`select "role" from "member" where "id" = ?`).get(bobMemberId)).toEqual({ role: "owner" })

  const bobInv = (await (await mutate(env, "POST", "/api/auth/organization/invite-member", { email: "invitee-for-bob@example.com", role: "member", organizationId: bob.organizationId }, { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId })).json()) as { id: string }

  const cancelMissing = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: "inv_does_not_exist" }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  const cancelForeign = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: bobInv.id }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(cancelMissing.status).toBe(400)
  expect(cancelForeign.status).toBe(400)
  expect(await cancelForeign.json()).toEqual(await cancelMissing.json())

  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [alice.userId])
  const rejectMissing = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: "inv_does_not_exist" }, { cookie: alice.cookie })
  const rejectForeign = await mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: bobInv.id }, { cookie: alice.cookie })
  expect(rejectMissing.status).toBe(400)
  expect(rejectForeign.status).toBe(400)
  expect(await rejectForeign.json()).toEqual(await rejectMissing.json())
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(bobInv.id)).toEqual({ status: "pending" })

  const lowPriv = await enroll(env, "s5-member@example.com", "LowPriv")
  db.run(`delete from "member" where "userId" = ?`, [lowPriv.userId])
  addMember(db, alice.organizationId, lowPriv.userId, "member", "m_lowpriv")
  db.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [alice.organizationId, lowPriv.userId])
  const aliceMemberId = (db.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(alice.userId, alice.organizationId) as { id: string }).id
  const aliceInv = (await (await mutate(env, "POST", "/api/auth/organization/invite-member", { email: "invitee-for-alice@example.com", role: "member", organizationId: alice.organizationId }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })).json()) as { id: string }

  for (const path of ["/api/auth/organization/remove-member", "/api/auth/organization/update-member-role"]) {
    const bodyFor = (id: string) => path.endsWith("remove-member")
      ? { memberIdOrEmail: id, organizationId: alice.organizationId }
      : { memberId: id, role: "admin", organizationId: alice.organizationId }
    const rAbsent = await mutate(env, "POST", path, bodyFor("mem_absent"), { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    const rReal = await mutate(env, "POST", path, bodyFor(aliceMemberId), { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
    expect(rAbsent.status).toBe(403)
    expect(rReal.status).toBe(403)
    expect(await rReal.json()).toEqual(await rAbsent.json())
  }

  const lowCancelAbsent = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: "inv_absent" }, { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  const lowCancelReal = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: lowPriv.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(lowCancelAbsent.status).toBe(403)
  expect(lowCancelReal.status).toBe(403)
  expect(await lowCancelReal.json()).toEqual(await lowCancelAbsent.json())

  addMember(db, alice.organizationId, bob.userId, "member", "m_bob_in_alice")
  const bobCancelAbsent = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: "inv_absent" }, { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId })
  const bobCancelUnpriv = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId })
  expect(bobCancelAbsent.status).toBe(400)
  expect(bobCancelUnpriv.status).toBe(400)
  expect(await bobCancelUnpriv.json()).toEqual(await bobCancelAbsent.json())

  for (const path of ["/api/auth/account-info", "/api/auth/account-info/", "/api/auth/Account-Info"]) {
    expect({ path, status: (await call(env, path, { headers: { cookie: alice.cookie } })).status }).toEqual({ path, status: 404 })
  }

  db.run(`drop trigger if exists "invitation_status_final"`)
  db.run(`update "invitation" set "status" = 'accepted' where "id" = ?`, [aliceInv.id])
  const cancelAccepted = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(cancelAccepted.status).toBe(400)
  expect(await cancelAccepted.json()).toMatchObject({ code: "INVITATION_NOT_FOUND" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(aliceInv.id)).toEqual({ status: "accepted" })

  const unverified = await enroll(env, "s5-unverified@example.com", "Unverified")
  const invForUnverified = (await (await mutate(env, "POST", "/api/auth/organization/invite-member", { email: "s5-unverified@example.com", role: "member", organizationId: alice.organizationId }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })).json()) as { id: string }
  for (const path of ["/api/auth/organization/accept-invitation", "/api/auth/organization/reject-invitation"]) {
    const rOwn = await mutate(env, "POST", path, { invitationId: invForUnverified.id }, { cookie: unverified.cookie })
    const rMissing = await mutate(env, "POST", path, { invitationId: "inv_missing" }, { cookie: unverified.cookie })
    expect(rOwn.status).toBe(403)
    expect(rMissing.status).toBe(403)
    expect(await rOwn.json()).toEqual(await rMissing.json())
  }
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invForUnverified.id)).toEqual({ status: "pending" })

  db.run(`update "user" set "emailVerified" = 1 where "id" = ?`, [unverified.userId])
  const { result: racedReject, raced: unverifiedMidFlight } = await withSqlRace(
    db,
    `set "status" = 'rejected'`,
    (run) => run(`update "user" set "emailVerified" = 0 where "id" = ?`, unverified.userId),
    () => mutate(env, "POST", "/api/auth/organization/reject-invitation", { invitationId: invForUnverified.id }, { cookie: unverified.cookie }),
  )
  expect(unverifiedMidFlight).toBe(true)
  expect(racedReject.status).toBe(403)
  expect(await racedReject.json()).toMatchObject({ code: "EMAIL_VERIFICATION_REQUIRED_BEFORE_ACCEPTING_OR_REJECTING_INVITATION" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(invForUnverified.id)).toEqual({ status: "pending" })

  for (const path of ["/api-key/get", "/api-key/list", "/api-key/update", "/api-key/delete", "/api-key/delete-all-expired-api-keys"]) {
    for (const variant of [path, `${path}/`, path.toUpperCase()]) {
      expect({ method: "GET", variant, status: (await call(env, `/api/auth${variant}?id=key_1&organizationId=${alice.organizationId}`, { headers: { cookie: alice.cookie } })).status }).toEqual({ method: "GET", variant, status: 404 })
      expect({ method: "POST", variant, status: (await mutate(env, "POST", `/api/auth${variant}`, { keyId: "key_1" }, { cookie: alice.cookie })).status }).toEqual({ method: "POST", variant, status: 404 })
    }
  }
  const wrongScopeKey = await mutate(env, "POST", "/api/auth/api-key/create", { name: "mismatched-scope", organizationId: alice.organizationId }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId })
  expect(wrongScopeKey.status).toBe(409)
  expect(await wrongScopeKey.json()).toMatchObject({ code: "WORKSPACE_CHANGED" })

  addMember(db, bob.organizationId, alice.userId, "admin", "m_alice_in_bob")
  const crossOrgCancel = await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: bobInv.id }, { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })
  expect(crossOrgCancel.status).toBe(400)
  expect(await crossOrgCancel.json()).toMatchObject({ code: "INVITATION_NOT_FOUND" })
  expect(db.query(`select "status" from "invitation" where "id" = ?`).get(bobInv.id)).toEqual({ status: "pending" })
  expect((await mutate(env, "POST", "/api/auth/organization/cancel-invitation", { invitationId: aliceInv.id }, { cookie: bob.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId })).status).toBe(403)
})

test("GET /api/workspaces/:orgId/api-keys and DELETE /api/workspaces/:orgId/api-keys/:keyId enforce workspace authz, zero side effects, and atomic revocation (#1859)", async () => {
  const { db, env } = stage()
  const alice = await enroll(env, "ak-alice@example.com", "Alice")
  const bob = await enroll(env, "ak-bob@example.com", "Bob")
  const memberUser = await enroll(env, "ak-member@example.com", "Member")
  const now = new Date().toISOString()

  addMember(db, alice.organizationId, memberUser.userId, "member", "m_member_alice")
  addOrg(db, "org_ak_prov", "ak-prov", JSON.stringify({ "norn.provisioning": true }), "Prov")
  addMember(db, "org_ak_prov", alice.userId, "owner", "m_alice_prov")

  const mintKey = async (user: typeof alice, name: string, extra?: Record<string, unknown>) => {
    const res = await mutate(env, "POST", "/api/auth/api-key/create", { name, organizationId: user.organizationId, ...extra }, { cookie: user.cookie, [EXPECTED_ORG_HEADER]: user.organizationId })
    expect(res.status).toBe(200)
    return (await res.json()) as { id: string; key: string }
  }
  const aliceKey = await mintKey(alice, "alice-ci", { metadata: { purpose: "ci" } })
  const bobKey = await mintKey(bob, "bob-ci")
  db.run(
    `insert into "apikey" ("id", "configId", "name", "start", "referenceId", "prefix", "key", "enabled", "expiresAt", "createdAt", "updatedAt")
     values ('ak_expired', 'default', 'old-key', 'old123', ?, null, 'secret_hash_never_returned', 1, '2020-01-01T00:00:00.000Z', '2019-01-01T00:00:00.000Z', '2019-01-01T00:00:00.000Z')`,
    [alice.organizationId],
  )

  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys`)).status).toBe(401)
  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { origin: ORIGIN } })).status).toBe(401)
  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: alice.cookie, origin: "https://evil.example" } })).status).toBe(403)

  const wrongHeaderList = await call(env, `/api/workspaces/${alice.organizationId}/api-keys`, { headers: { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: bob.organizationId } })
  expect(wrongHeaderList.status).toBe(409)
  expect(await wrongHeaderList.json()).toMatchObject({ code: "WORKSPACE_CHANGED" })
  const wrongHeaderDel = await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: bob.organizationId } })
  expect(wrongHeaderDel.status).toBe(409)
  expect(await wrongHeaderDel.json()).toMatchObject({ code: "WORKSPACE_CHANGED" })

  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys`, { headers: { cookie: bob.cookie } })).status).toBe(403)
  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys`, { headers: { cookie: memberUser.cookie } })).status).toBe(403)
  const bobDelReal = await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: bob.cookie, origin: ORIGIN } })
  const bobDelMissing = await call(env, `/api/workspaces/${alice.organizationId}/api-keys/key_missing`, { method: "DELETE", headers: { cookie: bob.cookie, origin: ORIGIN } })
  expect(bobDelReal.status).toBe(403)
  expect(bobDelMissing.status).toBe(403)
  expect(await bobDelReal.json()).toEqual(await bobDelMissing.json())
  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: memberUser.cookie, origin: ORIGIN } })).status).toBe(403)

  expect((await call(env, "/api/workspaces/org_ak_prov/api-keys", { headers: { cookie: alice.cookie } })).status).toBe(409)
  expect((await call(env, "/api/workspaces/org_ak_prov/api-keys/any", { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN } })).status).toBe(409)

  const listRes = await call(env, `/api/workspaces/${alice.organizationId}/api-keys`, { headers: { cookie: alice.cookie, [EXPECTED_ORG_HEADER]: alice.organizationId } })
  expect(listRes.status).toBe(200)
  const listed = (await listRes.json()) as WorkspaceApiKeysDocument
  expect(listed.apiKeys.map((k) => k.id).sort()).toEqual([aliceKey.id, "ak_expired"].sort())
  for (const item of listed.apiKeys) {
    expect("key" in item).toBe(false)
    expect(item.referenceId).toBe(alice.organizationId)
  }
  expect(listed.apiKeys.find((k) => k.id === aliceKey.id)?.metadata).toEqual({ purpose: "ci" })
  expect(countRows(db, "apikey", `"id" = ?`, "ak_expired")).toBe(1)

  const aliceDelBob = await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${bobKey.id}`, { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: alice.organizationId } })
  const aliceDelMissing = await call(env, `/api/workspaces/${alice.organizationId}/api-keys/key_absent`, { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: alice.organizationId } })
  expect(aliceDelBob.status).toBe(404)
  expect(aliceDelMissing.status).toBe(404)
  expect(await aliceDelBob.json()).toEqual(await aliceDelMissing.json())
  expect(countRows(db, "apikey", `"id" = ?`, bobKey.id)).toBe(1)

  const { result: racedDel, raced: demotedOnDelete } = await withSqlRace(
    db,
    `delete from "apikey"`,
    (run) => run(`update "member" set "role" = 'member' where "organizationId" = ? and "userId" = ?`, alice.organizationId, alice.userId),
    () => call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN } }),
  )
  expect(demotedOnDelete).toBe(true)
  expect(racedDel.status).toBe(403)
  expect(countRows(db, "apikey", `"id" = ?`, aliceKey.id)).toBe(1)

  db.run(`update "member" set "role" = 'owner' where "organizationId" = ? and "userId" = ?`, [alice.organizationId, alice.userId])
  const okDel = await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: alice.organizationId } })
  expect(okDel.status).toBe(200)
  expect(await okDel.json()).toMatchObject({ revoked: true, id: aliceKey.id })
  expect(countRows(db, "apikey", `"id" = ?`, aliceKey.id)).toBe(0)
  expect((await call(env, `/api/workspaces/${alice.organizationId}/api-keys/${aliceKey.id}`, { method: "DELETE", headers: { cookie: alice.cookie, origin: ORIGIN } })).status).toBe(404)

  const d1Wrapper = asProvisionDb(db)
  const aliceSession = db.query(`select "token" from "session" where "userId" = ?`).get(alice.userId) as { token: string }
  const d1Listed = await listWorkspaceApiKeys(d1Wrapper, { userId: alice.userId, organizationId: alice.organizationId, sessionToken: aliceSession.token })
  expect(d1Listed.kind).toBe("ok")
  if (d1Listed.kind === "ok") expect(d1Listed.apiKeys.map((k) => k.id)).toEqual(["ak_expired"])
  expect(await commitRevokeApiKey(d1Wrapper, { userId: alice.userId, organizationId: alice.organizationId, keyId: "ak_expired", sessionToken: aliceSession.token })).toEqual({ kind: "revoked", id: "ak_expired" })
  expect(await listWorkspaceApiKeys(d1Wrapper, { userId: alice.userId, organizationId: alice.organizationId, sessionToken: aliceSession.token })).toEqual({ kind: "ok", apiKeys: [] })
  db.run(`update "session" set "expiresAt" = ? where "token" = ?`, [new Date(Date.now() - 10_000).toISOString(), aliceSession.token])
  expect(await listWorkspaceApiKeys(d1Wrapper, { userId: alice.userId, organizationId: alice.organizationId, sessionToken: aliceSession.token })).toMatchObject({ kind: "refused", status: 401 })
  expect(await commitRevokeApiKey(d1Wrapper, { userId: alice.userId, organizationId: alice.organizationId, keyId: "ak_expired", sessionToken: aliceSession.token })).toMatchObject({ kind: "refused", status: 401 })

  const bobSessionRow = db.query(`select "token", "updatedAt", "expiresAt" from "session" where "userId" = ?`).get(bob.userId) as { token: string; updatedAt: string; expiresAt: string }
  const staleUpdatedAt = new Date(Date.now() - 48 * 3_600_000).toISOString()
  const originalExpiry = new Date(Date.now() + 3 * 24 * 3_600_000).toISOString()
  db.run(`update "session" set "updatedAt" = ?, "expiresAt" = ? where "token" = ?`, [staleUpdatedAt, originalExpiry, bobSessionRow.token])
  const noRefreshRes = await call(env, `/api/workspaces/${bob.organizationId}/api-keys`, { headers: { cookie: bob.cookie } })
  expect(noRefreshRes.status).toBe(200)
  expect(noRefreshRes.headers.getSetCookie()).toEqual([])
  expect(db.query(`select "updatedAt", "expiresAt" from "session" where "token" = ?`).get(bobSessionRow.token)).toEqual({ updatedAt: staleUpdatedAt, expiresAt: originalExpiry })

  const memberSession = db.query(`select "token" from "session" where "userId" = ?`).get(memberUser.userId) as { token: string }
  db.run(
    `insert into "apikey" ("id", "configId", "name", "start", "referenceId", "prefix", "key", "enabled", "createdAt", "updatedAt")
     values ('ak_race_visible', 'default', 'live-key', 'liv123', ?, null, 'hash_never_returned', 1, ?, ?)`,
    [alice.organizationId, now, now],
  )
  let promotedBetweenQueries = false
  const racingDb = asProvisionDb(db, {
    onAll(sql) {
      if (!promotedBetweenQueries && sql.includes(`left join "apikey"`)) {
        promotedBetweenQueries = true
        db.run(`update "member" set "role" = 'admin' where "organizationId" = ? and "userId" = ?`, [alice.organizationId, memberUser.userId])
      }
    },
  })
  const racedList = await listWorkspaceApiKeys(racingDb, { userId: memberUser.userId, organizationId: alice.organizationId, sessionToken: memberSession.token })
  expect(promotedBetweenQueries).toBe(true)
  expect(racedList).toMatchObject({ kind: "refused", status: 403, code: "FORBIDDEN" })
})
