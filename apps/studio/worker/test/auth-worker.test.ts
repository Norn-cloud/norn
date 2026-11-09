import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import worker, { type Env } from "../index.ts"

const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATION: string = readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8")

function stage(): Env {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  return { ASSETS: assets, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const post = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin: ORIGIN, ...headers } })
/** Sign up (no session is issued), then sign in with the password; the sign-in response carries the cookie. */
const enroll = async (env: Env, email: string, name: string, headers: HeadersInit = {}) => {
  const signUp = await post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name }, headers)
  if (signUp.status !== 200) return signUp
  return post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" }, headers)
}
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

test("signing in after sign-up sets an HTTP-only, secure, same-site session cookie and an owned organization", async () => {
  const env = stage()
  const signUp = await enroll(env, "ada@example.com", "Ada")
  expect(signUp.status).toBe(200)
  const session = signUp.headers.getSetCookie().find((line) => line.startsWith("__Secure-norn.session_token="))
  expect(session).toBeDefined()
  expect(session).toMatch(/HttpOnly/i)
  expect(session).toMatch(/Secure/i)
  expect(session).toMatch(/SameSite=Lax/i)
  const me = await call(env, "/api/me", { headers: { cookie: cookieOf(signUp) } })
  expect(me.status).toBe(200)
  const body = (await me.json()) as { user: { email: string }; organization: { role: string; name: string }; environmentId: string | null }
  expect(body.user.email).toBe("ada@example.com")
  expect(body.organization).toMatchObject({ role: "owner", name: "Ada's workspace" })
  expect(body.environmentId).toBeNull()
  const row = (env.AUTH_DB as unknown as Database).query("select authMethod from session").get() as { authMethod: string }
  expect(row.authMethod).toBe("password")
})

test("the active organization selects its metadata environment", async () => {
  const env = stage()
  const cookie = cookieOf(await enroll(env, "scope@example.com", "Scope"))
  const db = env.AUTH_DB as unknown as Database
  const user = (db.query("select id from user where email = 'scope@example.com'").get() as { id: string }).id
  db.run("insert into organization (id, name, slug, createdAt) values ('org_demo', 'Demo', 'demo', ?)", [Date.now()])
  db.run("insert into member (id, organizationId, userId, role, createdAt) values ('scope-demo', 'org_demo', ?, 'viewer', ?)", [user, Date.now()])
  db.run("update session set activeOrganizationId = 'org_demo' where userId = ?", [user])
  const body = await (await call(env, "/api/me", { headers: { cookie } })).json() as { organization: { id: string }; environmentId: string | null }
  expect(body.organization.id).toBe("org_demo")
  expect(body.environmentId).toBeNull()
  db.run("update organization set metadata = ? where id = ?", [JSON.stringify({ environmentId: "warehouse" }), "org_demo"])
  const changed = await (await call(env, "/api/me", { headers: { cookie } })).json() as { environmentId: string | null }
  expect(changed.environmentId).toBe("warehouse")
})

test("sign-out ends the session", async () => {
  const env = stage()
  const signUp = await enroll(env, "bo@example.com", "Bo")
  const cookie = cookieOf(signUp)
  expect((await post(env, "/api/auth/sign-out", {}, { cookie })).status).toBe(200)
  expect((await call(env, "/api/me", { headers: { cookie } })).status).toBe(401)
})

test("a state-changing auth request from another origin is refused", async () => {
  const env = stage()
  const response = await post(env, "/api/auth/sign-up/email", { email: "eve@example.com", password: "correct horse battery", name: "Eve" }, { origin: "https://evil.example" })
  expect(response.status).toBe(403)
})

test("a listed second origin may sign in while an unlisted origin is refused", async () => {
  const env = { ...stage(), BETTER_AUTH_TRUSTED_ORIGINS: JSON.stringify([ORIGIN, "https://studio-demo.norn.cloud"]) }
  await post(env, "/api/auth/sign-up/email", { email: "second@example.com", password: "correct horse battery", name: "Second" })
  const credentials = { email: "second@example.com", password: "correct horse battery" }
  expect((await post(env, "/api/auth/sign-in/email", credentials, { origin: "https://studio-demo.norn.cloud" })).status).toBe(200)
  expect((await post(env, "/api/auth/sign-in/email", credentials, { origin: "https://evil.example" })).status).toBe(403)
})

test("magic links stay on the trusted origin that requested them", async () => {
  const second = "https://studio-demo.norn.cloud"
  const sent: string[] = []
  const env = { ...stage(), BETTER_AUTH_TRUSTED_ORIGINS: JSON.stringify([ORIGIN, second]), EMAIL_FROM: "studio@norn.cloud", EMAIL: { send: async (message: { text: string }) => { sent.push(message.text) } } }
  await post(env, "/api/auth/sign-up/email", { email: "magic@example.com", password: "correct horse battery", name: "Magic" })
  const magic = (origin: string, extra: Record<string, string> = {}, callbackURL = "/") => worker.fetch(new Request(`${origin}/api/auth/sign-in/magic-link`, { method: "POST", headers: { "content-type": "application/json", origin, "cf-connecting-ip": `198.51.100.${++address % 250}`, ...extra }, body: JSON.stringify({ email: "magic@example.com", callbackURL }) }) as Parameters<typeof worker.fetch>[0], env)
  expect((await magic("https://evil.example")).status).toBe(403)
  expect((await magic(second, {}, "https://evil.example/landing")).status).toBe(403)
  expect((await magic(second, { host: "evil.example" })).status).toBe(403)
  expect((await magic(second, { "x-forwarded-host": "evil.example" })).status).toBe(403)
  expect(sent).toHaveLength(0)
  for (const [source, other] of [[second, ORIGIN], [ORIGIN, second]] as const) {
    expect((await magic(source)).status).toBe(200)
    const url = sent.at(-1)?.match(/https:\/\/[^\s]+/)?.[0]
    expect(url).toBeDefined()
    const link = new URL(url ?? "")
    expect(link.origin).toBe(source)
    expect(new URL(link.searchParams.get("callbackURL") ?? "").origin).toBe(source)
    const moved = new URL(link); moved.host = new URL(other).host
    expect((await worker.fetch(new Request(moved, { redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env)).status).toBe(403)
    for (const header of ["host", "x-forwarded-host"]) {
      expect((await worker.fetch(new Request(link, { headers: { [header]: "evil.example" }, redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env)).status).toBe(403)
    }
    const tampered = new URL(link); tampered.searchParams.set("callbackURL", `${other}/`)
    expect((await worker.fetch(new Request(tampered, { redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env)).status).toBe(403)
    const relative = new URL(link); relative.searchParams.set("callbackURL", "/")
    expect((await worker.fetch(new Request(relative, { redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env)).status).toBe(403)
    const verified = await worker.fetch(new Request(link, { redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env)
    expect(verified.status).toBe(302)
    expect(new URL(verified.headers.get("location") ?? "", source).origin).toBe(source)
    const cookie = cookieOf(verified)
    expect(cookie).toContain("__Secure-norn.session_token=")
    const me = await worker.fetch(new Request(`${source}/api/me`, { headers: { cookie } }) as Parameters<typeof worker.fetch>[0], env)
    expect(me.status).toBe(200)
    const replay = await worker.fetch(new Request(link, { redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env)
    expect(replay.headers.getSetCookie().some((line) => line.startsWith("__Secure-norn.session_token="))).toBe(false)
    expect(replay.headers.get("location")).toContain("INVALID_TOKEN")
  }
})

test("repeated password guesses from one address are rate limited", async () => {
  const env = stage()
  await post(env, "/api/auth/sign-up/email", { email: "cy@example.com", password: "correct horse battery", name: "Cy" })
  const statuses: number[] = []
  for (let attempt = 0; attempt < 12; attempt++) {
    statuses.push((await post(env, "/api/auth/sign-in/email", { email: "cy@example.com", password: "wrong password!" }, { "cf-connecting-ip": "203.0.113.7" })).status)
  }
  expect(statuses.slice(0, 10).every((status) => status === 401)).toBe(true)
  expect(statuses.at(-1)).toBe(429)
})

test("magic links refuse when the deployment has no mail sender", async () => {
  const env = stage()
  const response = await post(env, "/api/auth/sign-in/magic-link", { email: "di@example.com" })
  expect(response.status).toBe(503)
})

test("a client-supplied actor header is refused and other APIs need a session", async () => {
  const env = stage()
  expect((await call(env, "/api/me", { headers: { "x-norn-actor": "admin" } })).status).toBe(400)
  expect((await call(env, "/api/anything")).status).toBe(401)
  expect(await (await call(env, "/")).text()).toContain("studio")
})

test("sign-in always lands in the earliest of several organizations", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  const signUp = await enroll(env, "fa@example.com", "Fa")
  const own = (db.query("select organizationId from member").get() as { organizationId: string }).organizationId
  const user = (db.query("select id from user").get() as { id: string }).id
  db.run("insert into organization (id, name, slug, createdAt) values ('later', 'Later', 'later', ?)", [new Date(Date.now() + 60_000).toISOString()])
  db.run("insert into member (id, organizationId, userId, role, createdAt) values ('m2', 'later', ?, 'viewer', ?)", [user, new Date(Date.now() + 60_000).toISOString()])
  await post(env, "/api/auth/sign-out", {}, { cookie: cookieOf(signUp) })
  for (let attempt = 0; attempt < 3; attempt++) {
    const signIn = await post(env, "/api/auth/sign-in/email", { email: "fa@example.com", password: "correct horse battery" })
    const me = (await (await call(env, "/api/me", { headers: { cookie: cookieOf(signIn) } })).json()) as { organization: { id: string; role: string } }
    expect(me.organization).toMatchObject({ id: own, role: "owner" })
  }
})

test("an unverified account cannot accept an invitation sent to its address", async () => {
  const env = stage()
  const owner = cookieOf(await enroll(env, "ow@example.com", "Ow"))
  const org = ((await (await call(env, "/api/me", { headers: { cookie: owner } })).json()) as { organization: { id: string } }).organization.id
  const invite = await post(env, "/api/auth/organization/invite-member", { email: "claim@example.com", role: "admin", organizationId: org }, { cookie: owner })
  expect(invite.status).toBe(200)
  const { id } = (await invite.json()) as { id: string }
  const claimant = cookieOf(await enroll(env, "claim@example.com", "Claim"))
  const accept = await post(env, "/api/auth/organization/accept-invitation", { invitationId: id }, { cookie: claimant })
  expect(accept.status).toBe(403)
  const db = env.AUTH_DB as unknown as Database
  expect(db.query("select count(*) as n from member where organizationId = ?").get(org)).toEqual({ n: 1 })
})

test("sign-up answers a registered address byte for byte like a new one, apart from a placeholder id", async () => {
  const env = stage()
  await post(env, "/api/auth/sign-up/email", { email: "gi@example.com", password: "correct horse battery", name: "Gi" })
  const answer = async (email: string) => {
    const response = await post(env, "/api/auth/sign-up/email", { email, password: "another horse battery", name: "Gi" })
    return { status: response.status, cookies: response.headers.getSetCookie(), text: await response.text() }
  }
  const existing = await answer("gi@example.com")
  const fresh = await answer("ha@example.com")
  const masked = (text: string, email: string) => text.replace(/"id":"[0-9a-f-]{36}"/, '"id":"ID"').replace(email, "EMAIL")
  expect(masked(existing.text, "gi@example.com")).toBe(masked(fresh.text, "ha@example.com"))
  expect(masked(fresh.text, "ha@example.com")).toBe('{"token":null,"user":{"id":"ID","email":"EMAIL","name":"Gi","emailVerified":false}}')
  expect([existing.status, fresh.status, existing.cookies, fresh.cookies]).toEqual([200, 200, [], []])
  // The registered path's matching writes leave nothing behind.
  const db = env.AUTH_DB as unknown as Database
  expect(db.query("select count(*) as n from verification where identifier like 'signup-pad:%'").get()).toEqual({ n: 0 })
  // The registered account keeps its original password, and the new one can sign in.
  expect((await post(env, "/api/auth/sign-in/email", { email: "gi@example.com", password: "another horse battery" })).status).toBe(401)
  expect((await post(env, "/api/auth/sign-in/email", { email: "gi@example.com", password: "correct horse battery" })).status).toBe(200)
  expect((await post(env, "/api/auth/sign-in/email", { email: "ha@example.com", password: "another horse battery" })).status).toBe(200)
})


test("a failed membership insert is retried safely on the next sign-in", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  expect((await enroll(env, "recover@example.com", "Recover")).status).toBe(200)
  db.run(`delete from "member"`)
  db.run(`delete from "organization"`)
  db.run(`update "workspace_provisioning" set "status" = 'pending'`)
  db.exec(`create trigger fail_member before insert on "member" begin select raise(abort, 'injected membership failure'); end`)
  const failed = await post(env, "/api/auth/sign-in/email", { email: "recover@example.com", password: "correct horse battery" })
  expect(failed.status).not.toBe(200)
  expect(db.query(`select count(*) as n from "organization"`).get()).toEqual({ n: 0 })
  expect(db.query(`select count(*) as n from "member"`).get()).toEqual({ n: 0 })
  db.exec(`drop trigger fail_member`)
  const recovered = await post(env, "/api/auth/sign-in/email", { email: "recover@example.com", password: "correct horse battery" })
  expect(recovered.status).toBe(200)
  const me = await (await call(env, "/api/me", { headers: { cookie: cookieOf(recovered) } })).json() as { organization: { role: string; name: string } }
  expect(me.organization).toMatchObject({ role: "owner", name: "Recover's workspace" })
  expect(db.query(`select count(*) as n from "organization"`).get()).toEqual({ n: 1 })
})

test("recovery refuses a workspace whose server ownership proof is altered", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  expect((await enroll(env, "proof@example.com", "Proof")).status).toBe(200)
  db.run(`delete from "member"`)
  db.run(`update "workspace_provisioning" set "status" = 'pending'`)
  db.run(`update "organization" set "metadata" = ?`, ['{"norn.provisioningProof":"forged"}'])
  const response = await post(env, "/api/auth/sign-in/email", { email: "proof@example.com", password: "correct horse battery" })
  expect(response.status).not.toBe(200)
  expect(db.query(`select count(*) as n from "member"`).get()).toEqual({ n: 0 })
})


test("client organization metadata cannot assign or replace reserved cell bindings", async () => {
  const env = { ...stage(), STUDIO_DEFAULT_ENVIRONMENT_ID: "server-cell" }
  const db = env.AUTH_DB as unknown as Database
  const signedIn = await enroll(env, "scope@example.com", "Scope")
  const cookie = cookieOf(signedIn)
  const own = db.query(`select "organizationId" from "member"`).get() as { organizationId: string }
  const stored = () => JSON.parse((db.query(`select "metadata" from "organization" where "id" = ?`).get(own.organizationId) as { metadata: string }).metadata) as Record<string, unknown>
  expect(stored().environmentId).toBe("server-cell")
  const created = await post(env, "/api/auth/organization/create", { name: "Other", slug: "other-workspace", metadata: { environmentId: "analytics", "norn.test": "forged", label: "kept" } }, { cookie })
  expect(created.status).toBe(200)
  const row = db.query(`select "metadata" from "organization" where "slug" = 'other-workspace'`).get() as { metadata: string }
  expect(JSON.parse(row.metadata)).toEqual({ label: "kept" })
  const updated = await post(env, "/api/auth/organization/update", { organizationId: own.organizationId, data: { metadata: { environmentId: "analytics", "norn.provisioningProof": "forged", "norn.test": "forged", label: "changed" } } }, { cookie })
  expect(updated.status).toBe(200)
  expect(stored().environmentId).toBe("server-cell")
  expect(stored()["norn.provisioningProof"]).not.toBe("forged")
  expect(stored()["norn.test"]).toBeUndefined()
  expect(stored().label).toBe("changed")
})


test("removing a completed workspace owner does not restore membership on sign-in", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  expect((await enroll(env, "removed@example.com", "Removed")).status).toBe(200)
  db.run(`delete from "member"`)
  const response = await post(env, "/api/auth/sign-in/email", { email: "removed@example.com", password: "correct horse battery" })
  expect(response.status).toBe(200)
  expect(db.query(`select count(*) as n from "member"`).get()).toEqual({ n: 0 })
  expect(db.query(`select "status" from "workspace_provisioning"`).get()).toEqual({ status: "complete" })
})

test("concurrent recovery creates one owner membership", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  expect((await enroll(env, "race@example.com", "Race")).status).toBe(200)
  db.run(`delete from "member"`)
  db.run(`update "workspace_provisioning" set "status" = 'pending'`)
  const signIn = () => post(env, "/api/auth/sign-in/email", { email: "race@example.com", password: "correct horse battery" })
  const responses = await Promise.all([signIn(), signIn()])
  expect(responses.map((response) => response.status)).toEqual([200, 200])
  expect(db.query(`select count(*) as n from "member"`).get()).toEqual({ n: 1 })
  expect(db.query(`select "status" from "workspace_provisioning"`).get()).toEqual({ status: "complete" })
})


test("workspace migration deduplicates by role, creation time, then id", () => {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  db.exec(`insert into "user" ("id", "name", "email", "emailVerified", "createdAt", "updatedAt") values ('u', 'User', 'u@example.com', 1, '2020', '2020');
    insert into "organization" ("id", "name", "slug", "createdAt") values ('o', 'Org', 'org', '2020');
    insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values
      ('viewer', 'o', 'u', 'viewer', '2010'), ('new-owner', 'o', 'u', 'owner', '2022'),
      ('b-owner', 'o', 'u', 'owner', '2020'), ('a-owner', 'o', 'u', 'owner', '2020')`)
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  expect(db.query(`select "id", "role" from "member"`).all()).toEqual([{ id: "a-owner", role: "owner" }])
  expect(() => db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('duplicate', 'o', 'u', 'member', '2025')`)).toThrow()
})


test("failure to initialize provisioning rolls back user creation and a retry succeeds", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  db.exec(`create trigger fail_provisioning_marker before insert on "workspace_provisioning" begin select raise(abort, 'injected marker failure'); end`)
  const body = { email: "marker@example.com", password: "correct horse battery", name: "Marker" }
  const failed = await post(env, "/api/auth/sign-up/email", body)
  expect(failed.status).not.toBe(200)
  expect(db.query(`select count(*) as n from "user" where "email" = ?`).get(body.email)).toEqual({ n: 0 })
  expect(db.query(`select count(*) as n from "workspace_provisioning"`).get()).toEqual({ n: 0 })
  db.exec(`drop trigger fail_provisioning_marker`)
  expect((await post(env, "/api/auth/sign-up/email", body)).status).toBe(200)
  const signedIn = await post(env, "/api/auth/sign-in/email", { email: body.email, password: body.password })
  expect(signedIn.status).toBe(200)
  const me = await (await call(env, "/api/me", { headers: { cookie: cookieOf(signedIn) } })).json() as { organization: { role: string } }
  expect(me.organization.role).toBe("owner")
})

const meOf = async (env: Env, cookie: string, extra: HeadersInit = {}, query = "") =>
  await (await call(env, `/api/me${query}`, { headers: { cookie, ...extra } })).json() as { organization: { id: string } | null; environmentId: string | null }

test("/api/me gives the active workspace's provisioned environment, and none for a workspace without one", async () => {
  const env = { ...stage(), STUDIO_DEFAULT_ENVIRONMENT_ID: "server-cell" }
  const db = env.AUTH_DB as unknown as Database
  const cookie = cookieOf(await enroll(env, "cell@example.com", "Cell"))
  const own = (db.query(`select "organizationId" from "member"`).get() as { organizationId: string }).organizationId
  expect(await meOf(env, cookie)).toMatchObject({ organization: { id: own }, environmentId: "server-cell" })
  // A workspace provisioned without a default environment, or with malformed metadata, has none.
  db.run(`update "organization" set "metadata" = ? where "id" = ?`, [JSON.stringify({ label: "x" }), own])
  expect((await meOf(env, cookie)).environmentId).toBeNull()
  db.run(`update "organization" set "metadata" = ? where "id" = ?`, ["not-json", own])
  expect((await meOf(env, cookie)).environmentId).toBeNull()
  db.run(`update "organization" set "metadata" = ? where "id" = ?`, [JSON.stringify({ environmentId: " " }), own])
  expect((await meOf(env, cookie)).environmentId).toBeNull()
})

test("/api/me never takes the environment from the client, and gives none for a workspace the caller is not a member of", async () => {
  const env = { ...stage(), STUDIO_DEFAULT_ENVIRONMENT_ID: "server-cell" }
  const db = env.AUTH_DB as unknown as Database
  const cookie = cookieOf(await enroll(env, "claims@example.com", "Claims"))
  const user = (db.query(`select "id" from "user" where "email" = 'claims@example.com'`).get() as { id: string }).id
  // Whatever the request claims, the answer is the server's.
  expect((await meOf(env, cookie, { "x-norn-environment": "warehouse" }, "?environmentId=warehouse&orgId=org_other")).environmentId).toBe("server-cell")
  // A client's own metadata write cannot set it (reserved keys are stripped), so /api/me still reads the provisioned one.
  const own = (db.query(`select "organizationId" from "member"`).get() as { organizationId: string }).organizationId
  expect((await post(env, "/api/auth/organization/update", { organizationId: own, data: { metadata: { environmentId: "warehouse" } } }, { cookie })).status).toBe(200)
  expect((await meOf(env, cookie)).environmentId).toBe("server-cell")
  // An active workspace the caller does not belong to (a stale or forged session value): no workspace, no environment.
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_other', 'Other', 'other', ?, ?)`, [JSON.stringify({ environmentId: "warehouse" }), Date.now()])
  db.run(`update "session" set "activeOrganizationId" = 'org_other' where "userId" = ?`, [user])
  expect(await meOf(env, cookie)).toMatchObject({ organization: null, environmentId: null })
})
